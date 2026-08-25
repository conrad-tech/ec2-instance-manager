// Build-time validation of `assets/forwards.json`.
//
// NOTE: regular `//` comments, not `//!` inner docs — this file is also pulled
// into build.rs with `include!`, where inner-doc attributes are a syntax error.
//
// This file is shared verbatim by two compilations so the two never drift:
//   * `build.rs` pulls it in with `include!` and fails the build on anything
//     this reports.
//   * The library compiles it as the `forwards_check` module, which is where
//     its tests live — a build script's own `#[cfg(test)]` code is never run.
//
// WHY A BUILD CHECK. `ForwardsConfig::parse` is deliberately fail-soft: a
// malformed file yields NO forwards rather than an error, because forwards are
// a convenience and a bad config must never block a VS Code launch. The cost
// of that stance is that every mistake in this file is silent — a stray comma,
// a `"port": "8443"` written as a string, or an `environments` block nobody
// filled in all produce the same thing at runtime: a Port Forwards window with
// nothing in it and not one word saying why. So the file is checked where a
// mistake can still be shouted about, which is here.
//
// Only serde_json and std, since build.rs has no access to the crate's own
// modules.

/// Setting this in the environment allows a build with **no** forwards
/// declared. Shape errors are still fatal — this covers the developer who has
/// no endpoints to declare, not a file that is wrong.
pub const ALLOW_NO_FORWARDS_ENV: &str = "ALLOW_NO_FORWARDS";

/// The keys `ForwardEntry` actually reads. Anything else is a typo that serde
/// would drop in silence (`"Port": 8443` being the expensive one).
const ENTRY_KEYS: [&str; 3] = ["ip", "host", "port"];
/// The keys `PortRule` reads.
const RULE_KEYS: [&str; 2] = ["match", "port"];
/// The keys `ForwardsConfig` reads.
const TOP_KEYS: [&str; 3] = ["default_port", "port_rules", "environments"];

/// Keys beginning with `_` are documentation — `_comment` and
/// `_example_environments` are both load-bearing prose in the shipped file.
fn is_doc_key(key: &str) -> bool {
    key.starts_with('_')
}

/// Report every problem in the text of a `forwards.json`, most structural
/// first. An empty result means the file is fine.
///
/// `require_forwards` decides whether "declares nothing at all" counts as a
/// problem; everything else is a problem either way.
pub fn check_forwards_json(raw: &str, require_forwards: bool) -> Vec<String> {
    let value: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        // No point reporting anything else: serde read none of it either.
        Err(err) => return vec![format!("not valid JSON: {err}")],
    };

    let obj = match value.as_object() {
        Some(o) => o,
        None => return vec!["must be a JSON object".to_string()],
    };

    let mut problems: Vec<String> = Vec::new();

    for key in obj.keys() {
        if !is_doc_key(key) && !TOP_KEYS.contains(&key.as_str()) {
            problems.push(format!(
                "unknown top-level key `{key}` — expected one of {}, or a `_`-prefixed comment",
                TOP_KEYS.join(", ")
            ));
        }
    }

    if let Some(port) = obj.get("default_port") {
        check_port(port, "default_port", &mut problems);
    }

    match obj.get("port_rules") {
        None => {}
        Some(serde_json::Value::Array(rules)) => {
            for (idx, rule) in rules.iter().enumerate() {
                let at = format!("port_rules[{idx}]");
                let rule = match rule.as_object() {
                    Some(o) => o,
                    None => {
                        problems.push(format!("{at} must be an object"));
                        continue;
                    }
                };
                for key in rule.keys() {
                    if !is_doc_key(key) && !RULE_KEYS.contains(&key.as_str()) {
                        problems.push(format!(
                            "{at} has unknown key `{key}` — expected {}",
                            RULE_KEYS.join(", ")
                        ));
                    }
                }
                match rule.get("match").and_then(|v| v.as_str()) {
                    Some(m) if !m.trim().is_empty() => {}
                    _ => problems.push(format!(
                        "{at} needs a non-empty `match` string (the substring of a DNS name)"
                    )),
                }
                match rule.get("port") {
                    Some(port) => check_port(port, &format!("{at}.port"), &mut problems),
                    None => problems.push(format!("{at} needs a `port` number")),
                }
            }
        }
        Some(_) => problems.push("`port_rules` must be an array".to_string()),
    }

    let environments = match obj.get("environments") {
        Some(serde_json::Value::Object(map)) => map,
        Some(_) => {
            problems.push(
                "`environments` must be an object keyed by MMODAL_ENV value".to_string(),
            );
            return problems;
        }
        None => {
            problems.push(
                "no `environments` object — no port forwards are declared".to_string(),
            );
            return problems;
        }
    };

    if environments.is_empty() {
        if require_forwards {
            problems.push(
                "`environments` is empty — no port forwards are declared".to_string(),
            );
        }
        return problems;
    }

    for (env, entries) in environments {
        if env.trim().is_empty() {
            problems.push(
                "an environment is keyed by a blank name; the key is the MMODAL_ENV tag"
                    .to_string(),
            );
            continue;
        }
        let entries = match entries.as_array() {
            Some(a) => a,
            None => {
                problems.push(format!("environment `{env}` must be an array of forwards"));
                continue;
            }
        };
        if entries.is_empty() {
            problems.push(format!(
                "environment `{env}` declares no forwards — remove it, or give it an entry"
            ));
            continue;
        }
        for (idx, entry) in entries.iter().enumerate() {
            check_entry(env, idx, entry, &mut problems);
        }
    }

    problems
}

/// One `{ ip, host, port? }`.
fn check_entry(env: &str, idx: usize, entry: &serde_json::Value, problems: &mut Vec<String>) {
    let at = format!("environment `{env}` entry [{idx}]");
    let entry = match entry.as_object() {
        Some(o) => o,
        None => {
            problems.push(format!("{at} must be an object"));
            return;
        }
    };

    for key in entry.keys() {
        if !is_doc_key(key) && !ENTRY_KEYS.contains(&key.as_str()) {
            problems.push(format!(
                "{at} has unknown key `{key}` — expected {}",
                ENTRY_KEYS.join(", ")
            ));
        }
    }

    // The bind address is parsed rather than merely required: `127.200.20`
    // reads as an address and is not one, and ssh's refusal to bind it kills
    // the whole tunnel under ExitOnForwardFailure=yes.
    match entry.get("ip").and_then(|v| v.as_str()) {
        Some(ip) if !ip.trim().is_empty() => {
            if ip.trim().parse::<std::net::IpAddr>().is_err() {
                problems.push(format!("{at} has `ip` \"{ip}\", which is not an IP address"));
            }
        }
        _ => problems.push(format!(
            "{at} needs a non-empty `ip` string (the loopback address bound on this machine)"
        )),
    }

    match entry.get("host").and_then(|v| v.as_str()) {
        Some(host) if !host.trim().is_empty() => {
            let host = host.trim();
            if host.chars().any(char::is_whitespace) {
                problems.push(format!("{at} has `host` \"{host}\", which contains whitespace"));
            }
            // A port belongs in `port`; written here it becomes part of the
            // name ssh asks the bastion to resolve, and nothing resolves it.
            if host.contains(':') {
                problems.push(format!(
                    "{at} has `host` \"{host}\" — a port goes in `port`, not on the name"
                ));
            }
        }
        _ => problems.push(format!(
            "{at} needs a non-empty `host` string (the remote DNS name, resolved on the bastion)"
        )),
    }

    if let Some(port) = entry.get("port") {
        check_port(port, &format!("{at} `port`"), problems);
    }
}

/// A port must be a JSON *number* in 1..=65535. `"8443"` in quotes fails the
/// whole file's parse at runtime, taking every other environment with it.
fn check_port(value: &serde_json::Value, at: &str, problems: &mut Vec<String>) {
    match value.as_u64() {
        Some(port) if (1..=65535).contains(&port) => {}
        Some(port) => problems.push(format!("{at} is {port}, outside 1-65535")),
        None => problems.push(format!(
            "{at} must be a number in 1-65535, not {value}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_env() -> String {
        r#"{
            "default_port": 443,
            "port_rules": [{ "match": "postgres", "port": 5432 }],
            "environments": {
                "DEV1": [
                    { "ip": "127.200.10.1", "host": "uweb01.dev1.example.net" },
                    { "ip": "127.200.10.2", "host": "admin01.dev1.example.net", "port": 8443 }
                ]
            }
        }"#
        .to_string()
    }

    fn problems(raw: &str) -> Vec<String> {
        check_forwards_json(raw, true)
    }

    #[test]
    fn a_filled_in_file_passes() {
        assert!(problems(&one_env()).is_empty(), "{:#?}", problems(&one_env()));
    }

    /// The case this whole check exists for: the shipped file, untouched.
    #[test]
    fn an_empty_environments_block_is_a_failure() {
        let raw = r#"{ "default_port": 443, "port_rules": [], "environments": {} }"#;
        let found = problems(raw);
        assert_eq!(found.len(), 1, "{found:#?}");
        assert!(found[0].contains("no port forwards are declared"), "{found:#?}");
    }

    /// ...and the escape hatch for a developer with no endpoints to declare
    /// suppresses exactly that one problem.
    #[test]
    fn nothing_declared_is_allowed_when_forwards_are_not_required() {
        let raw = r#"{ "default_port": 443, "port_rules": [], "environments": {} }"#;
        assert!(check_forwards_json(raw, false).is_empty());
        // But it does not excuse a file that is wrong.
        let broken = r#"{ "environments": { "DEV1": [{ "host": "a.net" }] } }"#;
        assert!(!check_forwards_json(broken, false).is_empty());
    }

    #[test]
    fn a_missing_environments_key_is_a_failure() {
        let found = problems(r#"{ "default_port": 443 }"#);
        assert!(
            found.iter().any(|p| p.contains("no `environments` object")),
            "{found:#?}"
        );
    }

    #[test]
    fn a_stray_comma_is_named_rather_than_silently_disabling_everything() {
        let found = problems(r#"{ "environments": { "DEV1": [], } }"#);
        assert_eq!(found.len(), 1, "{found:#?}");
        assert!(found[0].starts_with("not valid JSON:"), "{found:#?}");
    }

    #[test]
    fn an_environment_with_no_entries_is_a_failure() {
        let found = problems(r#"{ "environments": { "DEV1": [] } }"#);
        assert!(
            found.iter().any(|p| p.contains("declares no forwards")),
            "{found:#?}"
        );
    }

    #[test]
    fn an_entry_needs_both_an_ip_and_a_host() {
        let found = problems(r#"{ "environments": { "DEV1": [{ "ip": "  " }] } }"#);
        assert!(found.iter().any(|p| p.contains("non-empty `ip`")), "{found:#?}");
        assert!(found.iter().any(|p| p.contains("non-empty `host`")), "{found:#?}");
    }

    #[test]
    fn a_bind_address_that_is_not_an_address_is_a_failure() {
        let found = problems(
            r#"{ "environments": { "DEV1": [{ "ip": "127.200.20", "host": "a.example.net" }] } }"#,
        );
        assert!(found.iter().any(|p| p.contains("not an IP address")), "{found:#?}");
    }

    /// A port written onto the name is resolved on the bastion as part of the
    /// name, so nothing answers and the local bind still succeeds.
    #[test]
    fn a_port_on_the_host_name_is_a_failure() {
        let found = problems(
            r#"{ "environments": { "DEV1": [{ "ip": "127.0.0.1", "host": "a.net:8443" }] } }"#,
        );
        assert!(found.iter().any(|p| p.contains("a port goes in `port`")), "{found:#?}");
    }

    /// serde fails the *whole file* on this, so it takes every environment
    /// down, not just the entry carrying it.
    #[test]
    fn a_quoted_port_is_a_failure() {
        let found = problems(
            r#"{ "environments": { "DEV1": [{ "ip": "127.0.0.1", "host": "a.net", "port": "8443" }] } }"#,
        );
        assert!(found.iter().any(|p| p.contains("must be a number")), "{found:#?}");

        let out_of_range = problems(
            r#"{ "environments": { "DEV1": [{ "ip": "127.0.0.1", "host": "a.net", "port": 70000 }] } }"#,
        );
        assert!(
            out_of_range.iter().any(|p| p.contains("outside 1-65535")),
            "{out_of_range:#?}"
        );
    }

    /// serde drops an unrecognised key without a word, so `"Port": 8443`
    /// reads as an entry with no port at all and quietly takes default_port.
    #[test]
    fn a_misspelled_key_is_named() {
        let found = problems(
            r#"{ "environments": { "DEV1": [{ "ip": "127.0.0.1", "host": "a.net", "Port": 8443 }] } }"#,
        );
        assert!(found.iter().any(|p| p.contains("unknown key `Port`")), "{found:#?}");
    }

    /// `_comment` and `_example_environments` are prose the shipped file
    /// depends on; they must not read as typos.
    #[test]
    fn underscore_keys_are_documentation_everywhere() {
        let raw = r#"{
            "_comment": ["anything"],
            "_example_environments": { "AUCT": [] },
            "environments": {
                "DEV1": [{ "_why": "x", "ip": "127.0.0.1", "host": "a.net" }]
            }
        }"#;
        assert!(problems(raw).is_empty(), "{:#?}", problems(raw));
    }

    #[test]
    fn a_broken_port_rule_is_named() {
        let found = problems(
            r#"{
                "port_rules": [{ "match": "", "port": 5432 }, { "match": "solr" }],
                "environments": { "DEV1": [{ "ip": "127.0.0.1", "host": "a.net" }] }
            }"#,
        );
        assert!(found.iter().any(|p| p.contains("non-empty `match`")), "{found:#?}");
        assert!(found.iter().any(|p| p.contains("needs a `port` number")), "{found:#?}");
    }

    #[test]
    fn a_top_level_typo_is_named() {
        let found = problems(
            r#"{ "portrules": [], "environments": { "DEV1": [{ "ip": "127.0.0.1", "host": "a.net" }] } }"#,
        );
        assert!(
            found.iter().any(|p| p.contains("unknown top-level key `portrules`")),
            "{found:#?}"
        );
    }
}

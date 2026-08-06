//! Per-environment SSH port forwards for the Open in VS Code integration.
//!
//! Reaching an internal service from a workstation means tunnelling it
//! through a bastion. This module turns the compiled-in `assets/forwards.json`
//! plus the machine's hosts file into the `LocalForward` lines that go into
//! the managed Host block.
//!
//! **The hosts file is read, never written.** Writing
//! `C:\Windows\System32\drivers\etc\hosts` needs elevation that most
//! corporate users do not have, and programmatic writes to that path are a
//! behaviour EDR products flag — a real cost for this app, which has a
//! CrowdStrike quarantine in its history. That is affordable because a
//! forward does not depend on the hosts file: in
//! `LocalForward 127.200.20.1:443 host.net:443` the remote name is resolved
//! on the bastion. A hosts entry only lets the user type the name in a
//! browser instead of the loopback IP.
//!
//! So where the user's hosts file already resolves a name, the forward binds
//! *that* IP — the generated config conforms to the machine rather than
//! asking the machine to change.

use std::collections::BTreeMap;

use serde::Deserialize;

/// Compiled-in forward definitions from `assets/forwards.json`, obfuscated at
/// build time like the other bundled assets (see [`crate::obf_core`]).
const BUNDLED_FORWARDS_OBF: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/forwards.json.obf"));

fn bundled_forwards() -> String {
    let plain = crate::obf_core::obf_transform(BUNDLED_FORWARDS_OBF);
    String::from_utf8(plain).expect("bundled forwards.json is valid UTF-8")
}

/// Port inferred from a substring of the DNS name, e.g. `postgres` → 5432.
#[derive(Clone, Debug, Deserialize)]
pub struct PortRule {
    /// Case-insensitive substring matched against the DNS name.
    /// Spelled `match_` because `match` is a keyword; renamed on the wire.
    #[serde(rename = "match", default)]
    pub match_: String,
    /// Port to use on both ends of the forward.
    #[serde(default)]
    pub port: u16,
}

/// One forward declared in `forwards.json`.
#[derive(Clone, Debug, Deserialize)]
pub struct ForwardEntry {
    /// Loopback address to bind locally. Used only when the hosts file has
    /// nothing to say about `host`.
    #[serde(default)]
    pub ip: String,
    /// Remote DNS name, resolved on the bastion.
    #[serde(default)]
    pub host: String,
    /// Explicit port, overriding the rules. `None` means infer.
    #[serde(default)]
    pub port: Option<u16>,
}

/// Parsed `forwards.json`.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct ForwardsConfig {
    /// Port for a name matching no rule.
    pub default_port: u16,
    /// Checked in order — **first match wins**, so the list order is the
    /// tiebreak for a name like `kafka-postgres-proxy` rather than something
    /// implicit. Documented behaviour; do not sort this.
    pub port_rules: Vec<PortRule>,
    /// Forward sets keyed by `MMODAL_ENV` value.
    pub environments: BTreeMap<String, Vec<ForwardEntry>>,
}

impl Default for ForwardsConfig {
    fn default() -> Self {
        Self {
            default_port: 443,
            port_rules: Vec::new(),
            environments: BTreeMap::new(),
        }
    }
}

impl ForwardsConfig {
    /// Parse forward definitions. A malformed file yields the default (no
    /// forwards) rather than an error: forwards are a convenience, and a bad
    /// config must never block a VS Code launch.
    pub fn parse(json: &str) -> Self {
        serde_json::from_str(json).unwrap_or_default()
    }

    /// The compiled-in configuration.
    pub fn bundled() -> Self {
        Self::parse(&bundled_forwards())
    }

    /// Entries declared for an environment, matched case-insensitively —
    /// `MMODAL_ENV` is free text and `forwards.json` is typed by hand.
    pub fn entries_for(&self, env: &str) -> &[ForwardEntry] {
        let env = env.trim();
        if env.is_empty() {
            return &[];
        }
        self.environments
            .iter()
            .find(|(key, _)| key.trim().eq_ignore_ascii_case(env))
            .map(|(_, v)| v.as_slice())
            .unwrap_or(&[])
    }

    /// Port for a DNS name: an explicit port wins, then the first matching
    /// rule, then `default_port`.
    pub fn port_for(&self, host: &str, explicit: Option<u16>) -> u16 {
        if let Some(port) = explicit {
            return port;
        }
        let lower = host.to_ascii_lowercase();
        self.port_rules
            .iter()
            .find(|rule| {
                let needle = rule.match_.trim().to_ascii_lowercase();
                !needle.is_empty() && lower.contains(&needle)
            })
            .map(|rule| rule.port)
            .unwrap_or(self.default_port)
    }
}

/// One `IP  name` line from the hosts file.
#[derive(Clone, Debug, PartialEq)]
pub struct HostsEntry {
    pub ip: String,
    pub host: String,
    /// Section this line sits under, i.e. the environment named by the
    /// nearest preceding single-word comment. Empty for lines above any.
    pub section: String,
}

/// Parse a hosts-format file.
///
/// A comment whose text is a **single word** names a section (`# AUCT`);
/// anything longer is an ordinary comment (`# Added by Docker Desktop`).
/// Real hosts files are full of prose comments and none of them should
/// become a phantom environment.
pub fn parse_hosts(text: &str) -> Vec<HostsEntry> {
    let mut entries = Vec::new();
    let mut section = String::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(comment) = trimmed.strip_prefix('#') {
            let comment = comment.trim();
            // `#####` rules and blank comments leave the section alone: they
            // decorate a header rather than replacing it.
            if comment.is_empty() || comment.chars().all(|c| c == '#') {
                continue;
            }
            if !comment.contains(char::is_whitespace) {
                section = comment.trim_matches('#').to_string();
            }
            continue;
        }
        // `IP name [alias...]` — every name on the line maps to that IP.
        let mut parts = trimmed.split_whitespace();
        let Some(ip) = parts.next() else { continue };
        for host in parts {
            if host.starts_with('#') {
                break;
            }
            entries.push(HostsEntry {
                ip: ip.to_string(),
                host: host.to_string(),
                section: section.clone(),
            });
        }
    }

    entries
}

/// Where a resolved forward's local IP came from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ForwardSource {
    /// The environment's section in the hosts file defined this forward.
    HostsSection,
    /// Declared in forwards.json, but bound to the IP the hosts file
    /// already maps that name to.
    HostsIp,
    /// Declared in forwards.json, with no hosts entry for the name — the
    /// tunnel works, addressed by IP, but the name will not resolve in a
    /// browser until the user adds a line.
    ConfigOnly,
}

/// A forward ready to be written into the managed Host block.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedForward {
    /// Local address to bind.
    pub ip: String,
    /// Remote DNS name, resolved on the bastion.
    pub host: String,
    /// Port, used on both ends.
    pub port: u16,
    pub source: ForwardSource,
}

impl ResolvedForward {
    /// The ssh directive for this forward.
    pub fn directive(&self) -> String {
        format!(
            "LocalForward {}:{} {}:{}",
            self.ip, self.port, self.host, self.port
        )
    }

    /// True when the user's hosts file has no entry for this name, so the
    /// name will not resolve locally until they add one.
    pub fn needs_hosts_entry(&self) -> bool {
        self.source == ForwardSource::ConfigOnly
    }
}

/// Resolve the forwards for one environment.
///
/// 1. The hosts file has a section for the environment — those entries are
///    the forward set. Hosts files carry no port, so it comes from a
///    forwards.json entry for the same name if there is one (honouring its
///    explicit port), otherwise from the rules.
/// 2. No section, but the name appears anywhere in the hosts file — take the
///    forwards.json entry and bind the IP the hosts file gives. This is what
///    lets an existing setup work untouched, and it closes a hazard: if
///    forwards.json names an IP the user already points a *different* name
///    at, binding it would silently hijack theirs.
/// 3. Name absent from the hosts file — use the entry as written and mark it
///    [`ForwardSource::ConfigOnly`].
pub fn resolve_forwards(
    config: &ForwardsConfig,
    hosts: &[HostsEntry],
    env: &str,
) -> Vec<ResolvedForward> {
    let env = env.trim();
    let declared = config.entries_for(env);

    let section: Vec<&HostsEntry> = if env.is_empty() {
        Vec::new()
    } else {
        hosts
            .iter()
            .filter(|e| e.section.eq_ignore_ascii_case(env))
            .collect()
    };

    if !section.is_empty() {
        return section
            .into_iter()
            .map(|entry| {
                let declared_entry = declared
                    .iter()
                    .find(|d| d.host.eq_ignore_ascii_case(&entry.host));
                let explicit = declared_entry.and_then(|d| d.port);
                ResolvedForward {
                    ip: entry.ip.clone(),
                    host: entry.host.clone(),
                    port: config.port_for(&entry.host, explicit),
                    source: ForwardSource::HostsSection,
                }
            })
            .collect();
    }

    declared
        .iter()
        .filter(|entry| !entry.host.trim().is_empty())
        .map(|entry| {
            let matching = hosts
                .iter()
                .find(|h| h.host.eq_ignore_ascii_case(entry.host.trim()));
            let (ip, source) = match matching {
                Some(h) => (h.ip.clone(), ForwardSource::HostsIp),
                None => (entry.ip.trim().to_string(), ForwardSource::ConfigOnly),
            };
            ResolvedForward {
                ip,
                host: entry.host.trim().to_string(),
                port: config.port_for(entry.host.trim(), entry.port),
                source,
            }
        })
        .filter(|f| !f.ip.is_empty())
        .collect()
}

/// The hosts-file text for the forwards that have no entry yet, in the
/// sectioned form the user's file already uses. Empty when nothing is
/// missing. This is offered for pasting — the app never writes the file.
pub fn hosts_snippet(env: &str, forwards: &[ResolvedForward]) -> String {
    let missing: Vec<&ResolvedForward> =
        forwards.iter().filter(|f| f.needs_hosts_entry()).collect();
    if missing.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let env = env.trim();
    if !env.is_empty() {
        out.push_str("#####\n");
        out.push_str(&format!("# {}\n", env.to_uppercase()));
        out.push_str("#####\n\n");
    }
    for f in missing {
        out.push_str(&format!("{}  {}\n", f.ip, f.host));
    }
    out
}

/// Default path of the machine's hosts file.
pub fn default_hosts_path() -> String {
    if cfg!(windows) {
        let root = std::env::var("SystemRoot")
            .unwrap_or_else(|_| "C:\\Windows".to_string());
        format!("{root}\\System32\\drivers\\etc\\hosts")
    } else {
        "/etc/hosts".to_string()
    }
}

/// Read and parse a hosts file. A missing or unreadable file is not an
/// error — it means no hosts data, and resolution falls back to
/// forwards.json.
pub fn load_hosts(path: &str) -> Vec<HostsEntry> {
    std::fs::read_to_string(path)
        .map(|text| parse_hosts(&text))
        .unwrap_or_default()
}

/// Warnings for forwards.json entries whose IP has no matching line in that
/// environment's hosts section. This is the drift the two-file split
/// invites; it gets surfaced in the log rather than silently merged.
pub fn drift_warnings(config: &ForwardsConfig, hosts: &[HostsEntry]) -> Vec<String> {
    let mut out = Vec::new();
    for (env, entries) in &config.environments {
        let section: Vec<&HostsEntry> = hosts
            .iter()
            .filter(|h| h.section.eq_ignore_ascii_case(env.trim()))
            .collect();
        if section.is_empty() {
            continue;
        }
        for entry in entries {
            let host = entry.host.trim();
            if host.is_empty() {
                continue;
            }
            match section.iter().find(|h| h.host.eq_ignore_ascii_case(host)) {
                Some(h) if h.ip != entry.ip.trim() && !entry.ip.trim().is_empty() => {
                    out.push(format!(
                        "forwards: {env} {host} is {} in forwards.json but {} in \
                         the hosts file — using the hosts file",
                        entry.ip.trim(),
                        h.ip
                    ));
                }
                None => out.push(format!(
                    "forwards: {env} {host} has no line in the hosts file's \
                     {env} section"
                )),
                _ => {}
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ForwardsConfig {
        ForwardsConfig::parse(
            r#"{
                "default_port": 443,
                "port_rules": [
                    { "match": "postgres", "port": 5432 },
                    { "match": "solr", "port": 8984 },
                    { "match": "kafka", "port": 9094 }
                ],
                "environments": {
                    "AUCT": [
                        { "ip": "127.200.20.1", "host": "uweb.example.net" },
                        { "ip": "127.200.20.2", "host": "pg-postgres.example.net" }
                    ]
                }
            }"#,
        )
    }

    #[test]
    fn malformed_json_yields_no_forwards_rather_than_an_error() {
        let cfg = ForwardsConfig::parse("{ not json");
        assert!(cfg.environments.is_empty());
        // Still usable: a bad config must never block a launch.
        assert_eq!(cfg.default_port, 443);
        assert_eq!(cfg.port_for("anything", None), 443);
    }

    #[test]
    fn port_precedence_explicit_then_rule_then_default() {
        let cfg = config();
        assert_eq!(cfg.port_for("pg-postgres.example.net", Some(6000)), 6000);
        assert_eq!(cfg.port_for("pg-postgres.example.net", None), 5432);
        assert_eq!(cfg.port_for("SOLR-search.example.net", None), 8984);
        assert_eq!(cfg.port_for("kafka1.example.net", None), 9094);
        assert_eq!(cfg.port_for("uweb.example.net", None), 443);
    }

    /// Order is the documented tiebreak, so a name matching two rules is
    /// answered by the earlier one rather than by iteration luck.
    #[test]
    fn first_matching_rule_wins() {
        let cfg = config();
        assert_eq!(cfg.port_for("kafka-postgres-proxy.example.net", None), 5432);
    }

    #[test]
    fn environments_match_case_insensitively() {
        let cfg = config();
        assert_eq!(cfg.entries_for("auct").len(), 2);
        assert_eq!(cfg.entries_for(" AUCT ").len(), 2);
        assert!(cfg.entries_for("").is_empty());
        assert!(cfg.entries_for("NOPE").is_empty());
    }

    #[test]
    fn hosts_sections_come_from_single_word_comments() {
        let entries = parse_hosts(
            "# Copyright (c) 1993-2009 Microsoft Corp.\n\
             127.0.0.1  localhost\n\
             #####\n\
             # AUCT\n\
             #####\n\
             \n\
             127.200.20.1\tuweb.example.net\n\
             127.200.20.2  pg-postgres.example.net  pg-alias\n\
             # Added by Docker Desktop\n\
             127.200.20.3  extra.example.net\n\
             # DEV1\n\
             127.200.10.1  dev.example.net\n",
        );
        // Prose comments do not open a section; the decorative ##### rules
        // do not close one.
        assert_eq!(entries[0].host, "localhost");
        assert_eq!(entries[0].section, "");
        assert_eq!(entries[1].section, "AUCT");
        assert_eq!(entries[2].host, "pg-postgres.example.net");
        assert_eq!(entries[3].host, "pg-alias");
        assert_eq!(entries[3].section, "AUCT");
        assert_eq!(entries[4].host, "extra.example.net");
        assert_eq!(entries[4].section, "AUCT");
        assert_eq!(entries[5].section, "DEV1");
    }

    #[test]
    fn hosts_section_defines_the_forward_set() {
        let hosts = parse_hosts(
            "# AUCT\n\
             127.9.9.1  uweb.example.net\n\
             127.9.9.2  solr-search.example.net\n",
        );
        let out = resolve_forwards(&config(), &hosts, "AUCT");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].ip, "127.9.9.1");
        assert_eq!(out[0].port, 443);
        assert_eq!(out[0].source, ForwardSource::HostsSection);
        // A name only the hosts file knows still gets its port inferred.
        assert_eq!(out[1].port, 8984);
        assert!(out.iter().all(|f| !f.needs_hosts_entry()));
    }

    /// The case that makes an existing setup work untouched: the name is in
    /// the hosts file, so the forward binds the user's IP, not ours.
    #[test]
    fn hosts_ip_overrides_the_configured_ip() {
        let hosts = parse_hosts("127.55.55.55  uweb.example.net\n");
        let out = resolve_forwards(&config(), &hosts, "AUCT");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].ip, "127.55.55.55");
        assert_eq!(out[0].source, ForwardSource::HostsIp);
        assert!(!out[0].needs_hosts_entry());
        // The other name is absent from the hosts file entirely.
        assert_eq!(out[1].ip, "127.200.20.2");
        assert_eq!(out[1].source, ForwardSource::ConfigOnly);
        assert!(out[1].needs_hosts_entry());
    }

    #[test]
    fn no_hosts_file_still_yields_working_forwards() {
        let out = resolve_forwards(&config(), &[], "AUCT");
        assert_eq!(out.len(), 2);
        assert_eq!(
            out[0].directive(),
            "LocalForward 127.200.20.1:443 uweb.example.net:443"
        );
        assert_eq!(
            out[1].directive(),
            "LocalForward 127.200.20.2:5432 pg-postgres.example.net:5432"
        );
        assert!(out.iter().all(|f| f.needs_hosts_entry()));
    }

    #[test]
    fn unknown_or_empty_environment_yields_nothing() {
        assert!(resolve_forwards(&config(), &[], "NOPE").is_empty());
        assert!(resolve_forwards(&config(), &[], "").is_empty());
    }

    #[test]
    fn snippet_lists_only_the_missing_entries() {
        let hosts = parse_hosts("127.55.55.55  uweb.example.net\n");
        let out = resolve_forwards(&config(), &hosts, "auct");
        let snippet = hosts_snippet("auct", &out);
        assert!(snippet.contains("# AUCT"));
        assert!(snippet.contains("127.200.20.2  pg-postgres.example.net"));
        // Already resolved by the user's file — nothing to paste for it.
        assert!(!snippet.contains("uweb.example.net"));
        // Nothing missing at all means nothing to show.
        assert!(hosts_snippet("auct", &resolve_forwards(&config(), &hosts, "auct"))
            .lines()
            .count()
            > 0);
        assert!(hosts_snippet("x", &[]).is_empty());
    }

    #[test]
    fn drift_warnings_flag_a_mismatched_or_missing_line() {
        let hosts = parse_hosts(
            "# AUCT\n\
             127.9.9.1  uweb.example.net\n",
        );
        let warnings = drift_warnings(&config(), &hosts);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings[0].contains("127.200.20.1"), "{warnings:?}");
        assert!(warnings[0].contains("127.9.9.1"), "{warnings:?}");
        assert!(warnings[1].contains("no line"), "{warnings:?}");
        // No section for the environment at all is not drift — that is just
        // a user who has not set the hosts file up.
        assert!(drift_warnings(&config(), &[]).is_empty());
    }

    #[test]
    fn bundled_forwards_parses() {
        let cfg = ForwardsConfig::bundled();
        assert_eq!(cfg.default_port, 443);
        assert!(
            cfg.port_rules.iter().any(|r| r.match_ == "postgres"),
            "bundled forwards.json should ship the port rules"
        );
    }
}

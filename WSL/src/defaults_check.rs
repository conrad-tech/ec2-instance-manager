// Build-time detection of a bundled configuration nobody has filled in yet:
// an `assets/accounts.json` or `assets/features.json` still carrying the
// values this repo ships as a template.
//
// NOTE: regular `//` comments, not `//!` inner docs — this file is also pulled
// into build.rs with `include!`, where inner-doc attributes are a syntax error.
//
// This file is shared verbatim by two compilations so the two never drift:
//   * `build.rs` pulls it in with `include!` and fails the build on anything
//     this reports.
//   * The library compiles it as the `defaults_check` module, which is where
//     its tests live — a build script's own `#[cfg(test)]` code is never run.
//
// WHY A BUILD CHECK. Same reasoning as `forwards_check`, and the same failure
// shape: both files are *valid* as shipped, so nothing at runtime has grounds
// to complain about them. A build made against the template comes up looking
// exactly like a working one — it lists three AWS accounts that do not exist,
// offers a re-clone script pointing at `github.YOUR-ENTERPRISE.com`, and would
// mail a private key to a domain called `test.com` — and says not one word
// about any of it. The app cannot tell "this site really is called that" from
// "nobody edited the file", so the question is asked here, where the answer is
// still cheap and can be shouted about.
//
// The existing accounts/features validators in build.rs check *shape* — every
// required field present, with the right type. This checks *content*, and is
// the only one of the three that a correctly-shaped file can fail.
//
// Only serde_json and std, since build.rs has no access to the crate's own
// modules.

/// Fragments that only ever appear in placeholder values. Matched
/// case-insensitively against every string in the file, so a template value
/// is caught wherever it was copied to.
const PLACEHOLDER_MARKERS: [&str; 8] = [
    "your-company",
    "your-enterprise",
    "your-org",
    "your-repo",
    "changeme",
    "example.com",
    "example.net",
    "example.org",
];

/// The account numbers in the shipped `accounts.json`. They are in AWS's own
/// documentation range and belong to nobody.
const TEMPLATE_ACCOUNT_IDS: [&str; 3] = ["123456789012", "234567890123", "345678901234"];

/// The mail domains the shipped `features.json` puts in
/// `access_email.email_domains`.
const TEMPLATE_EMAIL_DOMAINS: [&str; 2] = ["test.com", "test2.com"];

/// Keys beginning with `_` are documentation. Both files lean on them heavily
/// — most of `features.json` by volume is `_*_comment` prose, and that prose
/// names example domains and example hosts on purpose.
fn is_comment_key(key: &str) -> bool {
    key.starts_with('_')
}

/// The placeholder fragment `text` carries, if any.
fn placeholder_marker(text: &str) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    PLACEHOLDER_MARKERS
        .into_iter()
        .find(|marker| lower.contains(marker))
}

/// Walk every string in `value`, reporting the ones that are still template
/// text. `path` is the dotted location reported to the user.
fn scan_placeholders(value: &serde_json::Value, path: &str, problems: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => {
            if let Some(marker) = placeholder_marker(text) {
                problems.push(format!(
                    "{path} is still a template value ({text:?} contains `{marker}`)"
                ));
            }
        }
        serde_json::Value::Array(items) => {
            for (idx, item) in items.iter().enumerate() {
                scan_placeholders(item, &format!("{path}[{idx}]"), problems);
            }
        }
        serde_json::Value::Object(map) => {
            for (key, item) in map {
                if is_comment_key(key) {
                    continue;
                }
                let child = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                scan_placeholders(item, &child, problems);
            }
        }
        _ => {}
    }
}

/// Whether `section`'s `allowed_users` list hands the feature to anybody. A
/// section nobody can reach is dead code in the binary, and holding up a
/// build over the template text inside it would force edits to a feature the
/// site has deliberately not switched on.
fn section_has_users(section: &serde_json::Value) -> bool {
    section
        .get("allowed_users")
        .and_then(|v| v.as_array())
        .map(|list| !list.is_empty())
        .unwrap_or(false)
}

/// Whether `section` carries `"enabled": true`.
fn section_enabled(section: &serde_json::Value) -> bool {
    section
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// A GUID whose every hex digit is `0` — the placeholder RMS/IRM template id
/// in the shipped `access_email` block, which must be replaced with the
/// tenant's own before an encrypted send can work.
fn is_zero_guid(text: &str) -> bool {
    let digits: Vec<char> = text
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    !digits.is_empty() && digits.iter().all(|c| *c == '0')
}

/// Report every value in an `accounts.json` that is still the shipped
/// template. An empty result means the file names real accounts.
///
/// Shape is *not* checked here — `build.rs` already does that, and this runs
/// after it, so a file that reaches this point has every required field.
pub fn check_accounts_json(raw: &str) -> Vec<String> {
    let value: serde_json::Value = match serde_json::from_str(raw) {
        // Unreadable is the shape check's problem, not this one; reporting it
        // twice with two different remedies is worse than staying quiet.
        Err(_) => return Vec::new(),
        Ok(v) => v,
    };
    let entries = match value.as_array() {
        Some(entries) => entries,
        None => return Vec::new(),
    };

    let mut problems: Vec<String> = Vec::new();
    for (idx, entry) in entries.iter().enumerate() {
        let at = format!("[{idx}]");
        if let Some(id) = entry.get("account_id").and_then(|v| v.as_str()) {
            if TEMPLATE_ACCOUNT_IDS.contains(&id) {
                problems.push(format!(
                    "{at}.account_id is still the example account number `{id}`"
                ));
            }
        }
        scan_placeholders(entry, &at, &mut problems);
    }
    problems
}

/// Report every value in a `features.json` that is still the shipped
/// template, ignoring sections the file itself switches off.
pub fn check_features_json(raw: &str) -> Vec<String> {
    let value: serde_json::Value = match serde_json::from_str(raw) {
        Err(_) => return Vec::new(),
        Ok(v) => v,
    };
    let obj = match value.as_object() {
        Some(obj) => obj,
        None => return Vec::new(),
    };

    let mut problems: Vec<String> = Vec::new();
    for (key, section) in obj {
        if is_comment_key(key) {
            continue;
        }
        // `personal_scripts` carries the git host and the default scripts,
        // and hands out neither until somebody is on its allow-list.
        if key == "personal_scripts" && !section_has_users(section) {
            continue;
        }
        // `access_email` is the one block gated by `enabled` rather than by
        // an allow-list.
        if key == "access_email" && !section_enabled(section) {
            continue;
        }
        scan_placeholders(section, key, &mut problems);
    }

    if let Some(email) = obj.get("access_email").filter(|s| section_enabled(s)) {
        // Neither of these reads as a placeholder on its own — `test.com` is
        // a real domain and the GUID is well-formed — so they are named
        // outright. Both are load-bearing: the domain list is what stops a
        // private key reaching a stale contact, and the GUID is what makes
        // the mail encrypt at all.
        if let Some(domains) = email.get("email_domains").and_then(|v| v.as_array()) {
            for (idx, domain) in domains.iter().enumerate() {
                let text = domain.as_str().unwrap_or_default();
                if TEMPLATE_EMAIL_DOMAINS.contains(&text.to_ascii_lowercase().as_str()) {
                    problems.push(format!(
                        "access_email.email_domains[{idx}] is still the example mail domain \
                         `{text}` — a private key is only ever sent unattended to an address \
                         inside these domains"
                    ));
                }
            }
        }
        if let Some(guid) = email.get("encrypt_template_guid").and_then(|v| v.as_str()) {
            if is_zero_guid(guid) {
                problems.push(format!(
                    "access_email.encrypt_template_guid is still the all-zeros placeholder \
                     `{guid}` — discover your tenant's own with \
                     scripts/outlook_verification.ps1"
                ));
            }
        }
    }

    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_configured_accounts_file_is_accepted() {
        // `r##` because the colour value contains `"#`, which closes an
        // ordinary `r#` raw string.
        let raw = r##"[
            {
                "label": "Prod",
                "account_id": "918273645500",
                "region": "us-east-1",
                "sort_order": 1,
                "color": "#c82828",
                "vault_addr": "https://vault.prod.acme.net:8200"
            }
        ]"##;
        assert!(check_accounts_json(raw).is_empty(), "{:#?}", check_accounts_json(raw));
    }

    #[test]
    fn the_example_account_numbers_are_named() {
        let raw = r#"[{ "label": "Dev", "account_id": "123456789012", "region": "us-east-1" }]"#;
        let found = check_accounts_json(raw);
        assert!(
            found.iter().any(|p| p.contains("[0].account_id") && p.contains("123456789012")),
            "{found:#?}"
        );
    }

    /// The placeholder domain is nested two levels down, inside an array of
    /// environments — the scan has to reach it wherever it was copied to.
    #[test]
    fn a_placeholder_domain_is_named_with_its_path() {
        let raw = r#"[{
            "label": "Dev",
            "account_id": "918273645500",
            "environments": [
                { "name": "DEV1", "vault_addr": "https://vault.dev1.YOUR-COMPANY.com:8200" }
            ]
        }]"#;
        let found = check_accounts_json(raw);
        assert!(
            found.iter().any(|p| p.contains("[0].environments[0].vault_addr")),
            "{found:#?}"
        );
    }

    /// Most of features.json by volume is `_*_comment` prose, and that prose
    /// names example hosts and example domains deliberately. Reading it as
    /// configuration would make the check impossible to satisfy.
    #[test]
    fn comment_keys_are_documentation_and_never_configuration() {
        let raw = r#"{
            "_comment": "clone from github.YOUR-ENTERPRISE.com/YOUR-ORG/YOUR-REPO",
            "_accounts": ["123456789012", "https://vault.example.com"],
            "allow_delete_user": false
        }"#;
        assert!(check_features_json(raw).is_empty(), "{:#?}", check_features_json(raw));

        let accounts = r#"[{ "_note": "e.g. 123456789012", "account_id": "918273645500" }]"#;
        assert!(check_accounts_json(accounts).is_empty(), "{:#?}", check_accounts_json(accounts));
    }

    /// A section nobody can reach ships its template text harmlessly — the
    /// binary hands it to no one. Failing the build over it would force a
    /// site to configure a feature it has deliberately left switched off.
    #[test]
    fn a_section_with_no_users_is_not_holding_up_the_build() {
        let off = r#"{
            "personal_scripts": {
                "allowed_users": [],
                "git_host": "github.YOUR-ENTERPRISE.com",
                "default_scripts": [
                    { "name": "Re-clone", "body": "git clone https://github.YOUR-ENTERPRISE.com/YOUR-ORG/YOUR-REPO.git" }
                ]
            }
        }"#;
        assert!(check_features_json(off).is_empty(), "{:#?}", check_features_json(off));

        let on = off.replace(r#""allowed_users": []"#, r#""allowed_users": ["bconrad"]"#);
        let found = check_features_json(&on);
        assert!(
            found.iter().any(|p| p.contains("personal_scripts.git_host")),
            "{found:#?}"
        );
        assert!(
            found.iter().any(|p| p.contains("personal_scripts.default_scripts[0].body")),
            "{found:#?}"
        );
    }

    #[test]
    fn a_disabled_access_email_block_is_not_holding_up_the_build() {
        let off = r#"{
            "access_email": {
                "enabled": false,
                "email_domains": ["test.com", "test2.com"],
                "encrypt_template_guid": "{00000000-0000-0000-0000-000000000000}"
            }
        }"#;
        assert!(check_features_json(off).is_empty(), "{:#?}", check_features_json(off));
    }

    /// Neither of these reads as a placeholder by inspection, so both are
    /// named by value rather than by pattern.
    #[test]
    fn the_example_mail_domain_and_the_zero_guid_are_named() {
        let raw = r#"{
            "access_email": {
                "enabled": true,
                "email_domains": ["test.com", "acme.net"],
                "encrypt_template_guid": "{00000000-0000-0000-0000-000000000000}"
            }
        }"#;
        let found = check_features_json(raw);
        assert!(
            found.iter().any(|p| p.contains("email_domains[0]") && p.contains("test.com")),
            "{found:#?}"
        );
        assert!(
            found.iter().any(|p| p.contains("encrypt_template_guid")),
            "{found:#?}"
        );
        assert_eq!(found.len(), 2, "the real domain must not be reported: {found:#?}");
    }

    #[test]
    fn a_configured_access_email_block_is_accepted() {
        let raw = r#"{
            "access_email": {
                "enabled": true,
                "email_domains": ["acme.net"],
                "encrypt_template_guid": "{2b0f8a11-0000-4c3d-9f10-a1b2c3d4e5f6}"
            }
        }"#;
        assert!(check_features_json(raw).is_empty(), "{:#?}", check_features_json(raw));
    }

    /// A GUID is only the placeholder when *every* digit is zero; one that
    /// merely contains a run of them is somebody's real template id.
    #[test]
    fn only_an_all_zeros_guid_is_the_placeholder() {
        assert!(is_zero_guid("{00000000-0000-0000-0000-000000000000}"));
        assert!(is_zero_guid("00000000-0000-0000-0000-000000000000"));
        assert!(!is_zero_guid("{00000000-0000-4c3d-0000-000000000001}"));
        assert!(!is_zero_guid(""));
        assert!(!is_zero_guid("{}"));
    }

    /// Shape is build.rs's own check, and it runs first. Reporting an
    /// unreadable file here as well would offer a second, wrong remedy for
    /// it — "fill in your accounts" for a file with a stray comma.
    #[test]
    fn an_unreadable_file_is_left_to_the_shape_check() {
        assert!(check_accounts_json("[ {,, ]").is_empty());
        assert!(check_features_json("not json at all").is_empty());
        assert!(check_accounts_json(r#"{"not":"an array"}"#).is_empty());
        assert!(check_features_json("[]").is_empty());
    }
}

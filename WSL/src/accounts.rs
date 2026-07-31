use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::config::AppConfig;
use crate::models::ProfileConfig;

/// Compiled-in default account list from `assets/accounts.json`, obfuscated at
/// build time (see [`crate::obf_core`]) so the account inventory does not sit
/// in the binary as readable JSON. `build.rs` writes the scrambled blob; we
/// unscramble it here. Edit `assets/accounts.json` and rebuild to change it.
const BUNDLED_ACCOUNTS_OBF: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/accounts.json.obf"));

fn bundled_accounts() -> String {
    let plain = crate::obf_core::obf_transform(BUNDLED_ACCOUNTS_OBF);
    String::from_utf8(plain).expect("bundled accounts.json is valid UTF-8")
}

#[derive(Deserialize)]
struct AccountEntry {
    label: String,
    account_id: String,
    region: Option<String>,
    sort_order: Option<u32>,
    color: Option<String>,
    /// Account-level Vault server URL, used by every environment in the
    /// account that doesn't declare its own.
    #[serde(default)]
    vault_addr: Option<String>,
    /// Environments hosted in this account. Several accounts host two, told
    /// apart by each instance's `MMODAL_ENV` tag. Absent for accounts with a
    /// single environment.
    #[serde(default)]
    environments: Option<Vec<AccountEnvironment>>,
}

/// One environment declared inside an account in `accounts.json`.
///
/// `name` must match the instances' `MMODAL_ENV` tag (compared
/// case-insensitively and trimmed) — that is what narrows the bastion
/// dropdowns in the Scripts dialogs to a single environment.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AccountEnvironment {
    pub name: String,
    #[serde(default)]
    pub vault_addr: Option<String>,
}

fn accounts_path() -> Option<PathBuf> {
    AppConfig::config_path().map(|p| p.with_file_name("accounts.json"))
}

/// Delete the `accounts.json` reference copy that older builds wrote into the
/// user's config dir. Silent and best-effort: a missing file is the expected
/// case, and a permissions failure must not stop the app from loading.
fn remove_stale_accounts_file(path: &Path) {
    let _ = fs::remove_file(path);
}

fn parse_accounts(json: &str) -> Vec<ProfileConfig> {
    let Ok(entries) = serde_json::from_str::<Vec<AccountEntry>>(json) else {
        return Vec::new();
    };
    entries
        .into_iter()
        .map(|e| ProfileConfig {
            profile_id: e.account_id.clone(),
            display_name: e.label,
            account_id: e.account_id,
            region: e.region,
            sort_order: e.sort_order,
            color: e.color,
        })
        .collect()
}

/// Trimmed value of an optional string, dropping blanks so an empty JSON
/// string behaves the same as an absent field.
fn non_blank(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// Environment names match the `MMODAL_ENV` instance tag, which users type by
/// hand in both places — compare them case-insensitively and trimmed.
fn env_eq(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

fn find_entry<'a>(entries: &'a [AccountEntry], account_id: &str) -> Option<&'a AccountEntry> {
    entries.iter().find(|e| e.account_id == account_id)
}

/// Environments declared for an account, in declaration order. Entries with a
/// blank name are dropped: they would produce an unselectable row.
fn environments_in(json: &str, account_id: &str) -> Vec<AccountEnvironment> {
    let Ok(entries) = serde_json::from_str::<Vec<AccountEntry>>(json) else {
        return Vec::new();
    };
    find_entry(&entries, account_id)
        .and_then(|e| e.environments.as_ref())
        .map(|envs| {
            envs.iter()
                .filter(|e| !e.name.trim().is_empty())
                .map(|e| AccountEnvironment {
                    name: e.name.trim().to_string(),
                    vault_addr: non_blank(&e.vault_addr),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Vault address for an environment: the environment's own value if it
/// declares one, otherwise the account-level value, otherwise `None`.
///
/// Pass an empty `env` for accounts with no environment dimension (untagged
/// instances) — that resolves straight to the account-level value.
fn vault_addr_in(json: &str, account_id: &str, env: &str) -> Option<String> {
    let Ok(entries) = serde_json::from_str::<Vec<AccountEntry>>(json) else {
        return None;
    };
    let entry = find_entry(&entries, account_id)?;
    if !env.trim().is_empty() {
        let from_env = entry
            .environments
            .as_ref()
            .and_then(|envs| envs.iter().find(|e| env_eq(&e.name, env)))
            .and_then(|e| non_blank(&e.vault_addr));
        if from_env.is_some() {
            return from_env;
        }
    }
    non_blank(&entry.vault_addr)
}

/// Environments declared for an account in the bundled `accounts.json`.
///
/// This is only half the list shown in the Scripts dialogs — see
/// [`crate::script_env`], which unions it with the environments actually
/// discovered in the account's inventory.
pub fn environments_for(account_id: &str) -> Vec<AccountEnvironment> {
    environments_in(&bundled_accounts(), account_id)
}

/// Vault address to pre-fill for an account/environment pair, with the
/// environment-level value taking precedence over the account-level one.
pub fn vault_addr_for(account_id: &str, env: &str) -> Option<String> {
    vault_addr_in(&bundled_accounts(), account_id, env)
}

/// Load the ordered account list.
///
/// Always uses the bundled `assets/accounts.json` compiled into the binary —
/// edit that file and rebuild to change accounts.
///
/// This used to also write the bundled JSON to `<config_dir>/accounts.json` on
/// every load, as a reference copy. Nothing ever read it back, and it put a
/// plaintext inventory of our AWS accounts (labels, account IDs, regions) into
/// every user's config directory, rewritten on each launch so deleting it did
/// not help. That write is gone, and we now proactively delete any copy an
/// older build left behind, so upgrading users stop having it on disk. The
/// copy inside the binary is now obfuscated (see [`bundled_accounts`]) so it is
/// no longer readable as plaintext JSON via `strings`.
pub fn load_accounts() -> Vec<ProfileConfig> {
    if let Some(path) = accounts_path() {
        remove_stale_accounts_file(&path);
    }
    parse_accounts(&bundled_accounts())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_entry() {
        let profiles = parse_accounts(
            r#"[{"label":"Dev","account_id":"123456789012","region":"us-east-1"}]"#,
        );
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].profile_id, "123456789012");
        assert_eq!(profiles[0].display_name, "Dev");
        assert_eq!(profiles[0].account_id, "123456789012");
        assert_eq!(profiles[0].region.as_deref(), Some("us-east-1"));
    }

    #[test]
    fn parse_entry_without_region() {
        let profiles = parse_accounts(
            r#"[{"label":"Prod","account_id":"999999999999"}]"#,
        );
        assert_eq!(profiles[0].region, None);
    }

    #[test]
    fn parse_preserves_order() {
        let profiles = parse_accounts(
            r#"[
                {"label":"A","account_id":"111"},
                {"label":"B","account_id":"222"},
                {"label":"C","account_id":"333"}
            ]"#,
        );
        assert_eq!(profiles[0].profile_id, "111");
        assert_eq!(profiles[1].profile_id, "222");
        assert_eq!(profiles[2].profile_id, "333");
    }

    #[test]
    fn remove_stale_accounts_file_deletes_and_tolerates_absence() {
        let mut path = std::env::temp_dir();
        path.push(format!("ec2mgr_accounts_test_{}.json", std::process::id()));
        fs::write(&path, "[]").unwrap();
        assert!(path.exists());

        remove_stale_accounts_file(&path);
        assert!(!path.exists(), "stale accounts file should be deleted");

        // Second call on a now-missing file must not panic or error.
        remove_stale_accounts_file(&path);
        assert!(!path.exists());
    }

    /// Two accounts: one declaring two environments (the second without its
    /// own Vault server), one declaring none.
    const ENV_JSON: &str = r#"[
        {
            "label": "Dev",
            "account_id": "111",
            "environments": [
                { "name": "DEV1", "vault_addr": "https://vault.dev1" },
                { "name": "DEV2" }
            ],
            "vault_addr": "https://vault.acct"
        },
        {
            "label": "Prod",
            "account_id": "222",
            "vault_addr": "https://vault.prod"
        }
    ]"#;

    #[test]
    fn environments_parse_in_declaration_order() {
        let envs = environments_in(ENV_JSON, "111");
        assert_eq!(envs.len(), 2);
        assert_eq!(envs[0].name, "DEV1");
        assert_eq!(envs[1].name, "DEV2");
        assert_eq!(envs[0].vault_addr.as_deref(), Some("https://vault.dev1"));
        assert_eq!(envs[1].vault_addr, None);
    }

    #[test]
    fn environments_absent_or_unknown_account_is_empty() {
        assert!(environments_in(ENV_JSON, "222").is_empty());
        assert!(environments_in(ENV_JSON, "nope").is_empty());
    }

    #[test]
    fn environments_drop_blank_names_and_trim() {
        let envs = environments_in(
            r#"[{"label":"A","account_id":"1","environments":[
                {"name":"  E1  "},{"name":"   "},{"name":""}
            ]}]"#,
            "1",
        );
        assert_eq!(envs.len(), 1, "blank-named environments are unselectable");
        assert_eq!(envs[0].name, "E1");
    }

    #[test]
    fn vault_addr_env_level_beats_account_level() {
        assert_eq!(
            vault_addr_in(ENV_JSON, "111", "DEV1").as_deref(),
            Some("https://vault.dev1")
        );
    }

    #[test]
    fn vault_addr_falls_back_to_account_level() {
        // DEV2 declares no vault_addr of its own.
        assert_eq!(
            vault_addr_in(ENV_JSON, "111", "DEV2").as_deref(),
            Some("https://vault.acct")
        );
        // An environment name that isn't declared at all still gets the
        // account-level value — it may have been discovered from a tag.
        assert_eq!(
            vault_addr_in(ENV_JSON, "111", "DEV9").as_deref(),
            Some("https://vault.acct")
        );
    }

    #[test]
    fn vault_addr_matches_env_case_insensitively() {
        // accounts.json says "DEV1"; the MMODAL_ENV tag may say "dev1 ".
        assert_eq!(
            vault_addr_in(ENV_JSON, "111", "dev1 ").as_deref(),
            Some("https://vault.dev1")
        );
    }

    #[test]
    fn vault_addr_empty_env_uses_account_level() {
        // Untagged single-environment account: no env dimension at all.
        assert_eq!(
            vault_addr_in(ENV_JSON, "222", "").as_deref(),
            Some("https://vault.prod")
        );
    }

    #[test]
    fn vault_addr_absent_yields_none() {
        assert_eq!(vault_addr_in(r#"[{"label":"A","account_id":"1"}]"#, "1", ""), None);
        // Blank string is treated as absent, not as an empty address.
        assert_eq!(
            vault_addr_in(r#"[{"label":"A","account_id":"1","vault_addr":"  "}]"#, "1", ""),
            None
        );
        assert_eq!(vault_addr_in(ENV_JSON, "unknown", ""), None);
    }

    #[test]
    fn accounts_without_new_fields_still_parse() {
        // The pre-environments file shape must keep working untouched.
        let json = r#"[{"label":"Dev","account_id":"123","region":"us-east-1"}]"#;
        let profiles = parse_accounts(json);
        assert_eq!(profiles.len(), 1);
        assert!(environments_in(json, "123").is_empty());
        assert_eq!(vault_addr_in(json, "123", ""), None);
    }

    #[test]
    fn bundled_accounts_environment_names_are_unique_per_account() {
        // A duplicate name in the shipped file would render two identical
        // rows in the Scripts environment dropdown.
        let json = bundled_accounts();
        for profile in parse_accounts(&json) {
            let envs = environments_in(&json, &profile.account_id);
            for (i, env) in envs.iter().enumerate() {
                assert!(
                    !envs[..i].iter().any(|e| env_eq(&e.name, &env.name)),
                    "account {} declares environment {} twice",
                    profile.account_id,
                    env.name
                );
            }
        }
    }

    #[test]
    fn parse_empty_array() {
        let profiles = parse_accounts("[]");
        assert!(profiles.is_empty());
    }

    #[test]
    fn bundled_accounts_parses_cleanly() {
        // Also exercises the build-time obfuscation round-trip: bundled_accounts()
        // decrypts the OUT_DIR blob, and it must yield parseable JSON.
        let profiles = parse_accounts(&bundled_accounts());
        assert!(!profiles.is_empty(), "bundled accounts.json should have at least one entry");
        for p in &profiles {
            assert!(!p.profile_id.is_empty(), "profile_id (account_id) must not be empty");
            assert!(!p.display_name.is_empty(), "label field must not be empty");
            assert!(!p.account_id.is_empty(), "account_id field must not be empty");
        }
    }
}

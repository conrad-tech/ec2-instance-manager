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

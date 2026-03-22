use std::fs;
use std::path::PathBuf;

use serde::Deserialize;

use crate::config::AppConfig;
use crate::models::ProfileConfig;

/// Compiled-in default account list from `assets/accounts.json`.
/// Edit that file and rebuild to change the defaults.
const BUNDLED_ACCOUNTS: &str = include_str!("../assets/accounts.json");

#[derive(Deserialize)]
struct AccountEntry {
    label: String,
    account_id: String,
    region: Option<String>,
    sort_order: Option<u32>,
    color: Option<String>,
}

pub fn accounts_path() -> Option<PathBuf> {
    AppConfig::config_path().map(|p| p.with_file_name("accounts.json"))
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
/// Priority:
/// 1. `accounts.json` in the user config dir (runtime override — add/remove without rebuilding)
/// 2. `assets/accounts.json` compiled into the binary (edit in repo, then rebuild)
pub fn load_accounts() -> Vec<ProfileConfig> {
    if let Some(path) = accounts_path() {
        if path.exists() {
            if let Ok(content) = fs::read_to_string(&path) {
                let profiles = parse_accounts(&content);
                if !profiles.is_empty() {
                    return profiles;
                }
            }
        }
    }
    parse_accounts(BUNDLED_ACCOUNTS)
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
    fn parse_empty_array() {
        let profiles = parse_accounts("[]");
        assert!(profiles.is_empty());
    }

    #[test]
    fn bundled_accounts_parses_cleanly() {
        let profiles = parse_accounts(BUNDLED_ACCOUNTS);
        assert!(!profiles.is_empty(), "bundled accounts.json should have at least one entry");
        for p in &profiles {
            assert!(!p.profile_id.is_empty(), "profile_id (account_id) must not be empty");
            assert!(!p.display_name.is_empty(), "label field must not be empty");
            assert!(!p.account_id.is_empty(), "account_id field must not be empty");
        }
    }
}

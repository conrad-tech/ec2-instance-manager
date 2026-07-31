//! Environment selection for the Scripts dialogs.
//!
//! Several AWS accounts host more than one environment, told apart by the
//! `MMODAL_ENV` tag on each instance. The Scripts dialogs therefore select an
//! *environment* rather than an *account*, and narrow their bastion dropdowns
//! to instances carrying that tag value.
//!
//! The list shown in the dropdown is the **union** of the environments declared
//! for the account in `accounts.json` (which is where a `vault_addr` can be
//! attached) and the environments actually discovered in the account's loaded
//! inventory. That way a new environment becomes selectable as soon as its
//! instances appear, and declaring it is only needed to give it a Vault
//! address.
//!
//! This module is deliberately free of `egui` and of the inventory types so it
//! can be unit tested directly.

use crate::accounts::AccountEnvironment;

/// One selectable row in a Scripts dialog's Environment dropdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptEnv {
    pub account_id: String,
    pub account_label: String,
    /// The `MMODAL_ENV` tag value this row targets. Empty means the account
    /// has no environment dimension at all (nothing declared, no tagged
    /// instances) — the pre-existing whole-account behavior.
    pub env: String,
    /// Text shown in the dropdown: `Account — ENV`, or just `Account` when the
    /// account has a single environment (the suffix would add nothing).
    pub label: String,
}

impl ScriptEnv {
    /// Stable identity for a row, used to match the current selection and to
    /// key the cached bastion pair.
    pub fn key(&self) -> (String, String) {
        (self.account_id.clone(), self.env.clone())
    }
}

/// Environment names are typed by hand in both `accounts.json` and the AWS tag,
/// so they are compared case-insensitively and trimmed.
pub fn env_eq(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// Whether an instance carrying `tag` belongs to environment `env`.
///
/// An empty `env` matches everything: that is the untagged-account case, where
/// no environment filter should be applied at all.
pub fn env_matches(tag: Option<&str>, env: &str) -> bool {
    if env.trim().is_empty() {
        return true;
    }
    tag.is_some_and(|t| env_eq(t, env))
}

/// Build the Environment dropdown rows for one account.
///
/// `declared` comes from `accounts.json`, `discovered` from the `MMODAL_ENV`
/// tags in that account's inventory (duplicates are fine — they are collapsed
/// here). Declared environments keep their declaration order and their declared
/// spelling; extra discovered ones follow, sorted alphabetically.
pub fn build(
    account_id: &str,
    account_label: &str,
    declared: &[AccountEnvironment],
    discovered: &[String],
) -> Vec<ScriptEnv> {
    let mut names: Vec<String> = Vec::new();
    let push_unique = |name: &str, names: &mut Vec<String>| {
        let name = name.trim();
        if name.is_empty() || names.iter().any(|n| env_eq(n, name)) {
            return;
        }
        names.push(name.to_string());
    };

    for env in declared {
        push_unique(&env.name, &mut names);
    }

    // Discovered names are appended in a stable alphabetical order rather than
    // inventory order, which depends on how AWS happened to return the page.
    let mut extra: Vec<&str> = discovered.iter().map(|s| s.trim()).collect();
    extra.sort_by_key(|s| s.to_ascii_lowercase());
    for name in extra {
        push_unique(name, &mut names);
    }

    // No environment dimension: one row for the account itself. Bastion
    // dropdowns then apply no environment filter, exactly as before.
    if names.is_empty() {
        return vec![ScriptEnv {
            account_id: account_id.to_string(),
            account_label: account_label.to_string(),
            env: String::new(),
            label: account_label.to_string(),
        }];
    }

    let single = names.len() == 1;
    names
        .into_iter()
        .map(|env| ScriptEnv {
            account_id: account_id.to_string(),
            account_label: account_label.to_string(),
            label: if single {
                account_label.to_string()
            } else {
                format!("{account_label} — {env}")
            },
            env,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared(names: &[&str]) -> Vec<AccountEnvironment> {
        names
            .iter()
            .map(|n| AccountEnvironment {
                name: (*n).to_string(),
                vault_addr: None,
            })
            .collect()
    }

    fn owned(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_string()).collect()
    }

    #[test]
    fn declared_and_discovered_are_unioned() {
        let rows = build("111", "Dev", &declared(&["DEV1"]), &owned(&["DEV2"]));
        assert_eq!(
            rows.iter().map(|r| r.env.as_str()).collect::<Vec<_>>(),
            ["DEV1", "DEV2"]
        );
    }

    #[test]
    fn declared_spelling_wins_over_discovered() {
        // accounts.json says "DEV1"; the tag says "dev1". One row, declared
        // casing, so the label matches what the admin wrote.
        let rows = build("111", "Dev", &declared(&["DEV1"]), &owned(&["dev1"]));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].env, "DEV1");
    }

    #[test]
    fn declared_order_is_preserved_and_discovered_are_sorted() {
        let rows = build(
            "111",
            "Dev",
            &declared(&["ZONE-B", "ZONE-A"]),
            &owned(&["zeta", "alpha"]),
        );
        assert_eq!(
            rows.iter().map(|r| r.env.as_str()).collect::<Vec<_>>(),
            ["ZONE-B", "ZONE-A", "alpha", "zeta"]
        );
    }

    #[test]
    fn duplicate_discovered_values_collapse() {
        // Every instance carries the tag, so the raw list is full of repeats.
        let rows = build(
            "111",
            "Dev",
            &[],
            &owned(&["DEV1", "DEV1", "DEV2", "DEV1"]),
        );
        assert_eq!(
            rows.iter().map(|r| r.env.as_str()).collect::<Vec<_>>(),
            ["DEV1", "DEV2"]
        );
    }

    #[test]
    fn multi_environment_rows_are_labelled_with_the_environment() {
        let rows = build("111", "Dev", &declared(&["DEV1", "DEV2"]), &[]);
        assert_eq!(rows[0].label, "Dev — DEV1");
        assert_eq!(rows[1].label, "Dev — DEV2");
    }

    #[test]
    fn single_environment_row_is_labelled_with_just_the_account() {
        // The suffix adds no information when there is only one choice.
        let rows = build("222", "Prod", &declared(&["PROD"]), &[]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "Prod");
        assert_eq!(rows[0].env, "PROD", "the filter still uses the tag value");
    }

    #[test]
    fn single_discovered_environment_also_drops_the_suffix() {
        let rows = build("222", "Prod", &[], &owned(&["PROD"]));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "Prod");
    }

    #[test]
    fn untagged_account_yields_one_whole_account_row() {
        let rows = build("333", "Legacy", &[], &[]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].label, "Legacy");
        assert_eq!(
            rows[0].env, "",
            "an empty env means no environment filter is applied"
        );
    }

    #[test]
    fn blank_names_are_ignored_on_both_sides() {
        let rows = build("111", "Dev", &declared(&["  ", ""]), &owned(&["", "   "]));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].env, "", "all names were blank — treat as untagged");
    }

    #[test]
    fn names_are_trimmed() {
        let rows = build("111", "Dev", &[], &owned(&["  DEV1  "]));
        assert_eq!(rows[0].env, "DEV1");
    }

    #[test]
    fn env_matches_is_case_insensitive_and_trimmed() {
        assert!(env_matches(Some("DEV1"), "dev1"));
        assert!(env_matches(Some(" dev1 "), "DEV1"));
        assert!(!env_matches(Some("DEV2"), "DEV1"));
    }

    #[test]
    fn empty_env_matches_everything_including_untagged() {
        assert!(env_matches(Some("DEV1"), ""));
        assert!(env_matches(None, ""));
        assert!(env_matches(None, "   "));
    }

    #[test]
    fn untagged_instance_never_matches_a_named_environment() {
        // Otherwise an untagged box would show up under every environment and
        // a script could be aimed at it by accident.
        assert!(!env_matches(None, "DEV1"));
    }

    #[test]
    fn key_pairs_account_and_environment() {
        let rows = build("111", "Dev", &declared(&["DEV1", "DEV2"]), &[]);
        assert_eq!(rows[0].key(), ("111".to_string(), "DEV1".to_string()));
        assert_ne!(rows[0].key(), rows[1].key());
    }
}

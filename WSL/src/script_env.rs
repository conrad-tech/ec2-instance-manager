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
    /// Text shown in the dropdown: the environment name on its own. Accounts
    /// with no environment dimension show their account label instead, since
    /// there is nothing else to name them by.
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
///
/// `hidden` is the toolbar's **Exclude Env** selection: those environments are
/// dropped from the list, so the Scripts dialogs offer the same environments
/// the Inventory page is showing. An account whose environments are *all*
/// excluded contributes no rows at all — it does **not** fall back to the
/// unfiltered whole-account row, which would quietly re-expose every bastion
/// the user just hid.
pub fn build(
    account_id: &str,
    account_label: &str,
    declared: &[AccountEnvironment],
    discovered: &[String],
    hidden: &[String],
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
    // dropdowns then apply no environment filter, exactly as before. There is
    // no environment name here, so Exclude Env has nothing to act on.
    if names.is_empty() {
        return vec![ScriptEnv {
            account_id: account_id.to_string(),
            account_label: account_label.to_string(),
            env: String::new(),
            label: account_label.to_string(),
        }];
    }

    names
        .into_iter()
        .filter(|env| !hidden.iter().any(|h| env_eq(h, env)))
        .map(|env| ScriptEnv {
            account_id: account_id.to_string(),
            account_label: account_label.to_string(),
            label: env.clone(),
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

    /// Nothing excluded — the common case.
    fn rows(
        account_id: &str,
        label: &str,
        decl: &[AccountEnvironment],
        disc: &[String],
    ) -> Vec<ScriptEnv> {
        build(account_id, label, decl, disc, &[])
    }

    #[test]
    fn declared_and_discovered_are_unioned() {
        let r = rows("111", "Dev", &declared(&["DEV1"]), &owned(&["DEV2"]));
        assert_eq!(r.iter().map(|r| r.env.as_str()).collect::<Vec<_>>(), ["DEV1", "DEV2"]);
    }

    #[test]
    fn declared_spelling_wins_over_discovered() {
        // accounts.json says "DEV1"; the tag says "dev1". One row, declared
        // casing, so the label matches what the admin wrote.
        let r = rows("111", "Dev", &declared(&["DEV1"]), &owned(&["dev1"]));
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].env, "DEV1");
    }

    #[test]
    fn declared_order_is_preserved_and_discovered_are_sorted() {
        let r = rows(
            "111",
            "Dev",
            &declared(&["ZONE-B", "ZONE-A"]),
            &owned(&["zeta", "alpha"]),
        );
        assert_eq!(
            r.iter().map(|r| r.env.as_str()).collect::<Vec<_>>(),
            ["ZONE-B", "ZONE-A", "alpha", "zeta"]
        );
    }

    #[test]
    fn duplicate_discovered_values_collapse() {
        // Every instance carries the tag, so the raw list is full of repeats.
        let r = rows("111", "Dev", &[], &owned(&["DEV1", "DEV1", "DEV2", "DEV1"]));
        assert_eq!(r.iter().map(|r| r.env.as_str()).collect::<Vec<_>>(), ["DEV1", "DEV2"]);
    }

    #[test]
    fn rows_are_labelled_with_the_environment_alone() {
        // No "Account — ENV" prefix: the environment name is the label.
        let r = rows("111", "Dev", &declared(&["DEV1", "DEV2"]), &[]);
        assert_eq!(r[0].label, "DEV1");
        assert_eq!(r[1].label, "DEV2");
        assert_eq!(r[0].account_label, "Dev", "the account is still tracked");
    }

    #[test]
    fn a_single_environment_is_labelled_with_its_own_name() {
        let r = rows("222", "Prod", &declared(&["PROD"]), &[]);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].label, "PROD");
        assert_eq!(r[0].env, "PROD");
    }

    #[test]
    fn untagged_account_falls_back_to_the_account_label() {
        // Nothing declared, nothing tagged: there is no environment name to
        // show, so the account name is the only thing left to call it.
        let r = rows("333", "Legacy", &[], &[]);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].label, "Legacy");
        assert_eq!(r[0].env, "", "an empty env means no environment filter");
    }

    #[test]
    fn blank_names_are_ignored_on_both_sides() {
        let r = rows("111", "Dev", &declared(&["  ", ""]), &owned(&["", "   "]));
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].env, "", "all names were blank — treat as untagged");
    }

    #[test]
    fn names_are_trimmed() {
        let r = rows("111", "Dev", &[], &owned(&["  DEV1  "]));
        assert_eq!(r[0].env, "DEV1");
    }

    #[test]
    fn excluded_environments_are_dropped() {
        // "Exclude Env" in the toolbar hid DEV2.
        let r = build(
            "111",
            "Dev",
            &declared(&["DEV1", "DEV2"]),
            &[],
            &owned(&["dev2"]),
        );
        assert_eq!(r.iter().map(|r| r.env.as_str()).collect::<Vec<_>>(), ["DEV1"]);
    }

    #[test]
    fn exclusion_matches_case_insensitively() {
        // hidden_envs stores lowercase; accounts.json declares upper.
        let r = build("111", "Dev", &declared(&["DEV1"]), &[], &owned(&["  dev1 "]));
        assert!(r.is_empty());
    }

    #[test]
    fn an_account_with_every_environment_excluded_contributes_nothing() {
        // It must NOT collapse to an unfiltered whole-account row, which would
        // re-expose the bastions the user just hid.
        let r = build(
            "111",
            "Dev",
            &declared(&["DEV1", "DEV2"]),
            &[],
            &owned(&["DEV1", "DEV2"]),
        );
        assert!(r.is_empty(), "the account drops out of the dropdown entirely");
    }

    #[test]
    fn exclusion_applies_to_discovered_environments_too() {
        let r = build("111", "Dev", &[], &owned(&["DEV1", "DEV2"]), &owned(&["dev1"]));
        assert_eq!(r.iter().map(|r| r.env.as_str()).collect::<Vec<_>>(), ["DEV2"]);
    }

    #[test]
    fn an_untagged_account_is_unaffected_by_exclusions() {
        // It has no environment name for Exclude Env to match against.
        let r = build("333", "Legacy", &[], &[], &owned(&["dev1", "legacy"]));
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].label, "Legacy");
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
        let r = rows("111", "Dev", &declared(&["DEV1", "DEV2"]), &[]);
        assert_eq!(r[0].key(), ("111".to_string(), "DEV1".to_string()));
        assert_ne!(r[0].key(), r[1].key());
    }
}

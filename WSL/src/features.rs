//! Compiled-in build-time feature gates.
//!
//! The JSON in `assets/features.json` is baked into the binary at build
//! time (like `assets/accounts.json`). An admin edits that file and
//! rebuilds to change what the app exposes — end users cannot flip these
//! at runtime. This is intentional for destructive actions such as
//! deleting a user.

use serde::Deserialize;

/// Compiled-in feature flags from `assets/features.json`.
const BUNDLED_FEATURES: &str = include_str!("../assets/features.json");

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct Features {
    /// Expose the destructive "delete_user.sh" entry in the Scripts menu.
    pub allow_delete_user: bool,
    /// Substring filter for the primary-bastion dropdown in the Scripts
    /// dialog: only instances whose name or id contains this (case-
    /// insensitive) are shown. Empty means show all.
    pub primary_bastion_filter: String,
    /// Substring filter for the secondary-bastion dropdown.
    pub secondary_bastion_filter: String,
}

impl Default for Features {
    fn default() -> Self {
        Self {
            allow_delete_user: false,
            primary_bastion_filter: "bastion".to_string(),
            secondary_bastion_filter: "bastion".to_string(),
        }
    }
}

/// Parse the bundled feature flags, falling back to all-off if the JSON is
/// malformed (fail closed — never enable a gated action by accident).
pub fn load() -> Features {
    serde_json::from_str(BUNDLED_FEATURES).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_features_parse() {
        // The shipped file must always parse; a typo there would silently
        // disable every gate.
        let parsed: std::result::Result<Features, _> =
            serde_json::from_str(BUNDLED_FEATURES);
        assert!(parsed.is_ok(), "assets/features.json failed to parse");
    }

    #[test]
    fn unknown_keys_are_ignored() {
        // Extra keys (like _comment) must not break parsing.
        let f: Features =
            serde_json::from_str(r#"{"_comment":"hi","allow_delete_user":true}"#)
                .expect("should parse");
        assert!(f.allow_delete_user);
    }

    #[test]
    fn missing_key_defaults_off() {
        let f: Features = serde_json::from_str("{}").expect("should parse");
        assert!(!f.allow_delete_user);
    }

    #[test]
    fn bastion_filters_default_to_bastion() {
        // Missing filter keys fall back to "bastion".
        let f: Features =
            serde_json::from_str(r#"{"allow_delete_user":true}"#).expect("should parse");
        assert_eq!(f.primary_bastion_filter, "bastion");
        assert_eq!(f.secondary_bastion_filter, "bastion");
    }

    #[test]
    fn bastion_filters_can_be_overridden() {
        let f: Features = serde_json::from_str(
            r#"{"primary_bastion_filter":"prod-a","secondary_bastion_filter":"prod-b"}"#,
        )
        .expect("should parse");
        assert_eq!(f.primary_bastion_filter, "prod-a");
        assert_eq!(f.secondary_bastion_filter, "prod-b");
    }
}

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
    /// Usernames that delete_user must never remove (case-insensitive).
    /// Defaults to a safe built-in set; edit in features.json to extend.
    pub protected_users: Vec<String>,
    /// On-call Alerts button: Jira site + who may see it.
    pub alerts: AlertsFeature,
    /// "Add Script" (personal scripts + git PAT): who may use it.
    pub personal_scripts: PersonalScriptsFeature,
}

/// The `personal_scripts` section of `assets/features.json`.
///
/// Users on `allowed_users` get the "Add Script" entry in the Scripts menu
/// and are prompted once for a git personal access token, which Prep
/// Terminal then exports to the remote shell as `GIT_PAT`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct PersonalScriptsFeature {
    /// OS usernames allowed to add personal scripts (case-insensitive).
    /// `["*"]` for everyone; an empty list (the shipped default) for nobody.
    pub allowed_users: Vec<String>,
}

impl PersonalScriptsFeature {
    /// True when `user` may add personal scripts / is prompted for a PAT.
    pub fn is_allowed_user(&self, user: &str) -> bool {
        let u = user.trim().to_ascii_lowercase();
        if u.is_empty() {
            return false;
        }
        self.allowed_users.iter().any(|a| {
            let a = a.trim();
            a == "*" || a.eq_ignore_ascii_case(&u)
        })
    }
}

/// The `alerts` section of `assets/features.json`.
///
/// `cloud_id` / `email` identify the Jira Service Management site to query.
/// The token is deliberately **not** expected to live in this file (it is
/// checked into git) — leave it empty and set `JIRA_TOKEN` in the environment;
/// see [`AlertsFeature::token`].
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct AlertsFeature {
    /// Atlassian cloud id for the JSM site (the UUID in the API path).
    pub cloud_id: String,
    /// Atlassian account email used for API basic auth.
    pub email: String,
    /// API token. Prefer leaving this empty and exporting `JIRA_TOKEN`:
    /// features.json is committed, so a token here is a token in git.
    /// A non-empty value is used only when `JIRA_TOKEN` is unset.
    pub token: String,
    /// OS usernames allowed to see the Alerts button (case-insensitive).
    /// `["*"]` shows it to everyone; an empty list hides it from everyone.
    pub allowed_users: Vec<String>,
}

impl AlertsFeature {
    /// True when `user` may see the Alerts button.
    pub fn is_allowed_user(&self, user: &str) -> bool {
        let u = user.trim().to_ascii_lowercase();
        if u.is_empty() {
            return false;
        }
        self.allowed_users.iter().any(|a| {
            let a = a.trim();
            a == "*" || a.eq_ignore_ascii_case(&u)
        })
    }

    /// The API token actually used: `JIRA_TOKEN` wins over features.json so a
    /// user can supply their own without a rebuild.
    pub fn resolved_token(&self) -> String {
        std::env::var("JIRA_TOKEN")
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| self.token.trim().to_string())
    }
}

/// The OS username of the person running the app: `USERNAME` on Windows,
/// `USER` elsewhere. Empty when neither is set.
pub fn current_os_user() -> String {
    let key = if cfg!(target_os = "windows") {
        "USERNAME"
    } else {
        "USER"
    };
    std::env::var(key).unwrap_or_default().trim().to_string()
}

/// Built-in never-delete list (system / critical accounts).
fn default_protected_users() -> Vec<String> {
    [
        "root",
        "ec2-user",
        "ssm-user",
        "ssm-agent",
        "ubuntu",
        "centos",
        "admin",
        "sshd",
        "nobody",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

impl Default for Features {
    fn default() -> Self {
        Self {
            allow_delete_user: false,
            primary_bastion_filter: "bastion".to_string(),
            secondary_bastion_filter: "bastion".to_string(),
            protected_users: default_protected_users(),
            alerts: AlertsFeature::default(),
            personal_scripts: PersonalScriptsFeature::default(),
        }
    }
}

impl Features {
    /// Auth for the alerts API, with the token resolved from the environment
    /// when set. Check `is_complete()` before using.
    pub fn alerts_auth(&self) -> crate::alerts::AlertsAuth {
        crate::alerts::AlertsAuth {
            email: self.alerts.email.trim().to_string(),
            token: self.alerts.resolved_token(),
            cloud_id: self.alerts.cloud_id.trim().to_string(),
        }
    }

    /// True when the Alerts button should be shown to `user` — the site is
    /// configured *and* the user is on the allow-list. Fails closed.
    pub fn alerts_visible_for(&self, user: &str) -> bool {
        !self.alerts.cloud_id.trim().is_empty()
            && !self.alerts.email.trim().is_empty()
            && self.alerts.is_allowed_user(user)
    }

    /// True when `user` may add personal scripts (and so is prompted for a
    /// git PAT on launch). Fails closed — the shipped allow-list is empty.
    pub fn personal_scripts_visible_for(&self, user: &str) -> bool {
        self.personal_scripts.is_allowed_user(user)
    }

    /// True when `name` is on the protected never-delete list (case- and
    /// whitespace-insensitive).
    pub fn is_protected_user(&self, name: &str) -> bool {
        let n = name.trim().to_ascii_lowercase();
        self.protected_users
            .iter()
            .any(|p| p.trim().to_ascii_lowercase() == n)
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

    #[test]
    fn protected_users_default_and_case_insensitive() {
        let f = Features::default();
        assert!(f.is_protected_user("ec2-user"));
        assert!(f.is_protected_user("SSM-User"));
        assert!(f.is_protected_user(" root "));
        assert!(!f.is_protected_user("test.user"));
    }

    #[test]
    fn protected_users_missing_key_uses_defaults() {
        let f: Features = serde_json::from_str("{}").expect("should parse");
        assert!(f.is_protected_user("ssm-user"));
    }

    fn alerts_features(allowed: &str) -> Features {
        serde_json::from_str(&format!(
            r#"{{"alerts":{{"cloud_id":"cid","email":"a@b.c","allowed_users":{allowed}}}}}"#
        ))
        .expect("should parse")
    }

    #[test]
    fn alerts_hidden_when_user_not_on_allow_list() {
        let f = alerts_features(r#"["bconrad"]"#);
        assert!(f.alerts_visible_for("bconrad"));
        assert!(f.alerts_visible_for("BConrad")); // case-insensitive
        assert!(!f.alerts_visible_for("someone.else"));
        assert!(!f.alerts_visible_for("")); // unknown user → hidden
    }

    #[test]
    fn alerts_wildcard_shows_to_everyone() {
        let f = alerts_features(r#"["*"]"#);
        assert!(f.alerts_visible_for("anyone"));
    }

    #[test]
    fn alerts_empty_allow_list_hides_from_everyone() {
        let f = alerts_features("[]");
        assert!(!f.alerts_visible_for("bconrad"));
    }

    #[test]
    fn alerts_hidden_when_site_not_configured() {
        // On the allow-list, but no cloud_id/email compiled in → nothing to
        // query, so no button.
        let f: Features =
            serde_json::from_str(r#"{"alerts":{"allowed_users":["*"]}}"#).expect("should parse");
        assert!(!f.alerts_visible_for("bconrad"));
    }

    #[test]
    fn personal_scripts_follow_the_allow_list() {
        let f: Features =
            serde_json::from_str(r#"{"personal_scripts":{"allowed_users":["bconrad"]}}"#)
                .expect("should parse");
        assert!(f.personal_scripts_visible_for("bconrad"));
        assert!(f.personal_scripts_visible_for("BConrad")); // case-insensitive
        assert!(!f.personal_scripts_visible_for("someone.else"));
        assert!(!f.personal_scripts_visible_for("")); // unknown user → hidden
    }

    #[test]
    fn personal_scripts_wildcard_and_empty_list() {
        let all: Features =
            serde_json::from_str(r#"{"personal_scripts":{"allowed_users":["*"]}}"#)
                .expect("should parse");
        assert!(all.personal_scripts_visible_for("anyone"));

        let none: Features = serde_json::from_str(r#"{"personal_scripts":{"allowed_users":[]}}"#)
            .expect("should parse");
        assert!(!none.personal_scripts_visible_for("bconrad"));
    }

    /// Fails closed: a build without the section shows nobody the feature.
    #[test]
    fn personal_scripts_missing_section_defaults_off() {
        let f: Features = serde_json::from_str("{}").expect("should parse");
        assert!(!f.personal_scripts_visible_for("bconrad"));
    }

    #[test]
    fn alerts_missing_section_defaults_off() {
        let f: Features = serde_json::from_str("{}").expect("should parse");
        assert!(!f.alerts_visible_for("bconrad"));
        assert!(!f.alerts_auth().is_complete());
    }
}

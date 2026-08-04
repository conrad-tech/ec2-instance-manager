//! Compiled-in build-time feature gates.
//!
//! The JSON in `assets/features.json` is baked into the binary at build
//! time (like `assets/accounts.json`). An admin edits that file and
//! rebuilds to change what the app exposes — end users cannot flip these
//! at runtime. This is intentional for destructive actions such as
//! deleting a user.

use serde::Deserialize;

/// Compiled-in feature flags from `assets/features.json`, obfuscated at build
/// time (see [`crate::obf_core`]) so the gate config, bastion filters and Jira
/// alert settings do not sit in the binary as readable JSON. `build.rs` writes
/// the scrambled blob; [`bundled_features`] unscrambles it.
const BUNDLED_FEATURES_OBF: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/features.json.obf"));

fn bundled_features() -> String {
    let plain = crate::obf_core::obf_transform(BUNDLED_FEATURES_OBF);
    String::from_utf8(plain).expect("bundled features.json is valid UTF-8")
}

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
    /// Git PAT + credential setup + default hotkey scripts: who gets them.
    pub personal_scripts: PersonalScriptsFeature,
    /// "Vault IAM Access" entry in the Scripts menu: who may see it.
    pub vault_iam: VaultIamFeature,
    /// Outlook "access email" automation config (Windows only). Controls
    /// how the post-create email is encrypted and whether it may auto-send.
    pub access_email: AccessEmailConfig,
}

/// Shared allow-list match used by every `allowed_users` gate: OS username,
/// trimmed and case-insensitive, with `"*"` meaning everyone. An empty user
/// never matches, so a missing `USER`/`USERNAME` fails closed.
fn user_in_list(allowed: &[String], user: &str) -> bool {
    let u = user.trim();
    if u.is_empty() {
        return false;
    }
    allowed.iter().any(|a| {
        let a = a.trim();
        a == "*" || a.eq_ignore_ascii_case(u)
    })
}

/// The `vault_iam` section of `assets/features.json`.
///
/// Gates the **Vault IAM Access** entry in the Scripts menu, which writes a
/// Vault policy and an AWS-auth role from a bastion. The shipped file sets
/// `["*"]` (everyone); the `Default` below is empty so a malformed features.json
/// hides the entry rather than exposing it.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct VaultIamFeature {
    /// OS usernames allowed to see the entry (case-insensitive).
    /// `["*"]` for everyone, an empty list for nobody.
    pub allowed_users: Vec<String>,
    /// OS usernames additionally allowed to see the destructive **Vault IAM
    /// Delete** entry, which removes a role and its policy. Separate from
    /// `allowed_users` and empty by default, so creating does not imply
    /// deleting — the same stance `allow_delete_user` takes.
    pub delete_allowed_users: Vec<String>,
}

impl VaultIamFeature {
    /// True when `user` may see the Vault IAM Access entry.
    pub fn is_allowed_user(&self, user: &str) -> bool {
        user_in_list(&self.allowed_users, user)
    }

    /// True when `user` may see the Vault IAM Delete entry. Being on the
    /// create list is not enough — delete has its own list.
    pub fn is_delete_allowed_user(&self, user: &str) -> bool {
        user_in_list(&self.delete_allowed_users, user)
    }
}

/// A hardcoded script (from `assets/features.json`) handed to allow-listed
/// users — e.g. the Ctrl+1 "re-clone the repo" script. Not editable in the
/// UI; users off the list bind their own instead.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct DefaultScript {
    pub name: String,
    /// Canonical hotkey string, e.g. "Ctrl+1". Empty for menu-only.
    pub hotkey: String,
    /// Shell body, pasted into the focused connection tab when run.
    pub body: String,
}

/// The `personal_scripts` section of `assets/features.json`.
///
/// The *personal scripts* feature ("Add Script") is available to everyone;
/// this section instead gates the git integration: users on `allowed_users`
/// are prompted once for a git personal access token, get git's credential
/// store populated by Prep Terminal (scoped to `git_host`), and receive the
/// hardcoded `default_scripts` (e.g. bound to Ctrl+1).
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct PersonalScriptsFeature {
    /// OS usernames allowed the git PAT + default scripts (case-insensitive).
    /// `["*"]` for everyone; an empty list (the shipped default) for nobody.
    pub allowed_users: Vec<String>,
    /// Host the cached PAT authenticates to (git credential store scope).
    pub git_host: String,
    /// Hardcoded scripts (with hotkeys) handed to allow-listed users.
    pub default_scripts: Vec<DefaultScript>,
}

impl Default for PersonalScriptsFeature {
    fn default() -> Self {
        Self {
            allowed_users: Vec::new(),
            git_host: default_git_host(),
            default_scripts: Vec::new(),
        }
    }
}

/// Host used for the git credential store when features.json omits one.
fn default_git_host() -> String {
    "github.com".to_string()
}

impl PersonalScriptsFeature {
    /// True when `user` gets the git PAT + credential setup + default scripts.
    pub fn is_allowed_user(&self, user: &str) -> bool {
        user_in_list(&self.allowed_users, user)
    }

    /// Host the credential store is scoped to, never empty.
    pub fn host(&self) -> String {
        let h = self.git_host.trim();
        if h.is_empty() {
            default_git_host()
        } else {
            h.to_string()
        }
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
        user_in_list(&self.allowed_users, user)
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

/// Config for the post-create Outlook access email (see
/// `assets/scripts/send_access_email.ps1`). The encryption values are
/// discovered per-tenant with `outlook_verification.ps1` and pasted here.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct AccessEmailConfig {
    /// Master switch. When false the GUI never launches the email.
    pub enabled: bool,
    /// Run `send_access_email.ps1` automatically once a create finishes with a
    /// saved PEM, instead of only offering the "Send Email Command" menu.
    /// Defaults to true — this is a convenience, not a privilege gate, so it
    /// does not follow the fail-closed pattern the `allowed_users` lists use.
    /// Set false to leave the menu as the only route.
    pub auto_run: bool,
    /// The organization's own mail domain, e.g. "xyz.com". A resolved
    /// recipient's address must sit in this domain before anything is sent
    /// unattended: Outlook's `Resolve()` also matches the local Contacts folder
    /// and the autocomplete cache, so a stale personal entry for the same name
    /// would otherwise be mailed a private key. Blank disables the check.
    pub email_domain: String,
    /// Shape the recipient's address must have, checked against the username
    /// before anything is sent unattended. `"flast"` means first initial +
    /// surname with an optional numeric suffix, so `john.smith` accepts
    /// `jsmith@` or `jsmith2@` but not `johnsmith@`. Blank disables the check.
    ///
    /// This catches what the domain check cannot: an in-domain address that
    /// simply belongs to a different person with a similar name.
    pub email_local_format: String,
    /// RMS/IRM template GUID to apply for encryption (tenant-specific).
    /// Empty disables the template path.
    pub encrypt_template_guid: String,
    /// `MailItem.Permission` value to set (e.g. 3 for the discovered
    /// template, 2 = Do Not Forward). 0 = don't set via Permission.
    pub encrypt_permission: i64,
    /// `MailItem.PermissionService` value (1 = olWindows). Required
    /// alongside a template GUID for headless encryption. 0 = don't set.
    pub encrypt_permission_service: i64,
    /// S/MIME security flag (MAPI 0x6E010003): 1 = encrypt contents.
    /// 0 = not used. Only relevant when there's no template GUID.
    pub encrypt_smime_flag: i64,
    /// Quick Access Toolbar Encrypt shortcut for the visible fallback
    /// path (SendKeys syntax; Alt+6 = "%6").
    pub encrypt_sendkeys: String,
}

impl Default for AccessEmailConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_run: true,
            email_domain: String::new(),
            email_local_format: String::new(),
            encrypt_template_guid: String::new(),
            encrypt_permission: 0,
            encrypt_permission_service: 0,
            encrypt_smime_flag: 0,
            encrypt_sendkeys: "%6".to_string(),
        }
    }
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
            vault_iam: VaultIamFeature::default(),
            access_email: AccessEmailConfig::default(),
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

    /// True when `user` gets the git integration — the PAT prompt on launch,
    /// the credential store populated by Prep Terminal, and the hardcoded
    /// default scripts. Fails closed — the shipped allow-list is empty.
    /// (The plain "Add Script" feature is available to everyone regardless.)
    pub fn git_scripts_enabled_for(&self, user: &str) -> bool {
        self.personal_scripts.is_allowed_user(user)
    }

    /// The hardcoded default scripts for `user` — empty unless they are on
    /// the allow-list.
    pub fn default_scripts_for(&self, user: &str) -> Vec<DefaultScript> {
        if self.git_scripts_enabled_for(user) {
            self.personal_scripts.default_scripts.clone()
        } else {
            Vec::new()
        }
    }

    /// True when the **Vault IAM Access** entry should be shown to `user`.
    /// Fails closed: a malformed features.json yields an empty allow-list.
    pub fn vault_iam_enabled_for(&self, user: &str) -> bool {
        self.vault_iam.is_allowed_user(user)
    }

    /// True when the destructive **Vault IAM Delete** entry should be shown to
    /// `user`. Requires both lists: you cannot be handed the delete without the
    /// create it undoes. Fails closed; the shipped delete list is empty.
    pub fn vault_iam_delete_enabled_for(&self, user: &str) -> bool {
        self.vault_iam.is_allowed_user(user)
            && self.vault_iam.is_delete_allowed_user(user)
    }

    /// Host the git credential store is scoped to.
    pub fn git_host(&self) -> String {
        self.personal_scripts.host()
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
    serde_json::from_str(&bundled_features()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_features_parse() {
        // The shipped file must always parse; a typo there would silently
        // disable every gate.
        // Also exercises the build-time obfuscation round-trip.
        let parsed: std::result::Result<Features, _> =
            serde_json::from_str(&bundled_features());
        assert!(parsed.is_ok(), "assets/features.json failed to parse");
    }

    #[test]
    fn access_email_domain_defaults_to_blank() {
        assert_eq!(AccessEmailConfig::default().email_domain, "");
    }

    #[test]
    fn access_email_domain_is_read_from_json() {
        let cfg: AccessEmailConfig =
            serde_json::from_str(r#"{"email_domain":"xyz.com"}"#).expect("parses");
        assert_eq!(cfg.email_domain, "xyz.com");
        // Unlisted fields still fall back to the Default impl.
        assert!(cfg.enabled);
    }

    #[test]
    fn access_email_auto_run_defaults_on_and_can_be_disabled() {
        assert!(AccessEmailConfig::default().auto_run);
        let cfg: AccessEmailConfig =
            serde_json::from_str(r#"{"auto_run":false}"#).expect("parses");
        assert!(!cfg.auto_run);
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

    fn vault_iam_features(allowed: &str) -> Features {
        serde_json::from_str(&format!(
            r#"{{"vault_iam":{{"allowed_users":{allowed}}}}}"#
        ))
        .expect("should parse")
    }

    #[test]
    fn vault_iam_wildcard_shows_to_everyone() {
        // The shipped features.json setting.
        let f = vault_iam_features(r#"["*"]"#);
        assert!(f.vault_iam_enabled_for("anyone"));
    }

    #[test]
    fn vault_iam_follows_a_named_allow_list() {
        let f = vault_iam_features(r#"["bconrad"]"#);
        assert!(f.vault_iam_enabled_for("bconrad"));
        assert!(f.vault_iam_enabled_for(" BConrad ")); // trimmed, case-insensitive
        assert!(!f.vault_iam_enabled_for("someone.else"));
    }

    #[test]
    fn vault_iam_empty_allow_list_hides_from_everyone() {
        assert!(!vault_iam_features("[]").vault_iam_enabled_for("bconrad"));
    }

    #[test]
    fn vault_iam_fails_closed_when_absent_or_user_unknown() {
        // A features.json with no vault_iam section at all, and the case where
        // neither USER nor USERNAME is set.
        let f: Features = serde_json::from_str("{}").expect("should parse");
        assert!(!f.vault_iam_enabled_for("bconrad"));
        assert!(!vault_iam_features(r#"["*"]"#).vault_iam_enabled_for(""));
        // A malformed file falls back to Default, which must also be closed.
        assert!(!Features::default().vault_iam_enabled_for("bconrad"));
    }

    #[test]
    fn shipped_features_show_vault_iam_to_everyone() {
        // Guards the shipped assets/features.json against an accidental edit
        // that would hide the entry from all users.
        let f = load();
        assert!(
            f.vault_iam_enabled_for("any.user"),
            "assets/features.json should ship vault_iam.allowed_users = [\"*\"]"
        );
    }

    #[test]
    fn vault_iam_delete_is_hidden_by_default() {
        // Everyone sees create in the shipped file; nobody sees delete.
        let f = load();
        assert!(f.vault_iam_enabled_for("any.user"));
        assert!(
            !f.vault_iam_delete_enabled_for("any.user"),
            "assets/features.json must ship an empty delete_allowed_users"
        );
    }

    #[test]
    fn vault_iam_delete_follows_its_own_list() {
        let f: Features = serde_json::from_str(
            r#"{"vault_iam":{"allowed_users":["*"],"delete_allowed_users":["bconrad"]}}"#,
        )
        .expect("should parse");
        assert!(f.vault_iam_delete_enabled_for("bconrad"));
        assert!(f.vault_iam_delete_enabled_for("BConrad"));
        assert!(
            !f.vault_iam_delete_enabled_for("someone.else"),
            "the create wildcard must not grant delete"
        );
    }

    #[test]
    fn vault_iam_delete_requires_create_access_too() {
        // On the delete list but not the create list: nothing to delete from.
        let f: Features = serde_json::from_str(
            r#"{"vault_iam":{"allowed_users":[],"delete_allowed_users":["bconrad"]}}"#,
        )
        .expect("should parse");
        assert!(!f.vault_iam_delete_enabled_for("bconrad"));
    }

    #[test]
    fn vault_iam_delete_fails_closed() {
        assert!(!Features::default().vault_iam_delete_enabled_for("bconrad"));
        let f: Features = serde_json::from_str("{}").expect("should parse");
        assert!(!f.vault_iam_delete_enabled_for("bconrad"));
    }

    #[test]
    fn git_scripts_follow_the_allow_list() {
        let f: Features =
            serde_json::from_str(r#"{"personal_scripts":{"allowed_users":["bconrad"]}}"#)
                .expect("should parse");
        assert!(f.git_scripts_enabled_for("bconrad"));
        assert!(f.git_scripts_enabled_for("BConrad")); // case-insensitive
        assert!(!f.git_scripts_enabled_for("someone.else"));
        assert!(!f.git_scripts_enabled_for("")); // unknown user → off
    }

    #[test]
    fn git_scripts_wildcard_and_empty_list() {
        let all: Features =
            serde_json::from_str(r#"{"personal_scripts":{"allowed_users":["*"]}}"#)
                .expect("should parse");
        assert!(all.git_scripts_enabled_for("anyone"));

        let none: Features = serde_json::from_str(r#"{"personal_scripts":{"allowed_users":[]}}"#)
            .expect("should parse");
        assert!(!none.git_scripts_enabled_for("bconrad"));
    }

    /// Fails closed: a build without the section gives nobody the git setup.
    #[test]
    fn git_scripts_missing_section_defaults_off() {
        let f: Features = serde_json::from_str("{}").expect("should parse");
        assert!(!f.git_scripts_enabled_for("bconrad"));
        assert_eq!(f.git_host(), "github.com");
    }

    #[test]
    fn git_host_defaults_and_overrides() {
        let f: Features = serde_json::from_str("{}").expect("should parse");
        assert_eq!(f.git_host(), "github.com");
        let f: Features =
            serde_json::from_str(r#"{"personal_scripts":{"git_host":"git.internal"}}"#)
                .expect("should parse");
        assert_eq!(f.git_host(), "git.internal");
        // Blank falls back rather than yielding an empty host.
        let f: Features =
            serde_json::from_str(r#"{"personal_scripts":{"git_host":"  "}}"#).expect("should parse");
        assert_eq!(f.git_host(), "github.com");
    }

    /// Default scripts reach allow-listed users only.
    #[test]
    fn default_scripts_gated_on_the_allow_list() {
        let f: Features = serde_json::from_str(
            r#"{"personal_scripts":{"allowed_users":["bconrad"],
                "default_scripts":[{"name":"Re-clone","hotkey":"Ctrl+1","body":"echo hi"}]}}"#,
        )
        .expect("should parse");
        let scripts = f.default_scripts_for("bconrad");
        assert_eq!(scripts.len(), 1);
        assert_eq!(scripts[0].hotkey, "Ctrl+1");
        assert!(f.default_scripts_for("someone.else").is_empty());
    }

    #[test]
    fn alerts_missing_section_defaults_off() {
        let f: Features = serde_json::from_str("{}").expect("should parse");
        assert!(!f.alerts_visible_for("bconrad"));
        assert!(!f.alerts_auth().is_complete());
    }
}

//! Compiled-in build-time feature gates.
//!
//! The JSON in `assets/features.json` is baked into the binary at build
//! time (like `assets/accounts.json`). An admin edits that file and
//! rebuilds to change what the app exposes — end users cannot flip these
//! at runtime. This is intentional for destructive actions such as
//! deleting a user.

use serde::{Deserialize, Deserializer};

/// Accept a JSON array, a single string, or a comma-separated string, and
/// return the trimmed non-empty entries.
///
/// Config that started as one value and grew into a list is a common source of
/// churn; taking both spellings means an admin's existing file keeps working
/// and `"a.com, b.com"` does the obvious thing instead of silently becoming one
/// domain nothing can match.
fn string_or_list<'de, D>(deserializer: D) -> std::result::Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }

    let raw = match OneOrMany::deserialize(deserializer)? {
        OneOrMany::One(s) => s.split(',').map(str::to_string).collect(),
        OneOrMany::Many(v) => v,
    };
    Ok(raw
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect())
}

/// Same as [`string_or_list`], but lower-cases every entry.
///
/// For values compared against an email address, which is case-insensitive:
/// the address is lower-cased before the comparison, so a `".CW"` written in
/// features.json would otherwise become a suffix that can never match.
fn string_or_list_lower<'de, D>(deserializer: D) -> std::result::Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(string_or_list(deserializer)?
        .into_iter()
        .map(|s| s.to_lowercase())
        .collect())
}

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
    /// "Bastion User Sync" entry in the Scripts menu: who may see it.
    pub user_sync: UserSyncFeature,
    /// "Send test escalation" action: who may see it.
    pub on_call_test: OnCallTestFeature,
    /// Outlook "access email" automation config (Windows only). Controls
    /// how the post-create email is encrypted and whether it may auto-send.
    pub access_email: AccessEmailConfig,
    /// Automatic `fed up` credential refresh: who gets it and how it is timed.
    pub fed_auth: FedAuthFeature,
    /// Unattended reaper remediation: who may run it, and every tunable.
    pub reaper: ReaperFeature,
}

/// The `fed_auth` section of `assets/features.json`.
///
/// Gates the automatic `fed up` refresh: the app runs the corporate
/// federation CLI when the cached credentials **expire**, retrying a failure
/// for 10 minutes before standing down.
///
/// Expiry-driven rather than on a timer: `fed_expire` in `~/.aws/credentials`
/// already says exactly when the credentials die, so there is nothing to gain
/// from asking on a clock. It also makes the feature testable — backdate
/// `fed_expire` and the run fires within a second, because the credentials
/// file is already watched by mtime.
///
/// It ships disabled with an empty allow-list and needs both, so a stray
/// `["*"]` cannot switch it on for a whole site.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct FedAuthFeature {
    /// Master switch. Off in the shipped file.
    pub enabled: bool,
    /// OS usernames allowed to use it (case-insensitive). `["*"]` means
    /// everyone, an empty list means nobody. Shipped empty.
    pub allowed_users: Vec<String>,
    /// The command to run, as argv. Empty falls back to `["fed", "up"]`.
    pub command: Vec<String>,
    /// Seconds between attempts while retrying a failure. 0 uses the default
    /// (120). Also the minimum gap between any two attempts, so an expiry
    /// that `fed up` does not clear cannot spin.
    pub retry_interval_secs: u64,
    /// Seconds to keep retrying before giving up. 0 uses the default (600).
    pub retry_window_secs: u64,
    /// Seconds between attempts when the failure is only a missing account
    /// entitlement -- signed in fine, but `fed` reports no access to one
    /// account and gives a URL to request it. 0 uses the default (30).
    ///
    /// Much shorter than `retry_interval_secs` on purpose: the sign-in
    /// already worked, and an entitlement usually lands within a minute, so
    /// the general interval spends most of its time waiting for something
    /// that has already happened.
    pub access_retry_interval_secs: u64,
    /// Seconds one `fed up` may run before it is stopped. 0 uses the default
    /// (300).
    ///
    /// `fed up` blocks until the device authorization is approved, so a
    /// sign-in nobody finishes would otherwise pin the worker forever — and
    /// with it every later expiry, since only one run happens at a time.
    /// Long enough for a human to work through the Okta pages, short enough
    /// that an abandoned one recovers on its own.
    pub run_timeout_secs: u64,
    /// Drive the Okta sign-in automatically rather than handing the user the
    /// activation page: type the code, the username and the password from
    /// Windows Credential Manager, then confirm the MFA prompt.
    ///
    /// **Off in the shipped file, and separate from `enabled` on purpose.**
    /// Typing a domain password into an identity provider and clicking
    /// through MFA is behaviour an EDR scores as credential theft, and this
    /// app has a CrowdStrike quarantine in its history. With this off the
    /// feature still does the useful part — it runs `fed up` on expiry, opens
    /// the page and copies the code — and never synthesises a keystroke.
    ///
    /// When on it is the *primary* path, but not the only one: if the script
    /// is missing, no password is stored, or the focus guard trips, it falls
    /// back to opening the page and copying the code.
    ///
    /// Gated by [`Self::auto_sign_in_allowed_users`] as well, so a site can
    /// give everyone the refresh and only some people the automation.
    pub auto_sign_in: bool,
    /// OS usernames additionally allowed the automatic sign-in.
    ///
    /// Separate from `allowed_users` and empty by default, so being handed
    /// the refresh does not hand you the automation -- the same stance
    /// `vault_iam.delete_allowed_users` takes. A user must be on **both**
    /// lists, and `auto_sign_in` must be on, for a keystroke to be
    /// synthesised.
    pub auto_sign_in_allowed_users: Vec<String>,
    /// Window-title fragments the browser step accepts before it will type
    /// anything, matched case-insensitively; **any** one matching is enough.
    /// This is the focus guard: a mistimed keystroke must never put a domain
    /// password into whatever window happens to be frontmost.
    ///
    /// A list rather than one string because the sign-in walks several pages
    /// — activation, username, password, MFA — and they do not share a title.
    /// A single fragment that matches the first page will abort on a later
    /// one. Accepts a JSON array, one string, or a comma-separated string.
    /// Empty falls back to the shipped set.
    #[serde(deserialize_with = "string_or_list")]
    pub browser_title_match: Vec<String>,
    /// Arguments passed to Chrome ahead of the activation URL. Empty falls
    /// back to `--new-window`.
    ///
    /// That default is load-bearing: with Chrome already running, a bare
    /// `chrome.exe <url>` opens a *tab* in whatever window exists, which may
    /// be minimised, behind other windows, or on another virtual desktop.
    /// The sign-in then times out waiting for a foreground window, on a page
    /// that loaded fine but nowhere it could be typed into.
    ///
    /// A separate profile (`--user-data-dir=...`) is *not* the default:
    /// remembered-device cookies live in the profile, so a fresh one tends to
    /// invite more verification rather than less.
    #[serde(deserialize_with = "string_or_list")]
    pub chrome_args: Vec<String>,
    /// Close the browser window the sign-in drove once `fed up` reports the
    /// credentials renewed. Shipped on.
    ///
    /// Closes that **window** -- `WM_CLOSE` to the handle the script
    /// reported -- never the `chrome.exe` process, which with `--new-window`
    /// is shared with whatever else the user has open. Only ever a window the
    /// automation itself opened and drove; a sign-in the user finished by
    /// hand is left alone.
    pub close_browser_on_success: bool,
    /// Process names accepted at the **MFA confirm step only**, matched
    /// case-insensitively as an unanchored regex. Empty falls back to
    /// `chrome|okta`.
    ///
    /// Okta Verify owns that prompt in its own borderless, centred window --
    /// it looks like a Chrome popup but is a separate process, so the
    /// chrome-only focus guard refuses it and the sign-in stops one keystroke
    /// from done.
    ///
    /// Only this step is widened, and only because it types no secret: it is
    /// a bare Enter. Every step that types the code, the username or the
    /// password still requires Chrome. The title check applies throughout.
    #[serde(deserialize_with = "string_or_list")]
    pub mfa_process_match: Vec<String>,
    /// Windows Credential Manager target name holding the federation
    /// password. Empty falls back to `ec2-manager-fed`.
    ///
    /// The password is **never** stored by this app — the script reads the
    /// vault at the moment it needs it, so it can be set or revoked entirely
    /// outside the app with `cmdkey` or Control Panel.
    pub credential_target: String,
}

impl FedAuthFeature {
    /// True when `user` may use the automatic refresh. Requires both the
    /// master switch and the allow-list — see the note on [`FedAuthFeature`].
    pub fn is_allowed_user(&self, user: &str) -> bool {
        self.enabled && user_in_list(&self.allowed_users, user)
    }

    /// The command to run, falling back to `fed up`.
    pub fn resolved_command(&self) -> Vec<String> {
        let argv: Vec<String> = self
            .command
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if argv.is_empty() {
            vec!["fed".to_string(), "up".to_string()]
        } else {
            argv
        }
    }

    /// The retry timings, with 0 meaning "use the default".
    pub fn retry_policy(&self) -> crate::fed_auth::RetryPolicy {
        let d = crate::fed_auth::RetryPolicy::default();
        let or_default = |secs: u64, fallback: std::time::Duration| {
            if secs == 0 {
                fallback
            } else {
                std::time::Duration::from_secs(secs)
            }
        };
        crate::fed_auth::RetryPolicy {
            interval: or_default(self.retry_interval_secs, d.interval),
            window: or_default(self.retry_window_secs, d.window),
            access_interval: or_default(self.access_retry_interval_secs, d.access_interval),
        }
    }

    /// How long one `fed up` may run before it is stopped.
    pub fn run_timeout(&self) -> std::time::Duration {
        if self.run_timeout_secs == 0 {
            std::time::Duration::from_secs(300)
        } else {
            std::time::Duration::from_secs(self.run_timeout_secs)
        }
    }

    /// True when the automatic sign-in should run for `user`.
    ///
    /// Needs all three: the feature gate, the master switch, and this user on
    /// the sign-in list. Fails closed at every step -- see the note on
    /// [`Self::auto_sign_in_allowed_users`].
    pub fn auto_sign_in_for(&self, user: &str) -> bool {
        self.is_allowed_user(user)
            && self.auto_sign_in
            && user_in_list(&self.auto_sign_in_allowed_users, user)
    }

    /// The window-title fragments the browser step accepts, as the
    /// pipe-separated form the script parses. Never empty — that check is
    /// what keeps the password out of the wrong window, so an empty config
    /// falls back to the shipped set rather than disabling it.
    ///
    /// `|` is the separator because a window title will not contain one.
    pub fn resolved_title_match(&self) -> String {
        let joined = self
            .browser_title_match
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("|");
        if joined.is_empty() {
            "okta|sign in|verify".to_string()
        } else {
            joined
        }
    }

    /// Arguments handed to Chrome, space-joined for the script. Never empty
    /// -- see the note on [`Self::chrome_args`].
    pub fn resolved_chrome_args(&self) -> String {
        let joined = self
            .chrome_args
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if joined.is_empty() {
            "--new-window".to_string()
        } else {
            joined
        }
    }

    /// Process names the MFA confirm step accepts, pipe-joined for the
    /// script. Never empty -- a blank pattern would match every process and
    /// remove the check entirely.
    pub fn resolved_mfa_process_match(&self) -> String {
        let joined = self
            .mfa_process_match
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("|");
        if joined.is_empty() {
            "chrome|okta".to_string()
        } else {
            joined
        }
    }

    /// The Credential Manager target name, falling back to `ec2-manager-fed`.
    /// Never empty — an empty target would read whatever happens to be first
    /// in the vault.
    pub fn resolved_credential_target(&self) -> String {
        let t = self.credential_target.trim();
        if t.is_empty() {
            "ec2-manager-fed".to_string()
        } else {
            t.to_string()
        }
    }
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

/// The `user_sync` section of `assets/features.json`.
///
/// Gates **Bastion User Sync**, which compares the accounts on a pair of
/// bastions and can create the ones missing from either, plus install the
/// cron entry that keeps doing it. It reads and writes accounts on both
/// boxes, so it ships with an empty list — the same fail-closed stance as
/// `alerts`, `personal_scripts` and `vault_iam.delete_allowed_users`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct UserSyncFeature {
    /// OS usernames allowed to see the entry (case-insensitive).
    /// `["*"]` for everyone, an empty list for nobody. Shipped empty.
    pub allowed_users: Vec<String>,
}

impl UserSyncFeature {
    /// True when `user` may see the Bastion User Sync entry.
    pub fn is_allowed_user(&self, user: &str) -> bool {
        user_in_list(&self.allowed_users, user)
    }
}

/// The "Send test escalation" action, which mails a content-free coded
/// subject to the configured escalation mailbox.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct OnCallTestFeature {
    /// OS usernames allowed to see the action (case-insensitive).
    /// `["*"]` for everyone, an empty list for nobody. Shipped empty.
    pub allowed_users: Vec<String>,
}

impl OnCallTestFeature {
    /// True when `user` may see the test action.
    ///
    /// The action itself is also hidden when no recipient is configured — a
    /// visible control that cannot work invites a click and then explains
    /// nothing. That check belongs to the caller, which has the credential
    /// lookup; this gate answers only "is this user allowed".
    pub fn is_allowed_user(&self, user: &str) -> bool {
        user_in_list(&self.allowed_users, user)
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
/// Only the allow-list lives here. The cloud id, email and API token are
/// deliberately **not** fields of this struct — this file is committed, and
/// none of the five JSM/Opsgenie credential values may ever live in it. They
/// resolve environment → Windows Credential Manager via `jsm_auth::load_auth`
/// (see that module for the target/env-var pairs).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct AlertsFeature {
    /// OS usernames allowed to see the Alerts button (case-insensitive).
    /// `["*"]` shows it to everyone; an empty list hides it from everyone.
    pub allowed_users: Vec<String>,
}

impl AlertsFeature {
    /// True when `user` may see the Alerts button.
    pub fn is_allowed_user(&self, user: &str) -> bool {
        user_in_list(&self.allowed_users, user)
    }
}

/// The `reaper` section of `assets/features.json`.
///
/// Gates the unattended reaper auto-remediation. Being on this list is a
/// materially larger privilege than `alerts.allowed_users` (which only shows
/// a read-only window), so it is granted separately and ships empty.
///
/// **Single-entry by policy.** Nothing arbitrates two machines remediating
/// one alert; see `has_multiple_users`.
///
/// The JSM schedule id and the caller's Atlassian account id are **not**
/// fields here — this file is committed, and those two identifiers resolve
/// environment → Windows Credential Manager only, via
/// [`Self::resolved_schedule_id`] / [`Self::resolved_atlassian_account_id`]
/// (see `jsm_auth`).
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct ReaperFeature {
    /// Master switch. Off in the shipped file.
    pub enabled: bool,
    /// When true (the shipped default) the worker logs what it *would*
    /// remediate and touches nothing. Arming is a deliberate edit.
    pub dry_run: bool,
    /// OS usernames permitted to remediate. `"*"` is deliberately **not**
    /// honoured here.
    pub allowed_users: Vec<String>,
    /// Substring identifying a reaper alert in `extraProperties.alertname`.
    pub alertname_contains: String,
    /// Substring identifying a reaper alert in the `App:` tag. Secondary —
    /// the tag is frequently an unrendered template or absent entirely.
    pub app_contains: String,
    /// Substring identifying a reaper alert in `message`. Last resort.
    pub message_contains: String,
    /// Timeout for the one remote `send-command`.
    pub send_command_timeout_secs: u64,
    /// Minimum gap between two remediations of the same instance.
    pub cooldown_mins: u64,
    /// How long to wait for the alert to close, on call.
    pub stage2_on_call_mins: u64,
    /// How long to wait for the alert to close, off call.
    pub stage2_off_call_mins: u64,
    /// Feed poll interval.
    pub poll_secs: u64,
    /// Alerts pulled per poll.
    pub fetch_count: u32,
}

impl Default for ReaperFeature {
    fn default() -> Self {
        // Not `#[derive(Default)]`: `dry_run` must default to `true` (arming
        // is opt-in), which a derived Default — false for every bool — would
        // get backwards. This impl also backs `#[serde(default)]`, so a
        // features.json that omits `dry_run` still ships safe.
        Self {
            enabled: false,
            dry_run: true,
            allowed_users: Vec::new(),
            alertname_contains: String::new(),
            app_contains: String::new(),
            message_contains: String::new(),
            send_command_timeout_secs: 0,
            cooldown_mins: 0,
            stage2_on_call_mins: 0,
            stage2_off_call_mins: 0,
            poll_secs: 0,
            fetch_count: 0,
        }
    }
}

impl ReaperFeature {
    /// True when `user` may remediate. Unlike every other gate in this file,
    /// `"*"` does not match: a site-wide wildcard must not silently authorise
    /// unattended `compose down` on production.
    pub fn is_allowed_user(&self, user: &str) -> bool {
        let u = user.trim();
        if u.is_empty() {
            return false;
        }
        self.allowed_users
            .iter()
            .any(|a| a.trim() != "*" && a.trim().eq_ignore_ascii_case(u))
    }

    /// True when the list names more than one user — the caller logs a
    /// warning, since the race this creates is invisible in normal operation.
    pub fn has_multiple_users(&self) -> bool {
        self.allowed_users
            .iter()
            .filter(|a| !a.trim().is_empty())
            .count()
            > 1
    }

    /// The JSM schedule id, resolved environment → Windows Credential
    /// Manager, and stopping there — `assets/features.json` is committed, so
    /// it is never a source. Blank when unconfigured, which the on-call
    /// lookup reports as an error rather than as "not on call".
    pub fn resolved_schedule_id(&self) -> String {
        crate::jsm_auth::resolve_id(
            crate::jsm_auth::SCHEDULE_ID_ENV,
            crate::jsm_auth::SCHEDULE_ID_TARGET,
            "",
        )
    }

    /// The **Atlassian** account id — the identity matched against the JSM
    /// on-call response. Unrelated to any AWS account id, which is what
    /// `account` means everywhere else in this codebase. Same
    /// environment-then-Credential-Manager-only resolution as
    /// [`Self::resolved_schedule_id`].
    pub fn resolved_atlassian_account_id(&self) -> String {
        crate::jsm_auth::resolve_id(
            crate::jsm_auth::ATLASSIAN_ACCOUNT_ID_ENV,
            crate::jsm_auth::ATLASSIAN_ACCOUNT_ID_TARGET,
            "",
        )
    }

    /// Effective values for the tunables, so a zero in features.json (or a
    /// missing section) means "the default" rather than "never wait".
    pub fn poll_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(if self.poll_secs == 0 {
            30
        } else {
            self.poll_secs
        })
    }
    pub fn send_command_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(if self.send_command_timeout_secs == 0 {
            90
        } else {
            self.send_command_timeout_secs
        })
    }
    pub fn cooldown(&self) -> std::time::Duration {
        std::time::Duration::from_secs(
            60 * if self.cooldown_mins == 0 {
                30
            } else {
                self.cooldown_mins
            },
        )
    }
    pub fn alerts_per_poll(&self) -> u32 {
        if self.fetch_count == 0 {
            10
        } else {
            self.fetch_count
        }
    }
    /// Stage-2 window. Longer on call, because there the timeout triggers
    /// phone calls; off call it is one quiet message and telling the user
    /// sooner costs nothing.
    pub fn stage2_window(&self, on_call: bool) -> std::time::Duration {
        let mins = if on_call {
            if self.stage2_on_call_mins == 0 {
                15
            } else {
                self.stage2_on_call_mins
            }
        } else if self.stage2_off_call_mins == 0 {
            10
        } else {
            self.stage2_off_call_mins
        };
        std::time::Duration::from_secs(60 * mins)
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
    /// The organization's own mail domains, e.g. `["xyz.com", "old-xyz.com"]`.
    /// A resolved recipient's address must sit in one of them before anything
    /// is sent unattended: Outlook's `Resolve()` also matches the local
    /// Contacts folder and the autocomplete cache, so a stale personal entry
    /// for the same name would otherwise be mailed a private key.
    ///
    /// Staff routinely have mail on more than one domain (after a merger or a
    /// rebrand), and the Windows/AD domain is often a third, unrelated name —
    /// so this is a list, not the machine's domain.
    ///
    /// Accepts a JSON array, a single string, or a comma-separated string, and
    /// the older `email_domain` spelling. Empty disables the check.
    #[serde(alias = "email_domain", deserialize_with = "string_or_list")]
    pub email_domains: Vec<String>,
    /// Shape the recipient's address must have, checked against the username
    /// before anything is sent unattended. `"flast"` means first initial +
    /// surname with an optional numeric suffix, so `john.smith` accepts
    /// `jsmith@` or `jsmith2@` but not `johnsmith@`. Blank disables the check.
    ///
    /// This catches what the domain check cannot: an in-domain address that
    /// simply belongs to a different person with a similar name.
    pub email_local_format: String,
    /// Optional local-part markers accepted *alongside* the bare stem, e.g.
    /// `[".cw"]` so `test.user` matches `tuser@` **and** `tuser.cw@`.
    ///
    /// Sites that mark a class of staff in the address itself (contractors,
    /// contingent workers) would otherwise fail the shape check and never
    /// send. The list is also probed: `expected_email_locals` emits the bare
    /// and the suffixed form at each numeric rank, so the mailbox is *found*
    /// by address rather than falling back to display-name resolution.
    ///
    /// Taken literally rather than assuming a dot, so `-contractor` or `_ext`
    /// work without a code change. Accepts a JSON array, a single string, or a
    /// comma-separated string. Empty leaves the check exactly as it was: the
    /// stem plus an optional number.
    #[serde(deserialize_with = "string_or_list_lower")]
    pub email_local_suffixes: Vec<String>,
    /// Highest numeric suffix to probe when looking for the mailbox, e.g. 20
    /// tries `jsmith`, `jsmith2` … `jsmith20`. People who share a surname get a
    /// number, and there is no way to know in advance which one — so the script
    /// resolves each candidate address and requires exactly one real match.
    /// Below 2 probes only the bare stem.
    ///
    /// Set high enough to cover the real range: a `dli15@` turned up in
    /// practice, and a suffix beyond this silently falls back to resolving the
    /// display name, which cannot detect duplicates. The cost is one address
    /// lookup per candidate per domain, all local MAPI calls.
    pub email_local_max_suffix: u32,
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
            email_domains: Vec::new(),
            email_local_format: String::new(),
            email_local_suffixes: Vec::new(),
            email_local_max_suffix: 20,
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
            // Derived Default: an empty allow-list, which is the fail-closed
            // state this feature must land in.
            user_sync: UserSyncFeature::default(),
            // Derived Default: an empty allow-list, which is the fail-closed
            // state this feature must land in.
            on_call_test: OnCallTestFeature::default(),
            access_email: AccessEmailConfig::default(),
            // Derived Default: `enabled` false and an empty allow-list, which
            // is the fail-closed state this feature must land in.
            fed_auth: FedAuthFeature::default(),
            // `enabled` false, allow-list empty (fail-closed) and `dry_run`
            // true (arming is opt-in) — see the hand-written `Default` impl.
            reaper: ReaperFeature::default(),
        }
    }
}

impl Features {
    /// Auth for the alerts API, resolved environment → Windows Credential
    /// Manager (see `jsm_auth::load_auth`) — never from this struct, since
    /// none of the five JSM/Opsgenie credential values may live in the
    /// committed `assets/features.json`. Check `is_complete()` before using.
    ///
    /// **Do not call this per frame.** It is a `CredReadW` per field. Resolve
    /// it once (e.g. at startup) and cache the result — see how the GUI
    /// stores the answer once into `App::alerts_auth` and reuses it for both
    /// `alerts_visible_for` and every later alerts-API call.
    pub fn alerts_auth(&self) -> crate::alerts::AlertsAuth {
        crate::jsm_auth::load_auth()
    }

    /// True when the Alerts button should be shown to `user` — a complete
    /// set of credentials resolved *and* the user is on the allow-list.
    /// Fails closed.
    ///
    /// `auth` is a parameter, not a lookup, for the same reason
    /// `on_call_test_visible_for` takes `mailbox` as one: resolving it reads
    /// the Windows registry, so the caller resolves it once (see
    /// `Self::alerts_auth`) and passes the answer in rather than this being
    /// re-resolved on every call.
    pub fn alerts_visible_for(&self, user: &str, auth: &crate::alerts::AlertsAuth) -> bool {
        auth.is_complete() && self.alerts.is_allowed_user(user)
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

    /// True when the **Bastion User Sync** entry should be shown to `user`.
    /// Fails closed: a malformed features.json yields an empty allow-list,
    /// and the shipped file ships one.
    pub fn user_sync_enabled_for(&self, user: &str) -> bool {
        self.user_sync.is_allowed_user(user)
    }

    /// True when the **Send test escalation** action should be shown to
    /// `user`: the gate lets them in *and* a recipient actually resolves.
    ///
    /// Both halves, in one tested function, for the same reason
    /// `alerts_visible_for` combines its two — a visible control that cannot
    /// work is worse than an absent one: it invites a click, does nothing,
    /// and leaves the user unable to tell whether the failure was theirs or
    /// the app's.
    ///
    /// `mailbox` is a parameter rather than a lookup so this module stays
    /// clear of `jsm_auth` and the Windows credential store. The caller
    /// resolves it **once, at startup** and passes the answer in — see
    /// `jsm_auth::escalation_mailbox`, which is an environment read plus a
    /// `CredReadW` per call and must not be run per frame.
    ///
    /// Fails closed twice over: the shipped allow-list is empty, and a blank
    /// address is absent rather than a destination.
    pub fn on_call_test_visible_for(&self, user: &str, mailbox: Option<&str>) -> bool {
        self.on_call_test.is_allowed_user(user)
            && mailbox.is_some_and(|m| !m.trim().is_empty())
    }

    /// True when the automatic `fed up` login should run for `user`. Fails
    /// closed twice over: a malformed features.json leaves `enabled` false
    /// *and* the allow-list empty.
    pub fn fed_auth_enabled_for(&self, user: &str) -> bool {
        self.fed_auth.is_allowed_user(user)
    }

    /// Host the git credential store is scoped to.
    pub fn git_host(&self) -> String {
        self.personal_scripts.host()
    }

    /// True when `user` may run unattended reaper remediation. Fails closed:
    /// the feature must be switched on *and* the user named explicitly.
    pub fn reaper_enabled_for(&self, user: &str) -> bool {
        self.reaper.enabled && self.reaper.is_allowed_user(user)
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
    fn access_email_domains_default_to_empty() {
        assert!(AccessEmailConfig::default().email_domains.is_empty());
    }

    #[test]
    fn access_email_domains_accept_a_list() {
        // Staff can receive mail on more than one domain, so any of them is a
        // valid destination.
        let cfg: AccessEmailConfig =
            serde_json::from_str(r#"{"email_domains":["a.com","b.com"]}"#).expect("parses");
        assert_eq!(cfg.email_domains, vec!["a.com", "b.com"]);
        // Unlisted fields still fall back to the Default impl.
        assert!(cfg.enabled);
    }

    #[test]
    fn a_single_domain_string_is_still_accepted() {
        // The field started life as a plain string; existing files must keep
        // working, and one domain is the common case.
        let cfg: AccessEmailConfig =
            serde_json::from_str(r#"{"email_domain":"xyz.com"}"#).expect("parses");
        assert_eq!(cfg.email_domains, vec!["xyz.com"]);
    }

    #[test]
    fn a_comma_separated_domain_string_is_split() {
        let cfg: AccessEmailConfig =
            serde_json::from_str(r#"{"email_domains":"a.com, b.com"}"#).expect("parses");
        assert_eq!(cfg.email_domains, vec!["a.com", "b.com"]);
    }

    #[test]
    fn blank_and_empty_domain_entries_are_dropped() {
        // A blank value means "no check"; it must not become a domain named ""
        // that nothing can ever match.
        let cfg: AccessEmailConfig =
            serde_json::from_str(r#"{"email_domains":["a.com","","  "]}"#).expect("parses");
        assert_eq!(cfg.email_domains, vec!["a.com"]);
        let blank: AccessEmailConfig =
            serde_json::from_str(r#"{"email_domains":""}"#).expect("parses");
        assert!(blank.email_domains.is_empty());
    }

    #[test]
    fn access_email_local_suffixes_default_to_empty() {
        // Empty is the shape the check has always had: bare stem plus an
        // optional number, nothing else.
        assert!(AccessEmailConfig::default().email_local_suffixes.is_empty());
    }

    #[test]
    fn access_email_local_suffixes_accept_a_list() {
        let cfg: AccessEmailConfig =
            serde_json::from_str(r#"{"email_local_suffixes":[".cw",".ctr"]}"#).expect("parses");
        assert_eq!(cfg.email_local_suffixes, vec![".cw", ".ctr"]);
    }

    #[test]
    fn a_comma_separated_suffix_string_is_split() {
        let cfg: AccessEmailConfig =
            serde_json::from_str(r#"{"email_local_suffixes":".cw, .ctr"}"#).expect("parses");
        assert_eq!(cfg.email_local_suffixes, vec![".cw", ".ctr"]);
    }

    #[test]
    fn suffixes_are_lower_cased_and_blanks_dropped() {
        // The address they are matched against is compared in lower case, so a
        // ".CW" written in features.json must not become a suffix nothing can
        // match. A blank entry would match everything, which is worse.
        let cfg: AccessEmailConfig =
            serde_json::from_str(r#"{"email_local_suffixes":[".CW","","  "]}"#).expect("parses");
        assert_eq!(cfg.email_local_suffixes, vec![".cw"]);
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
        serde_json::from_str(&format!(r#"{{"alerts":{{"allowed_users":{allowed}}}}}"#))
            .expect("should parse")
    }

    /// A fully resolved auth, as if Windows Credential Manager (or the
    /// environment) had every field set. Built by hand rather than through
    /// `jsm_auth::load_auth`, which touches the real registry/environment
    /// and would make these tests flaky on a machine that happens to have
    /// `JIRA_TOKEN` etc. exported for real use.
    fn complete_alerts_auth() -> crate::alerts::AlertsAuth {
        crate::alerts::AlertsAuth {
            email: "a@b.c".to_string(),
            token: "tok".to_string(),
            cloud_id: "cid".to_string(),
        }
    }

    #[test]
    fn alerts_hidden_when_user_not_on_allow_list() {
        let f = alerts_features(r#"["bconrad"]"#);
        let auth = complete_alerts_auth();
        assert!(f.alerts_visible_for("bconrad", &auth));
        assert!(f.alerts_visible_for("BConrad", &auth)); // case-insensitive
        assert!(!f.alerts_visible_for("someone.else", &auth));
        assert!(!f.alerts_visible_for("", &auth)); // unknown user → hidden
    }

    #[test]
    fn alerts_wildcard_shows_to_everyone() {
        let f = alerts_features(r#"["*"]"#);
        assert!(f.alerts_visible_for("anyone", &complete_alerts_auth()));
    }

    #[test]
    fn alerts_empty_allow_list_hides_from_everyone() {
        let f = alerts_features("[]");
        assert!(!f.alerts_visible_for("bconrad", &complete_alerts_auth()));
    }

    #[test]
    fn alerts_hidden_when_credentials_do_not_resolve() {
        // On the allow-list, but nothing resolved from Credential Manager or
        // the environment → nothing to query, so no button. This is the
        // replacement for the old "no cloud_id/email compiled in" case: those
        // fields no longer exist in features.json at all.
        let f: Features =
            serde_json::from_str(r#"{"alerts":{"allowed_users":["*"]}}"#).expect("should parse");
        assert!(!f.alerts_visible_for("bconrad", &crate::alerts::AlertsAuth::default()));
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

    /// User Sync reads and writes accounts on both bastions and can install a
    /// root cron entry, so it ships closed like alerts and personal_scripts —
    /// an admin adds names and rebuilds.
    #[test]
    fn user_sync_is_hidden_by_default() {
        let f = load();
        assert!(
            !f.user_sync_enabled_for("any.user"),
            "assets/features.json must ship an empty user_sync.allowed_users"
        );
        assert!(f.user_sync.allowed_users.is_empty());
    }

    #[test]
    fn user_sync_allow_list_is_honoured() {
        let f: Features =
            serde_json::from_str(r#"{"user_sync":{"allowed_users":["bconrad"]}}"#).expect("parses");
        assert!(f.user_sync_enabled_for("bconrad"));
        assert!(f.user_sync_enabled_for("BConrad"), "match is case-insensitive");
        assert!(!f.user_sync_enabled_for("someone.else"));

        let all: Features =
            serde_json::from_str(r#"{"user_sync":{"allowed_users":["*"]}}"#).expect("parses");
        assert!(all.user_sync_enabled_for("anyone"));
    }

    /// A malformed file must not hand out a feature that writes accounts.
    #[test]
    fn user_sync_fails_closed_on_a_missing_section() {
        let f: Features = serde_json::from_str("{}").expect("parses");
        assert!(!f.user_sync_enabled_for("anyone"));
        assert!(!Features::default().user_sync_enabled_for("anyone"));
    }

    #[test]
    fn the_on_call_test_gate_follows_the_shared_allow_list_rules() {
        let nobody = OnCallTestFeature { allowed_users: vec![] };
        assert!(!nobody.is_allowed_user("brandon"));

        let everyone = OnCallTestFeature { allowed_users: vec!["*".to_string()] };
        assert!(everyone.is_allowed_user("anyone"));

        let named = OnCallTestFeature {
            allowed_users: vec!["Brandon".to_string()],
        };
        assert!(named.is_allowed_user("brandon"), "match is case-insensitive");
        assert!(!named.is_allowed_user("someone_else"));
    }

    #[test]
    fn the_on_call_test_gate_deserializes_from_the_key_it_ships_under() {
        // `#[serde(default)]` means a missing or renamed key yields an empty
        // allow-list -- indistinguishable from a correctly-shipped closed
        // gate, so renaming it would disable the feature permanently and
        // silently. Constructing `OnCallTestFeature` directly cannot catch
        // that; only parsing an *open* gate out of JSON pins the key name.
        let open: Features =
            serde_json::from_str(r#"{"on_call_test":{"allowed_users":["*"]}}"#).expect("parses");
        assert!(open.on_call_test.is_allowed_user("anyone"));
        assert!(open.on_call_test_visible_for("anyone", Some("oncall@example.com")));

        // And the shipped file really carries the block, rather than relying
        // on the default to look the same as a closed gate.
        assert!(
            bundled_features().contains("\"on_call_test\""),
            "assets/features.json must carry an on_call_test block"
        );

        // A file without the key still fails closed, which is why the
        // assertion above cannot be replaced by "the list is empty".
        let missing: Features = serde_json::from_str("{}").expect("parses");
        assert!(!missing.on_call_test.is_allowed_user("anyone"));
    }

    #[test]
    fn the_on_call_test_action_needs_both_the_gate_and_a_recipient() {
        let open: Features =
            serde_json::from_str(r#"{"on_call_test":{"allowed_users":["*"]}}"#).expect("parses");
        let closed: Features = serde_json::from_str("{}").expect("parses");
        let addr = Some("oncall@example.com");

        assert!(open.on_call_test_visible_for("anyone", addr), "gate open, recipient set");
        assert!(
            !open.on_call_test_visible_for("anyone", None),
            "a control that cannot work must not be offered"
        );
        assert!(
            !closed.on_call_test_visible_for("anyone", addr),
            "a recipient does not grant the gate"
        );
        assert!(!closed.on_call_test_visible_for("anyone", None), "neither");

        // Blank is absent, not a destination -- the same rule `jsm_auth`
        // applies on the way in, restated here so a caller that hands over an
        // untrimmed value cannot open the action by accident.
        assert!(!open.on_call_test_visible_for("anyone", Some("")));
        assert!(!open.on_call_test_visible_for("anyone", Some("   ")));
    }

    #[test]
    fn the_shipped_on_call_test_gate_is_closed() {
        // It sends mail out of the org. It ships to nobody, and handing it
        // out is a deliberate edit plus a rebuild -- same posture as
        // user_sync and alerts.
        let shipped = load();
        assert!(
            shipped.on_call_test.allowed_users.is_empty(),
            "on_call_test must ship with an empty allow-list"
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
    fn shipped_features_keep_the_fed_refresh_off() {
        // The refresh runs a corporate CLI on a timer and pops a browser
        // window. That is opt-in per site: an admin has to set enabled AND
        // name the user.
        let f = load();
        assert!(
            !f.fed_auth_enabled_for("any.user"),
            "assets/features.json must ship fed_auth disabled with an empty allow-list"
        );
        assert!(!f.fed_auth.enabled, "fed_auth.enabled must ship false");
        assert!(
            f.fed_auth.allowed_users.is_empty(),
            "fed_auth.allowed_users must ship empty"
        );
    }

    #[test]
    fn the_fed_refresh_needs_both_the_switch_and_the_list() {
        // Either half alone is not enough — a stray ["*"] must not switch this
        // on for a whole site, and `enabled` must not switch it on for users
        // nobody named.
        let list_only: Features =
            serde_json::from_str(r#"{"fed_auth":{"allowed_users":["*"]}}"#).expect("parses");
        assert!(!list_only.fed_auth_enabled_for("bconrad"));

        let switch_only: Features =
            serde_json::from_str(r#"{"fed_auth":{"enabled":true}}"#).expect("parses");
        assert!(!switch_only.fed_auth_enabled_for("bconrad"));

        let both: Features =
            serde_json::from_str(r#"{"fed_auth":{"enabled":true,"allowed_users":["bconrad"]}}"#)
                .expect("parses");
        assert!(both.fed_auth_enabled_for("bconrad"));
        assert!(both.fed_auth_enabled_for("BConrad"), "case-insensitive");
        assert!(!both.fed_auth_enabled_for("someone.else"));
        assert!(!both.fed_auth_enabled_for(""), "unknown user fails closed");
    }

    #[test]
    fn the_fed_refresh_fails_closed_when_absent_or_malformed() {
        let absent: Features = serde_json::from_str("{}").expect("parses");
        assert!(!absent.fed_auth_enabled_for("bconrad"));
        assert!(!Features::default().fed_auth_enabled_for("bconrad"));
    }

    /// The shipped PowerShell must be pure ASCII.
    ///
    /// Windows PowerShell 5.1 reads a `.ps1` with no BOM as Windows-1252,
    /// not UTF-8. An em dash is UTF-8 `E2 80 94`, and that trailing `0x94`
    /// decodes as U+201D -- a smart closing quote, which PowerShell honours
    /// as a string delimiter. One inside a double-quoted string therefore
    /// ends the string early, turns the remainder of the line into code, and
    /// the file stops parsing with a brace error a dozen lines away.
    ///
    /// That is not hypothetical: it shipped, and the sign-in silently never
    /// ran because the script died at parse time before emitting a single
    /// marker.
    /// No two members of an embedded C# class may share a name and parameter
    /// list.
    ///
    /// The `Add-Type` blocks in the PowerShell assets are compiled at run
    /// time, so a duplicate is not a parse error here but a crash there --
    /// "Type 'Fg' already defines a member called 'Handle' with the same
    /// parameter types" -- and the script dies before emitting a single
    /// marker. C# does not overload on return type, so adding a `long
    /// Handle()` beside an `IntPtr Handle()` breaks the whole file.
    ///
    /// That has happened, which is why this exists. Nothing in a Linux build
    /// compiles this C#, so without a check like it the first sign is a
    /// failed sign-in on someone's desk.
    #[test]
    fn embedded_csharp_has_no_duplicate_members() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/scripts");
        for entry in std::fs::read_dir(&dir).expect("assets/scripts exists") {
            let path = entry.expect("readable entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("ps1") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("readable script");
            let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();

            let mut seen: std::collections::HashMap<(String, String), usize> =
                std::collections::HashMap::new();
            let mut in_csharp = false;
            for (i, line) in text.lines().enumerate() {
                let t = line.trim();
                // The heredocs holding C# open with `Add-Type @'` and close
                // with a lone `'@`.
                if t.starts_with("Add-Type @'") {
                    in_csharp = true;
                    seen.clear();
                    continue;
                }
                if in_csharp && t == "'@" {
                    in_csharp = false;
                    continue;
                }
                if !in_csharp || !t.starts_with("public static ") {
                    continue;
                }
                let Some((before, after)) = t["public static ".len()..].split_once('(') else {
                    continue;
                };
                // Last token before the paren is the member name; anything
                // ahead of it is the return type and modifiers like `extern`.
                let Some(member) = before.split_whitespace().next_back() else {
                    continue;
                };
                let params = after.split(')').next().unwrap_or("").trim().to_string();
                let key = (member.to_string(), params);
                if let Some(first) = seen.get(&key) {
                    panic!(
                        "{name}:{} declares '{}' again, already declared at line {}. \
                         C# does not overload on return type, so Add-Type will refuse \
                         the whole class at run time and the script will die before \
                         producing any output.",
                        i + 1,
                        key.0,
                        first
                    );
                }
                seen.insert(key, i + 1);
            }
        }
    }

    #[test]
    fn the_shipped_powershell_is_ascii_only() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/scripts");
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).expect("assets/scripts exists") {
            let path = entry.expect("readable entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("ps1") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("readable script");
            for (i, line) in text.lines().enumerate() {
                if let Some(ch) = line.chars().find(|c| !c.is_ascii()) {
                    panic!(
                        "{}:{} contains non-ASCII {:?} (U+{:04X}). Windows PowerShell \
                         reads these files as Windows-1252, where such bytes can act \
                         as string delimiters and break parsing. Use ASCII.",
                        path.file_name().unwrap_or_default().to_string_lossy(),
                        i + 1,
                        ch,
                        ch as u32
                    );
                }
            }
            checked += 1;
        }
        assert!(checked > 0, "no .ps1 files found under {}", dir.display());
    }

    #[test]
    fn the_shipped_file_keeps_the_automatic_sign_in_off() {
        // Typing a domain password into an identity provider is its own
        // opt-in, on top of the feature gate. The shipped file must hand it
        // to nobody.
        let f = load();
        assert!(!f.fed_auth.auto_sign_in, "auto_sign_in must ship false");
        assert!(!f.fed_auth.auto_sign_in_for("any.user"));
    }

    #[test]
    fn the_automatic_sign_in_has_its_own_allow_list() {
        // The point of the split: hand everyone the refresh, hand the
        // keystroke automation to a named few.
        let f: Features = serde_json::from_str(
            r#"{"fed_auth":{"enabled":true,"allowed_users":["*"],
                 "auto_sign_in":true,"auto_sign_in_allowed_users":["conra"]}}"#,
        )
        .expect("parses");
        assert!(f.fed_auth_enabled_for("someone.else"), "everyone gets the refresh");
        assert!(
            !f.fed_auth.auto_sign_in_for("someone.else"),
            "but not the automation"
        );
        assert!(f.fed_auth.auto_sign_in_for("conra"));
        assert!(f.fed_auth.auto_sign_in_for("CONRA"), "case-insensitive");
    }

    #[test]
    fn the_automatic_sign_in_needs_the_refresh_gate_as_well() {
        // On the sign-in list but not the refresh one: nothing to sign in for.
        let f: Features = serde_json::from_str(
            r#"{"fed_auth":{"enabled":true,"allowed_users":[],
                 "auto_sign_in":true,"auto_sign_in_allowed_users":["conra"]}}"#,
        )
        .expect("parses");
        assert!(!f.fed_auth.auto_sign_in_for("conra"));
    }

    #[test]
    fn the_master_switch_still_overrides_the_sign_in_list() {
        // One place to turn the automation off site-wide without editing the
        // list, mirroring how `enabled` sits above `allowed_users`.
        let f: Features = serde_json::from_str(
            r#"{"fed_auth":{"enabled":true,"allowed_users":["*"],
                 "auto_sign_in":false,"auto_sign_in_allowed_users":["*"]}}"#,
        )
        .expect("parses");
        assert!(f.fed_auth_enabled_for("conra"));
        assert!(!f.fed_auth.auto_sign_in_for("conra"));
    }

    #[test]
    fn the_shipped_sign_in_list_is_empty() {
        let f = load();
        assert!(
            f.fed_auth.auto_sign_in_allowed_users.is_empty(),
            "auto_sign_in_allowed_users must ship empty"
        );
    }

    #[test]
    fn the_automatic_sign_in_needs_the_feature_gate_too() {
        // auto_sign_in alone must not switch anything on: without `enabled`
        // and a named user there is no refresh to sign in for.
        let flag_only: Features =
            serde_json::from_str(r#"{"fed_auth":{"auto_sign_in":true}}"#).expect("parses");
        assert!(!flag_only.fed_auth.auto_sign_in_for("bconrad"));

        let gated_only: Features = serde_json::from_str(
            r#"{"fed_auth":{"enabled":true,"allowed_users":["bconrad"]}}"#,
        )
        .expect("parses");
        assert!(
            gated_only.fed_auth_enabled_for("bconrad"),
            "the refresh itself is on"
        );
        assert!(
            !gated_only.fed_auth.auto_sign_in_for("bconrad"),
            "but the sign-in is not"
        );

        // Still not enough without the sign-in list -- see
        // `the_automatic_sign_in_has_its_own_allow_list`.
        let all: Features = serde_json::from_str(
            r#"{"fed_auth":{"enabled":true,"allowed_users":["bconrad"],"auto_sign_in":true}}"#,
        )
        .expect("parses");
        assert!(!all.fed_auth.auto_sign_in_for("bconrad"));
    }

    #[test]
    fn the_focus_guard_and_vault_target_are_never_empty() {
        // A blank title match would disable the check that keeps the password
        // out of the wrong window; a blank target would read whatever is
        // first in the vault. Both fall back rather than going quiet.
        let blank: Features = serde_json::from_str(
            r#"{"fed_auth":{"browser_title_match":"  ","credential_target":" "}}"#,
        )
        .expect("parses");
        assert_eq!(blank.fed_auth.resolved_title_match(), "okta|sign in|verify");
        assert_eq!(blank.fed_auth.resolved_credential_target(), "ec2-manager-fed");
        assert!(!load().fed_auth.resolved_title_match().is_empty());
        assert!(!load().fed_auth.resolved_credential_target().is_empty());
    }

    #[test]
    fn chrome_always_gets_a_new_window_unless_told_otherwise() {
        // Without it, an already-running Chrome opens a tab in some existing
        // window and the sign-in waits for a foreground page that never
        // comes forward.
        let blank: Features =
            serde_json::from_str(r#"{"fed_auth":{"chrome_args":"  "}}"#).expect("parses");
        assert_eq!(blank.fed_auth.resolved_chrome_args(), "--new-window");
        assert_eq!(load().fed_auth.resolved_chrome_args(), "--new-window");

        let custom: Features = serde_json::from_str(
            r#"{"fed_auth":{"chrome_args":["--new-window","--profile-directory=Profile 1"]}}"#,
        )
        .expect("parses");
        assert_eq!(
            custom.fed_auth.resolved_chrome_args(),
            "--new-window --profile-directory=Profile 1"
        );
    }

    #[test]
    fn the_mfa_step_accepts_its_own_window_but_never_an_empty_pattern() {
        // Okta Verify owns the confirm prompt in a separate process, so the
        // chrome-only guard has to widen for that one step. A blank pattern
        // would match everything and remove the check, so it falls back.
        let blank: Features =
            serde_json::from_str(r#"{"fed_auth":{"mfa_process_match":"  "}}"#).expect("parses");
        assert_eq!(blank.fed_auth.resolved_mfa_process_match(), "chrome|okta");
        assert!(!load().fed_auth.resolved_mfa_process_match().is_empty());

        let custom: Features = serde_json::from_str(
            r#"{"fed_auth":{"mfa_process_match":["chrome","OktaVerify"]}}"#,
        )
        .expect("parses");
        assert_eq!(custom.fed_auth.resolved_mfa_process_match(), "chrome|OktaVerify");
    }

    #[test]
    fn the_focus_guard_takes_several_title_fragments() {
        // The sign-in walks activation -> username -> password -> MFA, and
        // those pages do not share a title. One fragment matching the first
        // page aborted the run on a later one, which is the bug this fixes.
        let list: Features = serde_json::from_str(
            r#"{"fed_auth":{"browser_title_match":["Sign In","Verify"]}}"#,
        )
        .expect("parses");
        assert_eq!(list.fed_auth.resolved_title_match(), "Sign In|Verify");

        // A single string still works — an existing config must not break.
        let one: Features =
            serde_json::from_str(r#"{"fed_auth":{"browser_title_match":"Sign In"}}"#)
                .expect("parses");
        assert_eq!(one.fed_auth.resolved_title_match(), "Sign In");

        // As does the comma-separated spelling, and blanks are dropped rather
        // than becoming an empty fragment that matches everything.
        let csv: Features = serde_json::from_str(
            r#"{"fed_auth":{"browser_title_match":"Sign In, ,Verify"}}"#,
        )
        .expect("parses");
        assert_eq!(csv.fed_auth.resolved_title_match(), "Sign In|Verify");
    }

    #[test]
    fn the_shipped_title_fragments_cover_the_whole_sign_in() {
        // Guards the shipped list against being narrowed back to one value.
        let shipped = load().fed_auth.resolved_title_match();
        for frag in ["okta", "sign in", "verify"] {
            assert!(
                shipped.contains(frag),
                "shipped browser_title_match should cover '{frag}': {shipped}"
            );
        }
    }

    #[test]
    fn the_fed_command_falls_back_to_fed_up() {
        let f: Features = serde_json::from_str("{}").expect("parses");
        assert_eq!(f.fed_auth.resolved_command(), vec!["fed", "up"]);
        // Blank entries are dropped, not passed through as empty argv slots.
        let blanks: Features =
            serde_json::from_str(r#"{"fed_auth":{"command":["  ",""]}}"#).expect("parses");
        assert_eq!(blanks.fed_auth.resolved_command(), vec!["fed", "up"]);
        let custom: Features =
            serde_json::from_str(r#"{"fed_auth":{"command":["fed","up","--profile","x"]}}"#)
                .expect("parses");
        assert_eq!(
            custom.fed_auth.resolved_command(),
            vec!["fed", "up", "--profile", "x"]
        );
    }

    #[test]
    fn zero_timings_mean_use_the_defaults() {
        let f: Features = serde_json::from_str("{}").expect("parses");
        assert_eq!(f.fed_auth.retry_policy(), crate::fed_auth::RetryPolicy::default());
        let custom: Features = serde_json::from_str(
            r#"{"fed_auth":{"retry_interval_secs":30,"retry_window_secs":0}}"#,
        )
        .expect("parses");
        let p = custom.fed_auth.retry_policy();
        assert_eq!(p.interval, std::time::Duration::from_secs(30));
        assert_eq!(
            p.window,
            crate::fed_auth::RetryPolicy::default().window,
            "0 keeps the default rather than meaning 'never retry'"
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
        // A missing `alerts` section means an empty allow-list, which hides
        // the button regardless of what credentials happen to resolve — this
        // asserts the allow-list gate specifically, with a deliberately
        // complete auth, so it can't pass for the wrong reason (see
        // `alerts_hidden_when_credentials_do_not_resolve` for that half).
        let f: Features = serde_json::from_str("{}").expect("should parse");
        assert!(!f.alerts_visible_for("bconrad", &complete_alerts_auth()));
    }

    #[test]
    fn reaper_ships_disabled_dry_and_with_nobody_allowed() {
        let f = Features::default();
        assert!(!f.reaper.enabled);
        assert!(f.reaper.dry_run, "dry_run must default on — arming is opt-in");
        assert!(f.reaper.allowed_users.is_empty());
        assert!(!f.reaper_enabled_for("anyone"));
    }

    #[test]
    fn reaper_needs_enabled_and_the_allow_list_and_a_schedule() {
        let mut f = Features::default();
        f.reaper.allowed_users = vec!["bconrad".to_string()];
        assert!(!f.reaper_enabled_for("bconrad"), "enabled is still false");

        f.reaper.enabled = true;
        assert!(f.reaper_enabled_for("bconrad"));
        assert!(f.reaper_enabled_for("BConrad"), "match is case-insensitive");
        assert!(!f.reaper_enabled_for("someone_else"));
        assert!(!f.reaper_enabled_for(""), "a missing username fails closed");
    }

    #[test]
    fn a_wildcard_does_not_open_reaper_to_everyone() {
        // Every other allow-list honours "*". This one must not: running
        // `compose down` on production is not a site-wide default.
        let mut f = Features::default();
        f.reaper.enabled = true;
        f.reaper.allowed_users = vec!["*".to_string()];
        assert!(!f.reaper_enabled_for("anyone"));
    }

    #[test]
    fn more_than_one_allowed_user_is_detectable() {
        // Nothing arbitrates two machines acting on one alert, so the caller
        // logs a warning. The list is single-entry by policy.
        let mut f = Features::default();
        f.reaper.allowed_users = vec!["a".to_string()];
        assert!(!f.reaper.has_multiple_users());
        f.reaper.allowed_users.push("b".to_string());
        assert!(f.reaper.has_multiple_users());
    }

    #[test]
    fn the_shipped_features_json_keeps_reaper_dark_and_secret_free() {
        let f = load();
        assert!(!f.reaper.enabled);
        assert!(f.reaper.allowed_users.is_empty());
        assert!(f.reaper.dry_run);
        // The load-bearing gate: a blank *_contains rule matches nothing
        // (see `contains_ci`), which is what makes shipping the armed code
        // path safe even with `enabled`/`dry_run` both flipped by hand.
        // Nothing else in this test actually exercises that guarantee.
        assert!(f.reaper.alertname_contains.trim().is_empty());
        assert!(f.reaper.app_contains.trim().is_empty());
        assert!(f.reaper.message_contains.trim().is_empty());
        // The JSM schedule id, Atlassian account id, cloud id, email and API
        // token are no longer fields anywhere in `Features` at all — see
        // `the_shipped_features_json_carries_no_jsm_or_opsgenie_credential_key`
        // below, which pins that they cannot even be *present* in the JSON.
    }

    #[test]
    fn the_shipped_features_json_carries_no_jsm_or_opsgenie_credential_key() {
        // The five JSM/Opsgenie values (Atlassian email, API token, cloud id,
        // schedule id, account id) must be resolvable ONLY from Windows
        // Credential Manager / the environment (see `jsm_auth`) — never from
        // this committed file. Walked structurally over the parsed JSON
        // rather than grepped as text, so a key nested anywhere still fails
        // the build, and so this cannot be fooled by the same words
        // appearing in plain English inside the `_alerts_comment` /
        // `_reaper_comment` strings (which they deliberately still do, to
        // point an admin at Credential Manager).
        let value: serde_json::Value = serde_json::from_str(&bundled_features())
            .expect("assets/features.json parses as JSON");

        fn walk(v: &serde_json::Value, forbidden: &[&str], found: &mut Vec<String>) {
            match v {
                serde_json::Value::Object(map) => {
                    for (k, val) in map {
                        if forbidden.contains(&k.as_str()) {
                            found.push(k.clone());
                        }
                        walk(val, forbidden, found);
                    }
                }
                serde_json::Value::Array(arr) => {
                    for item in arr {
                        walk(item, forbidden, found);
                    }
                }
                _ => {}
            }
        }

        let forbidden = ["cloud_id", "email", "token", "schedule_id", "account_id"];
        let mut found = Vec::new();
        walk(&value, &forbidden, &mut found);
        assert!(
            found.is_empty(),
            "assets/features.json must not carry any of {forbidden:?} as a JSON key \
             (all five JSM/Opsgenie values live in Windows Credential Manager only); \
             found: {found:?}"
        );
    }
}

//! Where the JSM Ops credentials and tenant identifiers come from, and in
//! what order.
//!
//! `assets/features.json` is committed, so it is the *last* resort and
//! ships every value blank. The everyday source is Windows Credential
//! Manager, holding five separate generic credentials, each with a matching
//! environment-variable override (target ↔ env var, one line each):
//!
//! - `ec2_manager/jsm` ↔ `ATLASSIAN_EMAIL_ENV` / `JIRA_TOKEN_ENV` —
//!   username = Atlassian email, password = API token
//! - `ec2_manager/jsm_cloud_id` ↔ `CLOUD_ID_ENV` — password = cloud id
//!   (username is a placeholder)
//! - `ec2_manager/jsm_schedule_id` ↔ `SCHEDULE_ID_ENV` — password =
//!   schedule id (username ignored)
//! - `ec2_manager/jsm_account_id` ↔ `ATLASSIAN_ACCOUNT_ID_ENV` — password =
//!   the caller's *Atlassian* account id (e.g. `5b10ac8d82e05b22cc7d4ef5`),
//!   used only to find yourself in the JSM on-call response. Unrelated to
//!   any AWS account id — elsewhere in this codebase (`Alert.account`,
//!   `Target.account_id`) `account_id` means an AWS account, so this
//!   constant is named `ATLASSIAN_ACCOUNT_ID_TARGET` to keep the two from
//!   being confused.
//! - `ec2_manager/opsgenie_api_key` ↔ `OPSGENIE_API_KEY_ENV` — password =
//!   the Opsgenie API key (username ignored). See `opsgenie_api_key` for
//!   why this is a different authentication scheme, not just another
//!   credential.
//!
//! Every value resolves environment → credential store → compiled-in
//! fallback, checked independently per field so one present override never
//! masks a sibling that still needs to fall through.
//!
//! The environment-variable names are exported as constants, not just used
//! as string literals at each call site, because they are a compatibility
//! contract with the user's existing shell workflow —
//! `assets/scripts/alerts_10min.sh` and their own curl commands already
//! export `CLOUD_ID`, `SCHEDULE_ID` and `MY_ID` — and a later caller (Task 2
//! supplies `SCHEDULE_ID_ENV`/`ATLASSIAN_ACCOUNT_ID_ENV` itself) mistyping a
//! literal would fail silently: the value simply never resolves and the
//! feature reports "not on call" forever, with no error.

use crate::alerts::AlertsAuth;
use crate::features::AlertsFeature;

/// Credential Manager target for the JSM email/token pair.
pub const JSM_CREDENTIAL_TARGET: &str = "ec2_manager/jsm";
/// Environment variable that overrides the stored/JSON Atlassian email.
pub const ATLASSIAN_EMAIL_ENV: &str = "ATLASSIAN_EMAIL";
/// Environment variable that overrides the stored/JSON API token.
pub const JIRA_TOKEN_ENV: &str = "JIRA_TOKEN";

/// Credential Manager target for the cloud id (password field only).
pub const CLOUD_ID_TARGET: &str = "ec2_manager/jsm_cloud_id";
/// Environment variable that overrides the stored/JSON cloud id. Matches
/// the name already used by `assets/scripts/alerts_10min.sh` and the
/// user's existing curl commands — do not rename.
pub const CLOUD_ID_ENV: &str = "CLOUD_ID";

/// Credential Manager target for the on-call schedule id (password field only).
pub const SCHEDULE_ID_TARGET: &str = "ec2_manager/jsm_schedule_id";
/// Environment variable that overrides the stored schedule id. Same
/// compatibility contract as `CLOUD_ID_ENV` — do not rename.
pub const SCHEDULE_ID_ENV: &str = "SCHEDULE_ID";

/// Credential Manager target for the caller's *Atlassian* account id
/// (password field only) — unrelated to any AWS account id.
pub const ATLASSIAN_ACCOUNT_ID_TARGET: &str = "ec2_manager/jsm_account_id";
/// Environment variable that overrides the stored Atlassian account id.
/// Named `MY_ID` (not `ACCOUNT_ID`) in the user's existing shell workflow —
/// do not rename.
pub const ATLASSIAN_ACCOUNT_ID_ENV: &str = "MY_ID";

/// Credential Manager target for the Opsgenie API key (password field only).
pub const OPSGENIE_API_KEY_TARGET: &str = "ec2_manager/opsgenie_api_key";
/// Environment variable that overrides the stored Opsgenie API key.
pub const OPSGENIE_API_KEY_ENV: &str = "OPSGENIE_API_KEY";

/// First non-blank of the candidates, trimmed. Blank-but-set is treated as
/// absent: `export JIRA_TOKEN=` must not shadow a real stored credential.
fn first_non_blank(candidates: [Option<&str>; 3]) -> String {
    candidates
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .unwrap_or_default()
        .to_string()
}

/// Resolve the auth pair and cloud id from all three sources. Pure: every
/// input is passed in, so precedence is testable without touching the
/// environment or the Windows API.
pub fn resolve_auth(
    feature: &AlertsFeature,
    stored: Option<(String, String)>,
    stored_cloud_id: Option<String>,
    env_email: Option<String>,
    env_token: Option<String>,
    env_cloud_id: Option<String>,
) -> AlertsAuth {
    let (stored_email, stored_token) = match stored {
        Some((u, s)) => (Some(u), Some(s)),
        None => (None, None),
    };
    AlertsAuth {
        email: first_non_blank([
            env_email.as_deref(),
            stored_email.as_deref(),
            Some(&feature.email),
        ]),
        token: first_non_blank([
            env_token.as_deref(),
            stored_token.as_deref(),
            Some(&feature.token),
        ]),
        cloud_id: first_non_blank([
            env_cloud_id.as_deref(),
            stored_cloud_id.as_deref(),
            Some(&feature.cloud_id),
        ]),
    }
}

/// One non-secret identifier, resolved environment → credential store →
/// compiled-in fallback. The credential's *password* field carries the
/// value; its username is a placeholder, because `cmdkey` requires a
/// username but these are single values.
///
/// Same precedence as `resolve_auth`, and blank-but-set is treated as
/// absent for the same reason: `export CLOUD_ID=` must not shadow a real
/// stored value.
pub fn resolve_id(env_var: &str, cred_target: &str, fallback: &str) -> String {
    let from_env = std::env::var(env_var).ok();
    let from_cred = crate::wincred::read_generic(cred_target).map(|(_user, secret)| secret);
    first_non_blank([from_env.as_deref(), from_cred.as_deref(), Some(fallback)])
}

/// `resolve_auth` wired to the real environment and credential store.
pub fn load_auth(feature: &AlertsFeature) -> AlertsAuth {
    resolve_auth(
        feature,
        crate::wincred::read_generic(JSM_CREDENTIAL_TARGET),
        crate::wincred::read_generic(CLOUD_ID_TARGET).map(|(_user, secret)| secret),
        std::env::var(ATLASSIAN_EMAIL_ENV).ok(),
        std::env::var(JIRA_TOKEN_ENV).ok(),
        std::env::var(CLOUD_ID_ENV).ok(),
    )
}

/// Wraps a resolved value as `Some` unless it is blank, matching the
/// blank-is-absent rule the rest of this module uses. Pure and separately
/// testable, since `opsgenie_api_key` itself is not — it is hardwired to
/// the real `OPSGENIE_API_KEY` environment variable.
fn some_unless_blank(v: String) -> Option<String> {
    if v.trim().is_empty() {
        None
    } else {
        Some(v)
    }
}

/// The Opsgenie API key, if one is configured.
///
/// Separate from the Atlassian email/token pair because it is a different
/// authentication scheme, not a different credential for the same one:
/// Opsgenie-lineage endpoints take `Authorization: GenieKey <key>`, where
/// the Jira REST endpoints take Basic `email:token`. The JSM Ops schedule
/// endpoints are Opsgenie-lineage, which is a candidate explanation for the
/// 403 seen there while the alerts endpoint works on Basic.
///
/// Returns `None` when unset, so a caller can fall back to Basic auth
/// rather than failing — this is an addition to the existing credentials,
/// never a replacement for them.
pub fn opsgenie_api_key() -> Option<String> {
    let v = resolve_id(OPSGENIE_API_KEY_ENV, OPSGENIE_API_KEY_TARGET, "");
    some_unless_blank(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::AlertsFeature;

    fn feature() -> AlertsFeature {
        AlertsFeature {
            cloud_id: "cloud-from-json".to_string(),
            email: "json@example.com".to_string(),
            token: "json-token".to_string(),
            allowed_users: vec![],
        }
    }

    #[test]
    fn the_environment_beats_the_credential_store_and_the_json() {
        let auth = resolve_auth(
            &feature(),
            Some(("stored@example.com".into(), "stored-token".into())),
            Some("stored-cloud".into()),
            Some("env@example.com".into()),
            Some("env-token".into()),
            Some("env-cloud".into()),
        );
        assert_eq!(auth.email, "env@example.com");
        assert_eq!(auth.token, "env-token");
        assert_eq!(auth.cloud_id, "env-cloud");
    }

    #[test]
    fn the_credential_store_beats_the_json() {
        let auth = resolve_auth(
            &feature(),
            Some(("stored@example.com".into(), "stored-token".into())),
            Some("stored-cloud".into()),
            None,
            None,
            None,
        );
        assert_eq!(auth.email, "stored@example.com");
        assert_eq!(auth.token, "stored-token");
        assert_eq!(auth.cloud_id, "stored-cloud");
    }

    #[test]
    fn the_json_is_the_last_resort() {
        let auth = resolve_auth(&feature(), None, None, None, None, None);
        assert_eq!(auth.email, "json@example.com");
        assert_eq!(auth.token, "json-token");
        assert_eq!(auth.cloud_id, "cloud-from-json");
    }

    #[test]
    fn each_field_falls_back_on_its_own() {
        // Only the token is in the environment; the email still comes from the
        // credential store. Resolving as a pair would silently pick up the
        // wrong email here.
        let auth = resolve_auth(
            &feature(),
            Some(("stored@example.com".into(), "stored-token".into())),
            None,
            None,
            Some("env-token".into()),
            None,
        );
        assert_eq!(auth.email, "stored@example.com");
        assert_eq!(auth.token, "env-token");
        assert_eq!(auth.cloud_id, "cloud-from-json");
    }

    #[test]
    fn a_blank_environment_variable_does_not_win() {
        // `export JIRA_TOKEN=` is a set-but-empty variable and must not
        // shadow a real stored credential.
        let auth = resolve_auth(
            &feature(),
            Some(("stored@example.com".into(), "stored-token".into())),
            Some("stored-cloud".into()),
            Some("   ".into()),
            Some("".into()),
            Some("  ".into()),
        );
        assert_eq!(auth.email, "stored@example.com");
        assert_eq!(auth.token, "stored-token");
        assert_eq!(auth.cloud_id, "stored-cloud");
    }

    #[test]
    fn everything_blank_yields_an_incomplete_auth() {
        let empty = AlertsFeature::default();
        let auth = resolve_auth(&empty, None, None, None, None, None);
        assert!(!auth.is_complete());
    }

    // -- resolve_id --------------------------------------------------------
    //
    // These use dedicated env var names (never the real `CLOUD_ID` /
    // `SCHEDULE_ID` / `MY_ID`) so parallel test execution can't collide with
    // each other or with anything a real run might have set.

    #[test]
    fn an_id_in_the_environment_wins_over_the_compiled_in_fallback() {
        let var = "TEST_REAPER_ID_A";
        std::env::set_var(var, "env-value");
        let result = resolve_id(var, "ec2_manager/does_not_exist_a", "fallback-value");
        std::env::remove_var(var);
        assert_eq!(result, "env-value");
    }

    #[test]
    fn a_blank_environment_variable_does_not_win_for_a_bare_id_either() {
        let var = "TEST_REAPER_ID_B";
        std::env::set_var(var, "   ");
        let result = resolve_id(var, "ec2_manager/does_not_exist_b", "fallback-value");
        std::env::remove_var(var);
        assert_eq!(result, "fallback-value");
    }

    #[test]
    fn an_absent_credential_and_an_unset_environment_variable_yield_the_fallback() {
        let var = "TEST_REAPER_ID_C";
        std::env::remove_var(var);
        let result = resolve_id(var, "ec2_manager/does_not_exist_c", "fallback-value");
        assert_eq!(result, "fallback-value");
    }

    #[test]
    fn an_id_with_nothing_set_and_no_fallback_yields_an_empty_string() {
        let var = "TEST_REAPER_ID_D";
        std::env::remove_var(var);
        let result = resolve_id(var, "ec2_manager/does_not_exist_d", "");
        assert_eq!(result, "");
    }

    // -- opsgenie_api_key ---------------------------------------------------
    //
    // `opsgenie_api_key` itself is hardwired to the real `OPSGENIE_API_KEY`
    // variable, so it is not called directly here — a developer with that
    // variable set locally would get a different (correct!) result than CI,
    // which is exactly the flakiness the coordinator flagged. Instead:
    // `some_unless_blank` (the wrapping logic) is tested directly, and the
    // env-var path it wraps is exercised through `resolve_id` with a
    // throwaway variable name, the same way the tests above do.

    #[test]
    fn a_blank_or_whitespace_value_resolves_to_none() {
        assert_eq!(some_unless_blank("   ".to_string()), None);
        assert_eq!(some_unless_blank(String::new()), None);
    }

    #[test]
    fn a_present_value_resolves_to_some() {
        assert_eq!(
            some_unless_blank("genie-key-123".to_string()),
            Some("genie-key-123".to_string())
        );
    }

    #[test]
    fn the_opsgenie_style_resolution_yields_none_when_nothing_is_set() {
        let var = "TEST_REAPER_ID_E";
        std::env::remove_var(var);
        let resolved = resolve_id(var, "ec2_manager/does_not_exist_e", "");
        assert_eq!(some_unless_blank(resolved), None);
    }

    #[test]
    fn the_opsgenie_style_resolution_yields_some_when_the_environment_is_set() {
        let var = "TEST_REAPER_ID_F";
        std::env::set_var(var, "genie-key-123");
        let resolved = resolve_id(var, "ec2_manager/does_not_exist_f", "");
        std::env::remove_var(var);
        assert_eq!(some_unless_blank(resolved), Some("genie-key-123".to_string()));
    }

    // -- environment-variable name constants --------------------------------
    //
    // These strings are a compatibility contract with the user's existing
    // shell workflow (`assets/scripts/alerts_10min.sh` and their own curl
    // commands already export `CLOUD_ID`, `SCHEDULE_ID` and `MY_ID`), so a
    // rename here would silently break it.

    #[test]
    fn the_environment_variable_names_match_the_users_existing_shell_workflow() {
        assert_eq!(ATLASSIAN_EMAIL_ENV, "ATLASSIAN_EMAIL");
        assert_eq!(JIRA_TOKEN_ENV, "JIRA_TOKEN");
        assert_eq!(CLOUD_ID_ENV, "CLOUD_ID");
        assert_eq!(SCHEDULE_ID_ENV, "SCHEDULE_ID");
        assert_eq!(ATLASSIAN_ACCOUNT_ID_ENV, "MY_ID");
        assert_eq!(OPSGENIE_API_KEY_ENV, "OPSGENIE_API_KEY");
    }
}

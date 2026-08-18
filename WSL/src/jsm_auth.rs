//! Where the JSM Ops credentials and tenant identifiers come from, and in
//! what order.
//!
//! `assets/features.json` is committed, so it is the *last* resort and
//! ships every value blank. The everyday source is Windows Credential
//! Manager, holding four separate generic credentials:
//!
//! - `ec2_manager/jsm`             — username = Atlassian email, password = API token
//! - `ec2_manager/jsm_cloud_id`    — password = cloud id (username is a placeholder)
//! - `ec2_manager/jsm_schedule_id` — password = schedule id (username ignored)
//! - `ec2_manager/jsm_account_id`  — password = account id (username ignored)
//!
//! Every value resolves environment → credential store → compiled-in
//! fallback, checked independently per field so one present override never
//! masks a sibling that still needs to fall through.

use crate::alerts::AlertsAuth;
use crate::features::AlertsFeature;

/// Credential Manager target for the JSM email/token pair.
pub const JSM_CREDENTIAL_TARGET: &str = "ec2_manager/jsm";
/// Credential Manager target for the cloud id (password field only).
pub const CLOUD_ID_TARGET: &str = "ec2_manager/jsm_cloud_id";
/// Credential Manager target for the on-call schedule id (password field only).
pub const SCHEDULE_ID_TARGET: &str = "ec2_manager/jsm_schedule_id";
/// Credential Manager target for the account id (password field only).
pub const ACCOUNT_ID_TARGET: &str = "ec2_manager/jsm_account_id";

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
        std::env::var("ATLASSIAN_EMAIL").ok(),
        std::env::var("JIRA_TOKEN").ok(),
        std::env::var("CLOUD_ID").ok(),
    )
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
    fn resolve_id_the_environment_wins_over_the_fallback() {
        let var = "TEST_REAPER_ID_A";
        std::env::set_var(var, "env-value");
        let result = resolve_id(var, "ec2_manager/does_not_exist_a", "fallback-value");
        std::env::remove_var(var);
        assert_eq!(result, "env-value");
    }

    #[test]
    fn resolve_id_a_blank_environment_variable_does_not_win() {
        let var = "TEST_REAPER_ID_B";
        std::env::set_var(var, "   ");
        let result = resolve_id(var, "ec2_manager/does_not_exist_b", "fallback-value");
        std::env::remove_var(var);
        assert_eq!(result, "fallback-value");
    }

    #[test]
    fn resolve_id_an_absent_credential_and_blank_env_yields_the_fallback() {
        let var = "TEST_REAPER_ID_C";
        std::env::remove_var(var);
        let result = resolve_id(var, "ec2_manager/does_not_exist_c", "fallback-value");
        assert_eq!(result, "fallback-value");
    }

    #[test]
    fn resolve_id_an_entirely_unset_value_yields_an_empty_string() {
        let var = "TEST_REAPER_ID_D";
        std::env::remove_var(var);
        let result = resolve_id(var, "ec2_manager/does_not_exist_d", "");
        assert_eq!(result, "");
    }
}

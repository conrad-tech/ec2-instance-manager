//! "Am I the current on-call?" — one GET against the JSM Ops schedule.
//!
//! This does **not** decide whether reaper is remediated. It selects the
//! handling: on call the app acknowledges the alert, waits longer, and
//! escalates on failure; off call it never acknowledges (that would suppress
//! the real on-call engineer's page), waits less, and sends one quiet
//! message. A failed lookup is therefore not fatal — the caller treats it as
//! off call, which is the conservative direction on both axes.

use crate::alerts::AlertsAuth;
use crate::error::{AppError, Result};

/// True when `id` appears anywhere in the JSON body as a complete string
/// value. Matched by walking every string rather than at a fixed path: the
/// on-call response has carried the participant list under several different
/// keys, and `oncall_probe.sh` resolves it the same way for the same reason.
pub fn response_mentions(body: &str, id: &str) -> bool {
    let needle = id.trim();
    if needle.is_empty() {
        return false;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };
    fn walk(v: &serde_json::Value, needle: &str) -> bool {
        match v {
            serde_json::Value::String(s) => s.trim() == needle,
            serde_json::Value::Array(a) => a.iter().any(|x| walk(x, needle)),
            serde_json::Value::Object(o) => o.values().any(|x| walk(x, needle)),
            _ => false,
        }
    }
    walk(&value, needle)
}

/// Ask the schedule whether `account_id` currently holds the pager.
///
/// `Err` means the question could not be answered — a 403, an outage, a
/// missing config value — and is deliberately distinct from `Ok(false)`.
pub fn is_on_call(auth: &AlertsAuth, schedule_id: &str, account_id: &str) -> Result<bool> {
    if !auth.is_complete() {
        return Err(AppError::InvalidArgument(
            "on-call: email, token and cloud id must all be set".to_string(),
        ));
    }
    if schedule_id.trim().is_empty() || account_id.trim().is_empty() {
        return Err(AppError::InvalidArgument(
            "on-call: schedule_id and account_id must be set in features.json".to_string(),
        ));
    }
    let body = crate::alerts::fetch_on_calls(auth, schedule_id.trim())?;
    Ok(response_mentions(&body, account_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "557058:abc-123";

    #[test]
    fn the_id_is_found_wherever_it_sits_in_the_response() {
        // Matched anywhere rather than at a guessed path, because the
        // on-call response shape has changed across API versions.
        let flat = format!(r#"{{"data":{{"onCallParticipants":["{ME}"]}}}}"#);
        assert!(response_mentions(&flat, ME));

        let nested = format!(
            r#"{{"values":[{{"recipient":{{"id":"{ME}","type":"user"}}}}]}}"#
        );
        assert!(response_mentions(&nested, ME));

        let as_key_value = format!(r#"{{"a":{{"b":{{"c":"{ME}"}}}}}}"#);
        assert!(response_mentions(&as_key_value, ME));
    }

    #[test]
    fn somebody_else_on_call_is_not_a_match() {
        let body = r#"{"data":{"onCallParticipants":["557058:someone-else"]}}"#;
        assert!(!response_mentions(body, ME));
    }

    #[test]
    fn an_empty_rotation_is_not_a_match() {
        assert!(!response_mentions(r#"{"data":{"onCallParticipants":[]}}"#, ME));
    }

    #[test]
    fn a_partial_id_does_not_match() {
        // Substring matching would make "abc" match "abc-123".
        let body = format!(r#"{{"x":["{ME}"]}}"#);
        assert!(!response_mentions(&body, "557058:abc"));
    }

    #[test]
    fn an_unparseable_body_is_not_a_match() {
        assert!(!response_mentions("<html>403 Forbidden</html>", ME));
    }

    #[test]
    fn a_blank_account_id_never_matches() {
        // Otherwise a missing config value would report "on call" against
        // any response containing an empty string.
        let body = format!(r#"{{"x":["{ME}",""]}}"#);
        assert!(!response_mentions(&body, ""));
        assert!(!response_mentions(&body, "   "));
    }

    #[test]
    fn an_incomplete_auth_is_an_error_not_a_false() {
        // "the lookup failed" must stay distinguishable from "not on call":
        // they take the same path today but are logged differently.
        let auth = crate::alerts::AlertsAuth::default();
        assert!(is_on_call(&auth, "sched", "me").is_err());
    }

    #[test]
    fn a_blank_schedule_id_is_an_error() {
        let auth = crate::alerts::AlertsAuth {
            email: "a@b.c".into(),
            token: "t".into(),
            cloud_id: "cloud".into(),
        };
        assert!(is_on_call(&auth, "", "me").is_err());
        assert!(is_on_call(&auth, "sched", "").is_err());
    }
}

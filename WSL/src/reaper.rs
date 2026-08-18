//! Deciding whether an alert is a reaper alert, and what to do about it.
//!
//! Everything in this module is pure — alerts in, decisions out — so the
//! matcher can be exercised against captured payloads with no AWS, no
//! network and no GUI. That matters more here than usual: a wrong matcher
//! runs `compose down` on a box that had nothing wrong with it.

use crate::alerts::Alert;
use crate::features::ReaperFeature;

/// Extra-properties keys checked for an instance id, in order. Free text is
/// the fallback because a template edit can move it.
const INSTANCE_ID_KEYS: [&str; 4] = ["InstanceId", "instanceId", "instance_id", "Instance"];

/// The `{{extraProperties}}` key is an unrendered template holding a
/// flattened copy of the whole map. Matching it would double-count and would
/// fire on alerts that merely mention reaper inside that copy.
const JUNK_EXTRA_KEY: &str = "{{extraProperties}}";

// Not `#[allow(dead_code)]`: the point of this constant is that the matcher
// itself never reads it (see `match_alert`, which only ever looks up
// `alertname` by name). The one real usage lives in the test that covers the
// hazard, which is invisible to a non-test `cargo build`, so this const-eval
// assertion gives the constant a use outside `#[cfg(test)]` without changing
// any runtime behaviour — a lint workaround that also happens to double as a
// standing invariant check.
const _: () = assert!(!JUNK_EXTRA_KEY.is_empty());

/// What a matched reaper alert tells us to act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub alert_id: String,
    pub instance_id: String,
    pub account_id: String,
    pub environment: String,
}

/// `true` when `hay` contains `needle`, case-insensitively. A blank needle
/// matches nothing — an unconfigured rule must not match everything.
fn contains_ci(hay: &str, needle: &str) -> bool {
    let n = needle.trim();
    if n.is_empty() {
        return false;
    }
    hay.to_ascii_lowercase().contains(&n.to_ascii_lowercase())
}

/// The first EC2 instance id in `text`, or `None`.
///
/// Hand-written rather than a regex: this crate has no regex dependency and
/// adding one for eight lines is not worth the tree. Ids are `i-` followed by
/// exactly 8 or 17 lowercase hex digits, and must stand alone as a token.
pub fn find_instance_id(text: &str) -> Option<String> {
    let b = text.as_bytes();
    let is_hex = |c: u8| c.is_ascii_digit() || (b'a'..=b'f').contains(&c);
    let is_word = |c: u8| c.is_ascii_alphanumeric() || c == b'_';

    let mut i = 0usize;
    while i + 2 < b.len() {
        if b[i] == b'i' && b[i + 1] == b'-' && (i == 0 || !is_word(b[i - 1])) {
            let start = i + 2;
            let mut end = start;
            while end < b.len() && is_hex(b[end]) {
                end += 1;
            }
            let len = end - start;
            let terminated = end == b.len() || !is_word(b[end]);
            if (len == 8 || len == 17) && terminated {
                return Some(text[i..end].to_string());
            }
        }
        i += 1;
    }
    None
}

/// Is this alert one of ours, and if so what does it point at?
///
/// Identification prefers `extraProperties.alertname`, then the `App:` tag,
/// then `message`. The App tag is measurably unreliable — a live pull found
/// two of ten alerts carrying an unrendered `&{%…%}%` template and two more
/// with no App tag at all — so a garbage or absent value is "no
/// information", never a non-match.
pub fn match_alert(alert: &Alert, cfg: &ReaperFeature) -> Option<Target> {
    let alertname = alert.extra.get("alertname").map(String::as_str).unwrap_or("");
    let identified = contains_ci(alertname, &cfg.alertname_contains)
        || contains_ci(&alert.app, &cfg.app_contains)
        || contains_ci(&alert.message, &cfg.message_contains);
    if !identified {
        return None;
    }

    let instance_id = INSTANCE_ID_KEYS
        .iter()
        .filter_map(|k| alert.extra.get(*k))
        .find_map(|v| find_instance_id(v))
        .or_else(|| {
            // Free text, in the order a human would read it.
            find_instance_id(&alert.description).or_else(|| find_instance_id(&alert.message))
        })?;

    Some(Target {
        alert_id: alert.id.clone(),
        instance_id,
        account_id: alert.account.clone(),
        environment: alert.environment.clone(),
    })
}

pub const RE_BEGIN: &str = "__RE_BEGIN__";
pub const RE_END: &str = "__RE_END__";
pub const RE_NODIR: &str = "__RE_NODIR__";
pub const RE_PS_BEGIN: &str = "__RE_PS_BEGIN__";
pub const RE_PS_END: &str = "__RE_PS_END__";

/// What the remote run proved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// `compose ps` reported every service up.
    Success,
    /// The run completed and the stack is not up.
    Failed(String),
    /// The run's outcome could not be established. Never treated as success,
    /// and reported separately from `Failed` so nobody is sent to debug a fix
    /// that may well have worked.
    Indeterminate(String),
}

/// Is one `compose ps` record a running service?
fn record_is_running(v: &serde_json::Value) -> bool {
    let state = v.get("State").and_then(|s| s.as_str()).unwrap_or("");
    if state.eq_ignore_ascii_case("running") {
        return true;
    }
    // Some compose versions put "Up 3 seconds" in Status and a lifecycle
    // word in State.
    v.get("Status")
        .and_then(|s| s.as_str())
        .map(|s| s.trim_start().starts_with("Up"))
        .unwrap_or(false)
}

fn record_name(v: &serde_json::Value) -> String {
    v.get("Name")
        .or_else(|| v.get("Service"))
        .and_then(|s| s.as_str())
        .unwrap_or("(unnamed)")
        .to_string()
}

/// Decide what the remote output proves.
///
/// `compose ps` is the authority, not the per-step markers: a 30s `timeout`
/// can kill an `up -d` that in fact succeeded a moment later, so treating
/// `__RE_UP_FAIL__` as the verdict would escalate a working fix.
pub fn parse_verdict(output: &str) -> Verdict {
    if !output.contains(RE_BEGIN) {
        return Verdict::Indeterminate(
            "no begin marker — the script did not start".to_string(),
        );
    }
    if !output.contains(RE_END) {
        return Verdict::Indeterminate(
            "no end marker — the run was cut off before it finished".to_string(),
        );
    }
    if output.contains(RE_NODIR) {
        return Verdict::Failed("/opt/reaper is not present on this instance".to_string());
    }

    let Some(block) = output
        .split_once(RE_PS_BEGIN)
        .and_then(|(_, rest)| rest.split_once(RE_PS_END))
        .map(|(inner, _)| inner)
    else {
        return Verdict::Indeterminate("no compose ps block in the output".to_string());
    };

    let mut records: Vec<serde_json::Value> = Vec::new();
    let mut unparsed = 0usize;
    for line in block.lines().map(str::trim).filter(|l| !l.is_empty()) {
        match serde_json::from_str::<serde_json::Value>(line) {
            Ok(serde_json::Value::Array(items)) => records.extend(items),
            Ok(v) => records.push(v),
            Err(_) => unparsed += 1,
        }
    }

    if records.is_empty() {
        return if unparsed > 0 {
            Verdict::Indeterminate(format!("compose ps output unreadable ({unparsed} lines)"))
        } else {
            Verdict::Failed("compose ps listed no services — nothing came back up".to_string())
        };
    }

    let down: Vec<String> = records
        .iter()
        .filter(|r| !record_is_running(r))
        .map(record_name)
        .collect();

    if down.is_empty() {
        Verdict::Success
    } else {
        Verdict::Failed(format!("not running after restart: {}", down.join(", ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alerts::Alert;
    use crate::features::ReaperFeature;

    fn cfg() -> ReaperFeature {
        ReaperFeature {
            alertname_contains: "reaper".to_string(),
            app_contains: "reaper".to_string(),
            message_contains: "reaper".to_string(),
            ..Default::default()
        }
    }

    fn alert() -> Alert {
        let mut a = Alert {
            id: "alert-1".to_string(),
            status: "open".to_string(),
            account: "123456789012".to_string(),
            environment: "DEV1".to_string(),
            description: "reaper stalled on i-0abc123def4567890".to_string(),
            ..Default::default()
        };
        a.extra.insert("alertname".to_string(), "reaper-stalled".to_string());
        a
    }

    #[test]
    fn a_matching_alert_yields_the_full_target() {
        let t = match_alert(&alert(), &cfg()).expect("matches");
        assert_eq!(t.alert_id, "alert-1");
        assert_eq!(t.instance_id, "i-0abc123def4567890");
        assert_eq!(t.account_id, "123456789012");
        assert_eq!(t.environment, "DEV1");
    }

    #[test]
    fn an_unrelated_alert_matches_nothing() {
        let mut a = alert();
        a.extra.clear();
        a.description = "disk full on i-0abc123def4567890".to_string();
        assert!(match_alert(&a, &cfg()).is_none());
    }

    #[test]
    fn a_match_with_no_instance_id_yields_none_not_a_partial_target() {
        let mut a = alert();
        a.description = "reaper stalled somewhere".to_string();
        assert!(match_alert(&a, &cfg()).is_none());
    }

    #[test]
    fn an_unrendered_app_template_is_not_treated_as_a_name() {
        // Observed live: two of ten alerts carried this literal string.
        let mut a = alert();
        a.extra.clear();
        a.app = "&{%&{% extraProperties['App'] &}%&}%".to_string();
        assert!(match_alert(&a, &cfg()).is_none());
    }

    #[test]
    fn a_missing_app_tag_does_not_veto_an_alertname_match() {
        // Two of ten live alerts had no App tag at all. Absence is "no
        // information", never a non-match.
        let mut a = alert();
        a.app = String::new();
        assert!(match_alert(&a, &cfg()).is_some());
    }

    #[test]
    fn the_junk_extra_properties_key_is_never_matched_against() {
        // `{{extraProperties}}` holds a flattened copy of the whole map, so
        // matching it would fire on alerts that only mention reaper inside
        // an unrendered template.
        let mut a = alert();
        a.extra.clear();
        a.description = "something on i-0abc123def4567890".to_string();
        a.extra.insert(
            JUNK_EXTRA_KEY.to_string(),
            "{alertname=reaper-stalled}".to_string(),
        );
        assert!(match_alert(&a, &cfg()).is_none());
    }

    #[test]
    fn a_blank_rule_never_matches_everything() {
        // An unconfigured *_contains must match nothing, not match all.
        let empty = ReaperFeature::default();
        assert!(match_alert(&alert(), &empty).is_none());
    }

    #[test]
    fn matching_is_case_insensitive() {
        let mut a = alert();
        a.extra.insert("alertname".to_string(), "REAPER-Stalled".to_string());
        assert!(match_alert(&a, &cfg()).is_some());
    }

    #[test]
    fn the_instance_id_is_read_from_extra_properties_in_preference() {
        // Free text is one template edit away from moving.
        let mut a = alert();
        a.extra.insert("InstanceId".to_string(), "i-0fff1111222233334".to_string());
        let t = match_alert(&a, &cfg()).expect("matches");
        assert_eq!(t.instance_id, "i-0fff1111222233334");
    }

    #[test]
    fn instance_ids_are_found_in_both_lengths_and_nothing_else() {
        assert_eq!(find_instance_id("on i-0abc1234 now"), Some("i-0abc1234".into()));
        assert_eq!(
            find_instance_id("on i-0abc123def4567890."),
            Some("i-0abc123def4567890".into())
        );
        // Wrong length.
        assert_eq!(find_instance_id("i-0abc12"), None);
        assert_eq!(find_instance_id("i-0abc123def456789012"), None);
        // Not hex.
        assert_eq!(find_instance_id("i-0abczzzz"), None);
        // Not a standalone token.
        assert_eq!(find_instance_id("xi-0abc1234"), None);
        assert_eq!(find_instance_id("i-0abc1234z"), None);
        assert_eq!(find_instance_id("no id here"), None);
    }

    #[test]
    fn an_uppercase_instance_id_is_not_accepted() {
        // EC2 ids are lowercase hex; accepting uppercase would match
        // unrelated identifiers that happen to start "I-".
        assert_eq!(find_instance_id("i-0ABC1234"), None);
    }

    fn run_output(ps: &str) -> String {
        format!(
            "__RE_BEGIN__\n__RE_WD_STOPPED__\n__RE_DOWN_OK__\n__RE_UP_OK__\n\
             __RE_PS_BEGIN__\n{ps}\n__RE_PS_END__\n__RE_END__\n"
        )
    }

    #[test]
    fn every_service_running_is_a_success() {
        let ps = r#"{"Name":"reaper-api","State":"running"}
{"Name":"reaper-worker","State":"running"}"#;
        assert_eq!(parse_verdict(&run_output(ps)), Verdict::Success);
    }

    #[test]
    fn a_json_array_from_compose_is_accepted_too() {
        // Older compose emits one array; newer emits NDJSON. Both are real.
        let ps = r#"[{"Name":"reaper-api","State":"running"}]"#;
        assert_eq!(parse_verdict(&run_output(ps)), Verdict::Success);
    }

    #[test]
    fn an_up_status_string_counts_as_running() {
        let ps = r#"{"Name":"reaper-api","State":"exited","Status":"Up 3 seconds"}"#;
        assert_eq!(parse_verdict(&run_output(ps)), Verdict::Success);
    }

    #[test]
    fn one_service_not_running_is_a_failure_naming_it() {
        let ps = r#"{"Name":"reaper-api","State":"running"}
{"Name":"reaper-worker","State":"exited"}"#;
        match parse_verdict(&run_output(ps)) {
            Verdict::Failed(why) => assert!(why.contains("reaper-worker"), "got {why}"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn no_services_at_all_is_a_failure() {
        // `compose ps` succeeding with an empty list means nothing came back up.
        assert!(matches!(parse_verdict(&run_output("")), Verdict::Failed(_)));
    }

    #[test]
    fn a_missing_end_marker_is_indeterminate_never_success() {
        // The command was cut off — by the send-command timeout, a dropped
        // session, the box dying. Reporting success here is the one outcome
        // that must be impossible.
        let cut = "__RE_BEGIN__\n__RE_WD_STOPPED__\n__RE_DOWN_OK__\n";
        assert!(matches!(parse_verdict(cut), Verdict::Indeterminate(_)));
    }

    #[test]
    fn a_missing_begin_marker_is_indeterminate() {
        assert!(matches!(parse_verdict("random ssm noise"), Verdict::Indeterminate(_)));
    }

    #[test]
    fn a_missing_directory_is_a_failure_not_a_success() {
        let out = "__RE_BEGIN__\n__RE_NODIR__\n__RE_END__\n";
        match parse_verdict(out) {
            Verdict::Failed(why) => assert!(why.contains("/opt/reaper"), "got {why}"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn unparseable_compose_output_is_indeterminate_not_failed() {
        // Escalating "the fix failed" when we only failed to *read* the result
        // sends someone to debug the wrong thing.
        let out = run_output("Error response from daemon: something");
        assert!(matches!(parse_verdict(&out), Verdict::Indeterminate(_)));
    }

    #[test]
    fn a_timeout_killed_up_still_passes_when_compose_ps_says_running() {
        // 30s `timeout` can kill an `up -d` that succeeded a moment later.
        // `compose ps` is the authority, not the step marker — the same rule as
        // Tunnel::is_bound: ask the thing itself.
        let out = format!(
            "__RE_BEGIN__\n__RE_WD_STOPPED__\n__RE_DOWN_OK__\n__RE_UP_FAIL__\n\
             __RE_PS_BEGIN__\n{}\n__RE_PS_END__\n__RE_END__\n",
            r#"{"Name":"reaper-api","State":"running"}"#
        );
        assert_eq!(parse_verdict(&out), Verdict::Success);
    }

    #[test]
    fn a_failed_watchdog_stop_does_not_by_itself_fail_the_run() {
        // The watchdog may already be stopped. What matters is the stack.
        let out = format!(
            "__RE_BEGIN__\n__RE_WD_FAIL__\n__RE_DOWN_OK__\n__RE_UP_OK__\n\
             __RE_PS_BEGIN__\n{}\n__RE_PS_END__\n__RE_END__\n",
            r#"{"Name":"reaper-api","State":"running"}"#
        );
        assert_eq!(parse_verdict(&out), Verdict::Success);
    }
}

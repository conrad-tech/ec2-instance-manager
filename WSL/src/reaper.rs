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
/// fire on alerts that merely mention reaper inside that copy. Only used by
/// the tests that cover that hazard — see `match_alert`'s doc comment for the
/// production-code version of this note.
#[cfg(test)]
const JUNK_EXTRA_KEY: &str = "{{extraProperties}}";

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
///
/// Identification deliberately reads only the `alertname` key by name, never
/// the whole `extraProperties` map: the feed carries a literal
/// `{{extraProperties}}` key holding a flattened copy of that whole map, and
/// matching against it would fire on alerts that merely mention this app
/// inside an unrendered template.
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

    // Exactly one of each, or the block's boundaries are not trustworthy.
    // A stray end marker — echoed by a container log line, or by shell
    // tracing — would truncate the block and hide a failing service behind
    // a prefix of healthy ones.
    let begins = output.matches(RE_PS_BEGIN).count();
    let ends = output.matches(RE_PS_END).count();
    if begins != 1 || ends != 1 {
        return Verdict::Indeterminate(format!(
            "compose ps block markers appeared {begins}x/{ends}x, expected 1x/1x"
        ));
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

    // A line we could not read is a service whose state we do not know.
    // Judging the stack from the lines that happened to parse would report
    // a box as fixed on partial evidence — the one outcome that must be
    // impossible here. Note this is Indeterminate, not Failed: we did not
    // observe a failure, we failed to observe.
    if unparsed > 0 {
        return Verdict::Indeterminate(format!(
            "compose ps output partly unreadable ({unparsed} line(s)); \
             stack state not established"
        ));
    }

    if records.is_empty() {
        return Verdict::Failed("compose ps listed no services — nothing came back up".to_string());
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

use std::collections::{HashMap, HashSet};

/// What this run of the app has already acted on.
///
/// In memory only, deliberately. Which alerts this process has handled is a
/// fact about this process; persisting it would mean a stale file could
/// suppress a real remediation, which is the worse failure. A duplicate after
/// a restart is absorbed by the last-look check whenever the alert has since
/// closed.
#[derive(Debug, Default)]
pub struct ReaperState {
    handled: HashSet<String>,
    last_action_ms: HashMap<String, u64>,
}

impl ReaperState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Two independent rules: an alert is acted on at most once ever, and an
    /// instance at most once per cooldown window.
    pub fn should_act(
        &self,
        alert_id: &str,
        instance_id: &str,
        now_ms: u64,
        cooldown_ms: u64,
    ) -> bool {
        if self.handled.contains(alert_id) {
            return false;
        }
        match self.last_action_ms.get(instance_id) {
            Some(&last) => now_ms.saturating_sub(last) >= cooldown_ms,
            None => true,
        }
    }

    pub fn mark_handled(&mut self, alert_id: &str, instance_id: &str, now_ms: u64) {
        self.handled.insert(alert_id.to_string());
        self.last_action_ms.insert(instance_id.to_string(), now_ms);
    }
}

/// The only status that stands a remediation down.
///
/// `acked` is a real third value on this tenant, and the on-call path
/// acknowledges the alert itself before running the fix — so anything
/// phrased as "not open" would make the app skip its own work.
pub fn alert_is_closed(status: &str) -> bool {
    status.trim().eq_ignore_ascii_case("closed")
}

/// What leaves the org. The code is the entire payload — the body is empty.
///
/// No instance id, account number, environment or product name. Enough for
/// the notifier to pick a tier and enough for the user to know what it is
/// about; the detail stays in the app log and in Jira, on the corporate side
/// of the line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutcomeCode {
    /// On call: escalate — calls and messages until acknowledged.
    Failure,
    /// Off call: one message, no calls. The official page is already running
    /// for whoever does hold the pager.
    FailureQuiet,
    /// Both stages passed. One quiet message: a box is running with its
    /// watchdog deliberately left stopped.
    Ok,
    /// Daily canary — proof the whole chain is alive.
    Canary,
}

impl OutcomeCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Failure => "RE-F",
            Self::FailureQuiet => "RE-N",
            Self::Ok => "RE-K",
            Self::Canary => "RE-C",
        }
    }

    /// True only for the tier that rings a phone.
    pub fn is_escalation(&self) -> bool {
        matches!(self, Self::Failure)
    }
}

/// Which code this remediation earned.
///
/// The alert closing outranks everything: it is the symptom going away, which
/// is the thing actually being asked about. Otherwise anything short of a
/// clean success reports failure, at the tier the on-call state selects.
pub fn decide_outcome(on_call: bool, verdict: &Verdict, closed_in_window: bool) -> OutcomeCode {
    if closed_in_window && !matches!(verdict, Verdict::Failed(_)) {
        return OutcomeCode::Ok;
    }
    if on_call {
        OutcomeCode::Failure
    } else {
        OutcomeCode::FailureQuiet
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
    fn an_instance_id_inside_the_junk_key_is_never_used() {
        // The alert IS legitimately identified here, so the matcher reaches the
        // instance-id search. `{{extraProperties}}` holds a flattened copy of
        // the whole map and may therefore contain a real instance id — reading
        // it would remediate a box named only by an unrendered template.
        let mut a = alert();
        a.description = "reaper stalled".to_string(); // identified, but no id
        a.message = String::new();
        a.extra.insert(
            JUNK_EXTRA_KEY.to_string(),
            "{alertname=reaper-stalled, InstanceId=i-0abc123def4567890}".to_string(),
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

    #[test]
    fn find_instance_id_never_panics_on_degenerate_input() {
        // This function slices &str by BYTE index. Every index it advances past
        // is matched as ASCII ('i', '-', hex, word-class), so a multi-byte code
        // point can never land inside a slice — but nothing proved that until
        // now, and the loop is exactly the kind of code a later refactor breaks.
        assert_eq!(find_instance_id(""), None);
        assert_eq!(find_instance_id("i"), None);
        assert_eq!(find_instance_id("i-"), None);
        assert_eq!(find_instance_id("i-0abc12"), None); // ends mid-token
        assert_eq!(find_instance_id("café"), None);
        assert_eq!(find_instance_id("日本語のテキスト"), None);
        // A valid id surrounded by multi-byte text is still found intact.
        assert_eq!(
            find_instance_id("café i-0abc1234 café"),
            Some("i-0abc1234".to_string())
        );
        // A multi-byte character immediately adjacent on both sides.
        assert_eq!(find_instance_id("é-0abc1234"), None);
    }

    fn run_output(ps: &str) -> String {
        format!(
            "{RE_BEGIN}\n__RE_WD_STOPPED__\n__RE_DOWN_OK__\n__RE_UP_OK__\n\
             {RE_PS_BEGIN}\n{ps}\n{RE_PS_END}\n{RE_END}\n"
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
        let cut = format!("{RE_BEGIN}\n__RE_WD_STOPPED__\n__RE_DOWN_OK__\n");
        assert!(matches!(parse_verdict(&cut), Verdict::Indeterminate(_)));
    }

    #[test]
    fn a_missing_begin_marker_is_indeterminate() {
        assert!(matches!(parse_verdict("random ssm noise"), Verdict::Indeterminate(_)));
    }

    #[test]
    fn a_missing_directory_is_a_failure_not_a_success() {
        let out = format!("{RE_BEGIN}\n{RE_NODIR}\n{RE_END}\n");
        match parse_verdict(&out) {
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
            "{RE_BEGIN}\n__RE_WD_STOPPED__\n__RE_DOWN_OK__\n__RE_UP_FAIL__\n\
             {RE_PS_BEGIN}\n{}\n{RE_PS_END}\n{RE_END}\n",
            r#"{"Name":"reaper-api","State":"running"}"#
        );
        assert_eq!(parse_verdict(&out), Verdict::Success);
    }

    #[test]
    fn a_failed_watchdog_stop_does_not_by_itself_fail_the_run() {
        // The watchdog may already be stopped. What matters is the stack.
        let out = format!(
            "{RE_BEGIN}\n__RE_WD_FAIL__\n__RE_DOWN_OK__\n__RE_UP_OK__\n\
             {RE_PS_BEGIN}\n{}\n{RE_PS_END}\n{RE_END}\n",
            r#"{"Name":"reaper-api","State":"running"}"#
        );
        assert_eq!(parse_verdict(&out), Verdict::Success);
    }

    #[test]
    fn a_partly_unreadable_compose_block_is_indeterminate_not_success() {
        // One service parses and is up; the other line is garbage. Judging from
        // the readable half alone would report the stack as fixed while a
        // service's state was never established.
        let ps = "{\"Name\":\"reaper-api\",\"State\":\"running\"}\nreaper-worker exited(1";
        assert!(matches!(parse_verdict(&run_output(ps)), Verdict::Indeterminate(_)));
    }

    #[test]
    fn a_repeated_ps_end_marker_is_indeterminate_not_a_truncated_success() {
        // A stray end marker before the real one would otherwise truncate the
        // block, hiding the failing service that follows it. The doubled
        // marker here is the point of the test, so it stays literal rather
        // than interpolating RE_PS_END once.
        let out = format!(
            "__RE_BEGIN__\n__RE_PS_BEGIN__\n{}\n__RE_PS_END__\n{}\n__RE_PS_END__\n__RE_END__\n",
            r#"{"Name":"reaper-api","State":"running"}"#,
            r#"{"Name":"reaper-worker","State":"exited"}"#
        );
        assert!(matches!(parse_verdict(&out), Verdict::Indeterminate(_)));
    }

    const COOLDOWN: u64 = 30 * 60 * 1000;

    #[test]
    fn a_fresh_alert_on_a_fresh_instance_is_acted_on() {
        let s = ReaperState::new();
        assert!(s.should_act("a1", "i-0abc1234", 0, COOLDOWN));
    }

    #[test]
    fn the_same_alert_is_never_acted_on_twice() {
        let mut s = ReaperState::new();
        s.mark_handled("a1", "i-0abc1234", 0);
        assert!(!s.should_act("a1", "i-0abc1234", 0, COOLDOWN));
        // Still refused long after the instance cooldown has expired: once per
        // alert is a separate rule from once per instance per window.
        assert!(!s.should_act("a1", "i-0abc1234", 10 * COOLDOWN, COOLDOWN));
    }

    #[test]
    fn a_different_alert_on_the_same_instance_waits_for_the_cooldown() {
        let mut s = ReaperState::new();
        s.mark_handled("a1", "i-0abc1234", 0);
        assert!(!s.should_act("a2", "i-0abc1234", COOLDOWN - 1, COOLDOWN));
        assert!(s.should_act("a2", "i-0abc1234", COOLDOWN, COOLDOWN));
    }

    #[test]
    fn the_cooldown_does_not_suppress_a_different_instance() {
        let mut s = ReaperState::new();
        s.mark_handled("a1", "i-0abc1234", 0);
        assert!(s.should_act("a2", "i-0fff5678", 1, COOLDOWN));
    }

    #[test]
    fn only_closed_stands_a_remediation_down() {
        // `acked` is a real third status on this tenant. A rule written as
        // `status != "open"` would skip every acknowledged alert — including the
        // ones this app acknowledges itself.
        assert!(alert_is_closed("closed"));
        assert!(alert_is_closed("CLOSED"));
        assert!(!alert_is_closed("open"));
        assert!(!alert_is_closed("acked"));
        assert!(!alert_is_closed("acknowledged"));
        assert!(!alert_is_closed(""));
    }

    #[test]
    fn a_status_with_surrounding_whitespace_is_still_recognised() {
        // The status is compared after trimming. Nothing tested that, so
        // dropping the trim would silently make a closed alert look open —
        // and an alert that looks open is one this app will remediate.
        assert!(alert_is_closed(" closed "));
        assert!(alert_is_closed("closed\n"));
        assert!(alert_is_closed("\tCLOSED\r\n"));
        // Whitespace must not turn a non-closed status into a closed one.
        assert!(!alert_is_closed("  acked  "));
        assert!(!alert_is_closed("   "));
    }

    #[test]
    fn a_fix_that_worked_and_closed_is_the_quiet_success_code() {
        assert_eq!(decide_outcome(true, &Verdict::Success, true), OutcomeCode::Ok);
        assert_eq!(decide_outcome(false, &Verdict::Success, true), OutcomeCode::Ok);
    }

    #[test]
    fn on_call_failures_escalate_and_off_call_failures_do_not() {
        let failed = Verdict::Failed("nope".into());
        assert_eq!(decide_outcome(true, &failed, false), OutcomeCode::Failure);
        assert_eq!(decide_outcome(false, &failed, false), OutcomeCode::FailureQuiet);
        assert!(OutcomeCode::Failure.is_escalation());
        assert!(!OutcomeCode::FailureQuiet.is_escalation());
        assert!(!OutcomeCode::Ok.is_escalation());
        assert!(!OutcomeCode::Canary.is_escalation());
    }

    #[test]
    fn a_stack_that_came_up_but_whose_alert_never_closed_is_a_failure() {
        // Stage 2 exists precisely for this: the stack is up and the symptom
        // is not gone.
        assert_eq!(decide_outcome(true, &Verdict::Success, false), OutcomeCode::Failure);
        assert_eq!(decide_outcome(false, &Verdict::Success, false), OutcomeCode::FailureQuiet);
    }

    #[test]
    fn indeterminate_is_reported_as_a_failure_never_as_success() {
        let unknown = Verdict::Indeterminate("cut off".into());
        assert_eq!(decide_outcome(true, &unknown, false), OutcomeCode::Failure);
        assert_eq!(decide_outcome(false, &unknown, false), OutcomeCode::FailureQuiet);
    }

    #[test]
    fn an_indeterminate_run_whose_alert_closed_is_still_a_success() {
        // The alert closing is the symptom going away. That outranks our
        // inability to read the compose output.
        let unknown = Verdict::Indeterminate("cut off".into());
        assert_eq!(decide_outcome(true, &unknown, true), OutcomeCode::Ok);
    }

    #[test]
    fn an_indeterminate_run_that_closed_is_a_success_off_call_too() {
        // Completes the truth table: closure outranks an unreadable verdict
        // regardless of who holds the pager.
        let unknown = Verdict::Indeterminate("cut off".into());
        assert_eq!(decide_outcome(false, &unknown, true), OutcomeCode::Ok);
    }

    #[test]
    fn a_known_failure_is_not_excused_by_the_alert_closing() {
        // The two rules conflict only here. "Closed" normally wins, because the
        // symptom going away is the real question — but not when we positively
        // observed the stack fail to come back. Something else closed that
        // alert, and a failed remediation must still be reported.
        let failed = Verdict::Failed("reaper-worker not running".into());
        assert_eq!(decide_outcome(true, &failed, true), OutcomeCode::Failure);
        assert_eq!(decide_outcome(false, &failed, true), OutcomeCode::FailureQuiet);
    }

    #[test]
    fn the_wire_codes_carry_nothing_identifying() {
        // The subject is the entire payload. No instance, account, environment,
        // or product name may appear in it.
        for c in [OutcomeCode::Failure, OutcomeCode::FailureQuiet, OutcomeCode::Ok, OutcomeCode::Canary] {
            let s = c.as_str();
            assert!(s.starts_with("RE-"), "{s}");
            assert_eq!(s.len(), 4, "{s}");
            assert!(!s.to_ascii_lowercase().contains("reaper"), "{s}");
        }
        assert_eq!(OutcomeCode::Failure.as_str(), "RE-F");
        assert_eq!(OutcomeCode::FailureQuiet.as_str(), "RE-N");
        assert_eq!(OutcomeCode::Ok.as_str(), "RE-K");
        assert_eq!(OutcomeCode::Canary.as_str(), "RE-C");
    }

    /// The script and these constants live in different files and are matched by
    /// string. `secondary_mirror_step_matches_its_wait` exists because exactly
    /// that pairing drifted apart once already.
    const REAPER_FIX_SH: &str = include_str!("../assets/scripts/reaper_fix.sh");

    #[test]
    fn the_script_emits_every_marker_the_parser_looks_for() {
        for m in [RE_BEGIN, RE_END, RE_NODIR, RE_PS_BEGIN, RE_PS_END] {
            assert!(REAPER_FIX_SH.contains(m), "script never emits {m}");
        }
    }

    #[test]
    fn the_script_checks_the_directory_before_stopping_the_watchdog() {
        // Wrong box: a no-op must not leave self-healing switched off.
        let dir_check = REAPER_FIX_SH.find("-d /opt/reaper").expect("has the check");
        let wd_stop = REAPER_FIX_SH.find("stop reaper-watchdog").expect("stops the watchdog");
        assert!(dir_check < wd_stop, "the directory check must come first");
    }

    #[test]
    fn the_script_never_sets_e() {
        // `set -e` would exit on a failing `down` and never reach `up -d`,
        // stranding reaper stopped with its watchdog off — the one state this
        // whole design exists to avoid.
        assert!(!REAPER_FIX_SH.contains("set -e"));
        assert!(REAPER_FIX_SH.contains("set -u"));
    }

    #[test]
    fn the_script_brings_the_stack_up_detached() {
        // A bare `docker compose up` never returns; it would hang to the
        // send-command timeout and be reported as a failure *because it worked*.
        assert!(REAPER_FIX_SH.contains("compose up -d"));
        for line in REAPER_FIX_SH.lines() {
            let l = line.trim();
            if l.starts_with('#') {
                continue;
            }
            if l.contains("compose up") {
                assert!(l.contains("-d"), "non-detached up: {l}");
            }
        }
    }

    #[test]
    fn the_script_never_restarts_the_watchdog() {
        // Leaving it stopped is deliberate: it is what makes a *successful* fix
        // worth reporting at all.
        assert!(!REAPER_FIX_SH.contains("start reaper-watchdog"));
        assert!(!REAPER_FIX_SH.contains("restart reaper-watchdog"));
    }

    #[test]
    fn the_script_asks_compose_for_json() {
        assert!(REAPER_FIX_SH.contains("compose ps --format json"));
    }
}

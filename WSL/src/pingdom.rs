//! Deciding what to do about a pingdom alert.
//!
//! Everything in this module is pure — alerts in, decisions out — so the
//! rules can be exercised against captured payloads with no JSM, no network
//! and no GUI. Two of the three things this feature does are irreversible
//! from the app's point of view: acknowledging silences a live page, and
//! escalating rings a phone.
//!
//! The shape of the job, and all of it:
//!
//! 1. acknowledge a pingdom alert the moment it is seen,
//! 2. wait for it to close,
//! 3. escalate if it is still open when the window runs out.
//!
//! There is no remediation here and nothing ever touches an instance, which
//! is why this is its own module rather than a second `FixKind` in
//! [`crate::reaper`].

use std::collections::{HashMap, HashSet};

use crate::alerts::Alert;
use crate::features::PingdomFeature;

/// `true` when `hay` contains `needle`, case-insensitively. A blank needle
/// matches nothing — an unconfigured rule must not match every alert on the
/// feed and start acknowledging them.
fn contains_ci(hay: &str, needle: &str) -> bool {
    let n = needle.trim();
    if n.is_empty() {
        return false;
    }
    hay.to_ascii_lowercase().contains(&n.to_ascii_lowercase())
}

/// Characters that mean a token came out of an unrendered template rather
/// than out of a real alert title.
///
/// This feed has been observed serving both `{{…}}` and `&{%…%}%` in fields
/// that should have held values — a live pull found two of ten alerts with a
/// templated `App:` tag. Keying an incident on such a token would file
/// unrelated outages under one environment and suppress all but the first.
const TEMPLATE_MARKERS: [char; 5] = ['{', '}', '%', '<', '>'];

/// How the environment was derived, so the log can say which rule produced
/// the value and the markers can be tuned from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnvSource {
    /// The text after this configured `environment_after` marker.
    AfterMarker(String),
    /// No marker matched, so the last word of the summary was used.
    LastWord,
    /// Nothing usable was found.
    Unreadable,
}

impl EnvSource {
    /// A phrase for the log line.
    pub fn describe(&self) -> String {
        match self {
            Self::AfterMarker(m) => format!("after marker '{m}'"),
            Self::LastWord => "last word of the summary".to_string(),
            Self::Unreadable => "could not be read from the summary".to_string(),
        }
    }
}

/// The environment a pingdom alert is about, read out of the alert summary
/// (`Alert.message`), plus how it was found.
///
/// Deliberately not the `Environment:` tag: pingdom alerts on this feed do
/// not carry one.
///
/// **`markers` is `pingdom.environment_after`, and is the rule that matters.**
/// The first marker in the list that appears in the summary wins — list
/// order, not the summary's word order — and the environment is *everything
/// after it*, trimmed. Everything after, rather than the next word, because
/// an environment here can be more than one word (`prod one`), and taking one
/// word would file `prod one` and `dev one` under the same `one`.
///
/// Markers match case-insensitively: summaries are free text and an app name
/// drifts in case the same way `MMODAL_ENV` does. A blank entry is skipped,
/// or it would match at offset 0 and make the whole summary the environment.
///
/// With no marker configured — or none present in this summary — it falls
/// back to the **last word**, which is what this parsed before markers
/// existed. That fallback cannot see a two-word environment; the log line
/// says `last word of the summary` when it happens, which is the signal to
/// add a marker.
///
/// `None` for a summary with nothing usable: one word, nothing after the
/// marker, or a value carrying template junk.
pub fn environment_from_title(message: &str, markers: &[String]) -> (Option<String>, EnvSource) {
    for marker in markers {
        let m = marker.trim();
        if m.is_empty() {
            continue;
        }
        let Some(at) = find_ci(message, m) else {
            continue;
        };
        let tail = message[at + m.len()..].trim();
        let source = EnvSource::AfterMarker(m.to_string());
        if tail.is_empty() || tail.contains(TEMPLATE_MARKERS) {
            return (None, EnvSource::Unreadable);
        }
        return (Some(tail.to_string()), source);
    }

    let words: Vec<&str> = message.split_whitespace().collect();
    // One word is the whole summary, not an environment named at the end of
    // one. Taking it would turn `[Pingdom]` itself into an environment.
    if words.len() < 2 {
        return (None, EnvSource::Unreadable);
    }
    let last = words[words.len() - 1];
    if last.contains(TEMPLATE_MARKERS) {
        return (None, EnvSource::Unreadable);
    }
    (Some(last.to_string()), EnvSource::LastWord)
}

/// Byte offset of the first case-insensitive occurrence of `needle`.
///
/// Hand-rolled over `to_ascii_lowercase` on both sides so the offset is valid
/// in the *original* string: lowercasing can change byte length in general,
/// and slicing the original by an index found in a transformed copy is how
/// that goes wrong. ASCII-only lowering keeps the two in step.
fn find_ci(hay: &str, needle: &str) -> Option<usize> {
    hay.to_ascii_lowercase().find(&needle.to_ascii_lowercase())
}

/// Is this alert one of ours?
///
/// `extraProperties.alertname` first, then the `App:` tag, then `message`.
/// The same order and the same reasoning as [`crate::reaper::identifies`]:
/// a garbage or absent field is "no information", never a non-match. For
/// this feed `message` is the rule that actually fires, since the title
/// carries the `[Pingdom]` marker.
///
/// Reads `alertname` by name and never scans the whole `extraProperties`
/// map: the feed carries a literal `{{extraProperties}}` key holding a
/// flattened copy of that map, and matching it would fire on alerts that
/// merely mention pingdom inside an unrendered template.
pub fn identifies(alert: &Alert, cfg: &PingdomFeature) -> bool {
    let alertname = alert.extra.get("alertname").map(String::as_str).unwrap_or("");
    contains_ci(alertname, &cfg.alertname_contains)
        || contains_ci(&alert.app, &cfg.app_contains)
        || contains_ci(&alert.message, &cfg.message_contains)
}

/// What two pingdom alerts have to share to be one incident.
///
/// Environment, wherever the title yields one — several pingdom checks
/// failing in one environment are one outage, and the first of them owns
/// the timer. An alert whose environment cannot be read falls back to its
/// own id, so it is an incident of one.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum IncidentKey {
    /// Upper-cased, because `MMODAL_ENV` is free text and this repo already
    /// has the scar: an account tagged `dev1` on some instances and `DEV1`
    /// on others got one pem entry but two bastion entries, which is why
    /// `bastion_key` upper-cases too.
    Environment(String),
    /// The alert's own id, for an alert naming no readable environment.
    Alert(String),
}

impl IncidentKey {
    /// What to call this incident in a log line.
    pub fn label(&self) -> String {
        match self {
            Self::Environment(e) => e.clone(),
            Self::Alert(id) => format!("alert {id} (no environment in the title)"),
        }
    }
}

/// The incident `alert` belongs to, and how its environment was read.
///
/// The key normalises **case and internal whitespace**, so `prod one`,
/// `PROD ONE` and `prod  one` are one environment. Both come from free text
/// somebody typed, and neither should split an incident in two.
pub fn incident_key_with_source(
    alert: &Alert,
    markers: &[String],
) -> (IncidentKey, EnvSource) {
    let (env, source) = environment_from_title(&alert.message, markers);
    let key = match env {
        Some(e) => IncidentKey::Environment(
            e.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_ascii_uppercase(),
        ),
        None => IncidentKey::Alert(alert.id.clone()),
    };
    (key, source)
}

/// The incident `alert` belongs to.
pub fn incident_key(alert: &Alert, markers: &[String]) -> IncidentKey {
    incident_key_with_source(alert, markers).0
}

/// What the caller should do about one alert.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Nothing. Already seen — acknowledged on an earlier poll, or watched,
    /// or recorded as a duplicate.
    Ignore,
    /// The first alert of an incident: acknowledge it and start its timer.
    AckAndWatch,
    /// Another report of an incident already being timed: acknowledge it so
    /// it stops ringing, and do nothing else. The first alert keeps its own
    /// deadline.
    AckOnly {
        /// The alert id that owns the incident, for the log line.
        owner: String,
    },
}

/// One incident whose window has run out. The payload that leaves the org is
/// built from this and carries nothing but a code and a timestamp.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Escalation {
    pub alert_id: String,
    /// `createdAt` from the alert, verbatim as the API returned it.
    pub created_at: String,
    /// What to call the incident in the log. Never sent anywhere.
    pub incident: String,
}

/// One incident being timed.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Watch {
    owner: String,
    created_at: String,
    deadline_ms: u64,
    escalated: bool,
}

/// What this process has acknowledged, is timing, and has escalated.
///
/// Owned by the poll thread and never shared, so there is no lock anywhere
/// here — the same property [`crate::reaper::ReaperState`] keeps.
#[derive(Debug, Default)]
pub struct PingdomState {
    /// One live incident per key. Its presence *is* that environment's
    /// escalation slot: while it is here, no other alert in that
    /// environment starts a timer or escalates.
    watching: HashMap<IncidentKey, Watch>,
    /// Every alert id already acted on, so a still-open alert is not
    /// acknowledged again on every poll.
    seen: HashSet<String>,
}

impl PingdomState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decide what to do about `alert`, and record the decision.
    ///
    /// Recording here rather than in the caller is deliberate, and matches
    /// `mark_handled`-before-spawn in reaper: an acknowledge that fails must
    /// not be retried on every poll for the rest of the run, and a dry run
    /// must log once rather than once per poll.
    pub fn consider(
        &mut self,
        alert: &Alert,
        markers: &[String],
        now_ms: u64,
        window_ms: u64,
    ) -> Action {
        if self.seen.contains(&alert.id) {
            return Action::Ignore;
        }
        self.seen.insert(alert.id.clone());

        let key = incident_key(alert, markers);
        if let Some(open) = self.watching.get(&key) {
            // Another report of an incident already being timed. The first
            // alert keeps its own deadline: refreshing it here would let a
            // trickle of duplicates hold the escalation off indefinitely.
            return Action::AckOnly { owner: open.owner.clone() };
        }

        self.watching.insert(
            key,
            Watch {
                owner: alert.id.clone(),
                created_at: alert.created_at.clone(),
                deadline_ms: now_ms.saturating_add(window_ms),
                escalated: false,
            },
        );
        Action::AckAndWatch
    }

    /// The alert ids being timed, which the caller re-reads each poll.
    ///
    /// Includes incidents that have already escalated: the slot is held
    /// until the alert actually closes, so its status still has to be
    /// watched.
    pub fn watched_alert_ids(&self) -> Vec<String> {
        self.watching.values().map(|w| w.owner.clone()).collect()
    }

    /// The incidents whose window has run out and which have not escalated.
    /// Marks them, so one incident escalates exactly once.
    pub fn due(&mut self, now_ms: u64) -> Vec<Escalation> {
        let mut out = Vec::new();
        for (key, w) in self.watching.iter_mut() {
            if w.escalated || now_ms < w.deadline_ms {
                continue;
            }
            w.escalated = true;
            out.push(Escalation {
                alert_id: w.owner.clone(),
                created_at: w.created_at.clone(),
                incident: key.label(),
            });
        }
        out
    }

    /// `alert_id` has closed. If it owned an incident, that incident is over
    /// and its environment is free for a new one.
    pub fn closed(&mut self, alert_id: &str) {
        // Only the owner's close ends the incident. A duplicate resolving on
        // its own says nothing about the outage the timer is about.
        self.watching.retain(|_, w| w.owner != alert_id);
    }

    /// How many incidents are being timed. For the log heartbeat.
    pub fn watching_count(&self) -> usize {
        self.watching.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: u64 = 10 * 60 * 1000;

    fn cfg() -> PingdomFeature {
        PingdomFeature {
            enabled: true,
            message_contains: "[Pingdom]".to_string(),
            ..PingdomFeature::default()
        }
    }

    fn alert(id: &str, title: &str) -> Alert {
        Alert {
            id: id.to_string(),
            status: "open".to_string(),
            message: title.to_string(),
            created_at: "2026-08-26T14:03:11Z".to_string(),
            ..Default::default()
        }
    }

    // -- the title parse -------------------------------------------------

    fn markers(m: &[&str]) -> Vec<String> {
        m.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_environment_is_everything_after_a_configured_marker() {
        let (env, how) =
            environment_from_title("[Pingdom] example.com MyApp PROD", &markers(&["MyApp"]));
        assert_eq!(env, Some("PROD".to_string()));
        assert_eq!(how, EnvSource::AfterMarker("MyApp".to_string()));
    }

    #[test]
    fn an_environment_of_two_words_survives_intact() {
        // The reason the marker exists. `prod one` is one environment, and
        // the last-word rule would call it `one` and file it with every
        // other environment ending in that word.
        let (env, _) =
            environment_from_title("[Pingdom] example.com MyApp prod one", &markers(&["MyApp"]));
        assert_eq!(env, Some("prod one".to_string()));
    }

    #[test]
    fn markers_are_tried_in_order_and_the_first_listed_wins() {
        // Both appear; the list decides, not the summary's word order.
        let (env, how) = environment_from_title(
            "[Pingdom] AppTwo AppOne prod two",
            &markers(&["AppOne", "AppTwo"]),
        );
        assert_eq!(env, Some("prod two".to_string()));
        assert_eq!(how, EnvSource::AfterMarker("AppOne".to_string()));
    }

    #[test]
    fn a_marker_matches_case_insensitively() {
        // Summaries are free text and app names drift in case, the same way
        // MMODAL_ENV does.
        let (env, _) =
            environment_from_title("[Pingdom] example.com MYAPP PROD", &markers(&["MyApp"]));
        assert_eq!(env, Some("PROD".to_string()));
    }

    #[test]
    fn no_marker_in_the_summary_falls_back_to_the_last_word() {
        let (env, how) =
            environment_from_title("[Pingdom] domain xxx yy PROD", &markers(&["SomeOtherApp"]));
        assert_eq!(env, Some("PROD".to_string()));
        assert_eq!(how, EnvSource::LastWord);
    }

    #[test]
    fn an_empty_marker_list_is_the_last_word_rule() {
        let (env, how) = environment_from_title("[Pingdom] domain xxx yy DEV1", &[]);
        assert_eq!(env, Some("DEV1".to_string()));
        assert_eq!(how, EnvSource::LastWord);
    }

    #[test]
    fn a_blank_marker_entry_matches_nothing() {
        // A blank entry left in the list would otherwise match at offset 0
        // and make the environment the entire summary.
        let (env, how) =
            environment_from_title("[Pingdom] domain xxx yy PROD", &markers(&["", "   "]));
        assert_eq!(env, Some("PROD".to_string()));
        assert_eq!(how, EnvSource::LastWord);
    }

    #[test]
    fn nothing_after_the_marker_is_not_an_environment() {
        let (env, how) =
            environment_from_title("[Pingdom] example.com MyApp", &markers(&["MyApp"]));
        assert_eq!(env, None);
        assert_eq!(how, EnvSource::Unreadable);
    }

    #[test]
    fn an_unrendered_template_is_not_an_environment() {
        // This feed serves both spellings in fields that should hold values.
        // Keying an incident on one would file unrelated outages together.
        for junk in [
            "[Pingdom] domain xxx yy {{environment}}",
            "[Pingdom] domain xxx yy &{%env%}%",
            "[Pingdom] domain xxx yy <environment>",
        ] {
            assert_eq!(environment_from_title(junk, &[]).0, None, "{junk}");
        }
        // And on the marker path, which reads a longer span and so has more
        // chance of catching one.
        assert_eq!(
            environment_from_title("[Pingdom] MyApp {{env}}", &markers(&["MyApp"])).0,
            None
        );
    }

    #[test]
    fn a_title_of_one_word_names_no_environment() {
        // Nothing to take the last word *of* — the whole title would become
        // the environment, which is not what a title like this means.
        assert_eq!(environment_from_title("[Pingdom]", &[]).0, None);
        assert_eq!(environment_from_title("", &[]).0, None);
        assert_eq!(environment_from_title("     ", &[]).0, None);
    }

    #[test]
    fn the_source_reads_back_as_a_sentence_for_the_log() {
        // The markers are tuned from this line, so it has to say which path
        // produced the value.
        assert_eq!(
            EnvSource::AfterMarker("MyApp".to_string()).describe(),
            "after marker 'MyApp'"
        );
        assert_eq!(EnvSource::LastWord.describe(), "last word of the summary");
        assert_eq!(
            EnvSource::Unreadable.describe(),
            "could not be read from the summary"
        );
    }

    #[test]
    fn a_multi_word_environment_keys_on_one_normalised_form() {
        // `prod one`, `PROD ONE` and `prod  one` are one environment. Free
        // text arrives spaced and cased however someone typed it.
        let m = markers(&["MyApp"]);
        assert_eq!(
            incident_key(&alert("a1", "[Pingdom] x MyApp prod one"), &m),
            incident_key(&alert("a2", "[Pingdom] y MyApp PROD  ONE"), &m)
        );
    }

    // -- identification --------------------------------------------------

    #[test]
    fn the_title_marker_identifies_a_pingdom_alert() {
        assert!(identifies(&alert("a1", "[Pingdom] domain xxx yy PROD"), &cfg()));
        assert!(!identifies(&alert("a1", "reaper stalled on i-0abc"), &cfg()));
    }

    #[test]
    fn a_blank_rule_matches_nothing() {
        // An unconfigured watcher must not acknowledge every alert on the
        // feed.
        let unset = PingdomFeature { enabled: true, ..PingdomFeature::default() };
        assert!(!identifies(&alert("a1", "[Pingdom] domain xxx yy PROD"), &unset));
        assert!(!identifies(&alert("a1", "anything at all"), &unset));
    }

    #[test]
    fn the_flattened_extra_properties_copy_is_never_matched() {
        // The feed carries a literal `{{extraProperties}}` key holding a
        // copy of the whole map. Matching it fires on alerts that merely
        // mention pingdom inside an unrendered template.
        let mut a = alert("a1", "something else entirely");
        a.extra.insert(
            "{{extraProperties}}".to_string(),
            "alertname=[Pingdom] domain xxx yy PROD".to_string(),
        );
        assert!(!identifies(&a, &cfg()));
    }

    // -- incident keying -------------------------------------------------

    #[test]
    fn environment_keying_ignores_case() {
        // `dev1` and `DEV1` are one environment. The repo already has the
        // scar from treating them as two.
        assert_eq!(
            incident_key(&alert("a1", "[Pingdom] x y dev1"), &[]),
            incident_key(&alert("a2", "[Pingdom] p q DEV1"), &[])
        );
    }

    #[test]
    fn an_alert_with_no_readable_environment_is_its_own_incident() {
        let a = alert("a1", "[Pingdom]");
        let b = alert("a2", "[Pingdom]");
        assert_eq!(incident_key(&a, &[]), IncidentKey::Alert("a1".to_string()));
        assert_ne!(incident_key(&a, &[]), incident_key(&b, &[]));
    }

    // -- the lifecycle ---------------------------------------------------

    #[test]
    fn the_first_alert_of_an_incident_is_acknowledged_and_timed() {
        let mut s = PingdomState::new();
        let a = alert("a1", "[Pingdom] domain xxx yy PROD");
        assert_eq!(s.consider(&a, &[], 0, WINDOW), Action::AckAndWatch);
        assert_eq!(s.watched_alert_ids(), vec!["a1".to_string()]);
    }

    #[test]
    fn an_alert_already_acted_on_is_never_acknowledged_twice() {
        // It stays open and therefore keeps coming back on the feed. Acking
        // it every 30s would be pointless traffic against a live page.
        let mut s = PingdomState::new();
        let a = alert("a1", "[Pingdom] domain xxx yy PROD");
        assert_eq!(s.consider(&a, &[], 0, WINDOW), Action::AckAndWatch);
        assert_eq!(s.consider(&a, &[], 30_000, WINDOW), Action::Ignore);
        assert_eq!(s.consider(&a, &[], 60_000, WINDOW), Action::Ignore);
    }

    #[test]
    fn a_second_alert_in_the_same_environment_is_acknowledged_and_nothing_else() {
        let mut s = PingdomState::new();
        s.consider(&alert("a1", "[Pingdom] domain xxx yy PROD"), &[], 0, WINDOW);
        assert_eq!(
            s.consider(&alert("a2", "[Pingdom] other host PROD"), &[], 60_000, WINDOW),
            Action::AckOnly { owner: "a1".to_string() }
        );
        // One incident, still owned by the first alert.
        assert_eq!(s.watched_alert_ids(), vec!["a1".to_string()]);
    }

    #[test]
    fn a_second_alert_does_not_push_the_first_ones_deadline_out() {
        // A trickle of duplicates must not extend the window indefinitely
        // and hold the escalation past the point of being useful.
        let mut s = PingdomState::new();
        s.consider(&alert("a1", "[Pingdom] domain xxx yy PROD"), &[], 0, WINDOW);
        s.consider(&alert("a2", "[Pingdom] other host PROD"), &[], 9 * 60_000, WINDOW);

        let due = s.due(WINDOW + 1);
        assert_eq!(due.len(), 1, "the first alert's own deadline still governs");
        assert_eq!(due[0].alert_id, "a1");
    }

    #[test]
    fn another_environment_runs_its_own_timer_concurrently() {
        // A PROD escalation must never suppress a DEV1 outage.
        let mut s = PingdomState::new();
        assert_eq!(
            s.consider(&alert("a1", "[Pingdom] domain xxx yy PROD"), &[], 0, WINDOW),
            Action::AckAndWatch
        );
        assert_eq!(
            s.consider(&alert("a2", "[Pingdom] domain xxx yy DEV1"), &[], 1_000, WINDOW),
            Action::AckAndWatch
        );
        assert_eq!(s.watching_count(), 2);

        let mut ids: Vec<String> = s.due(WINDOW + 2_000).into_iter().map(|e| e.alert_id).collect();
        ids.sort();
        assert_eq!(ids, vec!["a1".to_string(), "a2".to_string()]);
    }

    #[test]
    fn an_alert_still_open_at_the_deadline_escalates_exactly_once() {
        let mut s = PingdomState::new();
        s.consider(&alert("a1", "[Pingdom] domain xxx yy PROD"), &[], 0, WINDOW);

        assert!(s.due(WINDOW - 1).is_empty(), "not due yet");

        let due = s.due(WINDOW);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].alert_id, "a1");
        assert_eq!(due[0].created_at, "2026-08-26T14:03:11Z");

        assert!(s.due(WINDOW + 60_000).is_empty(), "and never a second time");
    }

    #[test]
    fn an_alert_that_closes_inside_the_window_never_escalates() {
        let mut s = PingdomState::new();
        s.consider(&alert("a1", "[Pingdom] domain xxx yy PROD"), &[], 0, WINDOW);
        s.closed("a1");
        assert!(s.due(WINDOW + 60_000).is_empty());
        assert_eq!(s.watching_count(), 0);
    }

    #[test]
    fn the_slot_is_held_while_an_escalated_alert_stays_open() {
        // The escalation has fired and nobody has closed the alert. A
        // further alert in that environment is acknowledged, not escalated:
        // the phone is already ringing about it.
        let mut s = PingdomState::new();
        s.consider(&alert("a1", "[Pingdom] domain xxx yy PROD"), &[], 0, WINDOW);
        assert_eq!(s.due(WINDOW).len(), 1);

        assert_eq!(
            s.consider(&alert("a2", "[Pingdom] other host PROD"), &[], WINDOW + 1_000, WINDOW),
            Action::AckOnly { owner: "a1".to_string() }
        );
        assert!(s.due(WINDOW * 3).is_empty());
    }

    #[test]
    fn closing_the_owner_frees_the_environment_for_a_new_incident() {
        let mut s = PingdomState::new();
        s.consider(&alert("a1", "[Pingdom] domain xxx yy PROD"), &[], 0, WINDOW);
        assert_eq!(s.due(WINDOW).len(), 1);
        s.closed("a1");

        // A fresh outage in the same environment gets a fresh timer.
        assert_eq!(
            s.consider(&alert("a2", "[Pingdom] domain xxx yy PROD"), &[], WINDOW + 1_000, WINDOW),
            Action::AckAndWatch
        );
        let due = s.due(WINDOW * 2 + 2_000);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].alert_id, "a2");
    }

    #[test]
    fn a_duplicate_closing_does_not_end_the_incident() {
        // Only the owner's close frees the slot. A duplicate resolving on
        // its own says nothing about the outage the timer is about.
        let mut s = PingdomState::new();
        s.consider(&alert("a1", "[Pingdom] domain xxx yy PROD"), &[], 0, WINDOW);
        s.consider(&alert("a2", "[Pingdom] other host PROD"), &[], 1_000, WINDOW);
        s.closed("a2");

        assert_eq!(s.watching_count(), 1);
        assert_eq!(s.due(WINDOW).len(), 1, "a1 still escalates");
    }
}

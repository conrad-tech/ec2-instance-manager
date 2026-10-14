//! User-defined alert watchers.
//!
//! An alert here is a name, some text to look for in an on-call alert's
//! summary, how long to wait before acknowledging it, and optionally how much
//! longer to wait before escalating if it is still open. Everything in this
//! module is pure -- alerts in, decisions out -- so the rules can be exercised
//! with no JSM, no network and no GUI. Acknowledging silences a live page and
//! escalating rings a phone.
//!
//! The shape of the job:
//!
//! 1. a matching alert is first seen and its timer starts,
//! 2. `ack_after_mins` later it is acknowledged,
//! 3. if `escalate` is on and the alert is still open `escalate_wait_mins`
//!    after that, it escalates.
//!
//! So acknowledge 2 and wait 5 escalates at minute 7.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::alerts::Alert;

/// The saved alerts, shared between the GUI (which edits them) and the poll
/// thread (which reads a snapshot every poll). Only the definitions are
/// shared; [`CustomAlertState`] is owned by the poll thread alone.
pub type SharedDefs = Arc<Mutex<Vec<CustomAlert>>>;

const MIN_MS: u64 = 60_000;

/// `true` when `hay` contains `needle`, case-insensitively. A blank needle
/// matches nothing -- a blank rule must not match every alert on the feed.
fn contains_ci(hay: &str, needle: &str) -> bool {
    let n = needle.trim();
    if n.is_empty() {
        return false;
    }
    hay.to_ascii_lowercase().contains(&n.to_ascii_lowercase())
}

/// One saved alert.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomAlert {
    pub name: String,
    /// Looked for, case-insensitively, in the alert's summary (`message`).
    pub match_text: String,
    /// Minutes after first sighting at which the alert is acknowledged.
    pub ack_after_mins: u32,
    pub escalate: bool,
    /// Minutes after the acknowledge before it escalates. Zero when
    /// `escalate` is off.
    pub escalate_wait_mins: u32,
}

impl CustomAlert {
    /// Minutes after first sighting at which this escalates, or `None` when
    /// it never does. Widened to `u64` so two `u32::MAX` values cannot wrap.
    pub fn escalate_at_mins(&self) -> Option<u64> {
        self.escalate
            .then(|| u64::from(self.ack_after_mins) + u64::from(self.escalate_wait_mins))
    }

    pub fn matches(&self, alert: &Alert) -> bool {
        contains_ci(&alert.message, &self.match_text)
    }
}

/// The first saved alert that matches, in list order. List order decides when
/// two match, so an alert is only ever timed once.
pub fn first_match<'a>(defs: &'a [CustomAlert], alert: &Alert) -> Option<&'a CustomAlert> {
    defs.iter().find(|d| d.matches(alert))
}

/// A whole number of minutes, at least 1. Surrounding spaces are fine.
/// Anything else -- empty, `0`, `-1`, `1.5`, `abc`, or a value that does not
/// fit -- is `None`.
pub fn parse_mins(raw: &str) -> Option<u32> {
    let t = raw.trim();
    if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    t.parse::<u32>().ok().filter(|n| *n >= 1)
}

/// Why a form cannot be saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    NothingSelected,
    BlankName,
    DuplicateName,
    BlankMatchText,
    BadAckMins,
    BadWaitMins,
}

impl Problem {
    pub fn message(self) -> &'static str {
        match self {
            Self::NothingSelected => "Pick an alert to update.",
            Self::BlankName => "Give the alert a name.",
            Self::DuplicateName => "An alert with that name already exists.",
            Self::BlankMatchText => {
                "Enter the text to look for. A blank would match every alert."
            }
            Self::BadAckMins => "Minutes to acknowledge must be a whole number, 1 or more.",
            Self::BadWaitMins => "Minutes to wait to escalate must be a whole number, 1 or more.",
        }
    }
}

/// Add `def`, or replace the entry named `replacing` (case-insensitive) in
/// place, keeping its position: list order decides which alert wins when two
/// match.
pub fn upsert(list: &mut Vec<CustomAlert>, def: CustomAlert, replacing: Option<&str>) {
    if let Some(old) = replacing {
        if let Some(slot) = list.iter_mut().find(|d| d.name.eq_ignore_ascii_case(old)) {
            *slot = def;
            return;
        }
    }
    list.push(def);
}

/// Remove the entry named `name` (case-insensitive). True when one existed.
pub fn remove(list: &mut Vec<CustomAlert>, name: &str) -> bool {
    let before = list.len();
    list.retain(|d| !d.name.eq_ignore_ascii_case(name));
    list.len() != before
}

// ---- the dialog's form ----------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FormMode {
    #[default]
    New,
    Current,
}

/// What the Add/Update alert dialog edits. Plain strings for the two minute
/// boxes so half-typed input is representable; [`AlertForm::check`] is the
/// only way to turn it into a [`CustomAlert`].
#[derive(Clone, Debug, Default)]
pub struct AlertForm {
    pub mode: FormMode,
    /// The saved alert being edited, in Current mode. Its name is what a
    /// rename replaces and what the duplicate check excuses.
    pub selected: Option<String>,
    pub name: String,
    pub match_text: String,
    pub ack_raw: String,
    pub escalate: bool,
    pub wait_raw: String,
    /// Delete has been clicked once and is waiting for its confirmation.
    pub confirm_delete: bool,
}

impl AlertForm {
    pub fn new_blank() -> Self {
        Self {
            ack_raw: "1".to_string(),
            wait_raw: "5".to_string(),
            ..Self::default()
        }
    }

    /// Switching to New empties the form; switching to Current drops the
    /// selection so nothing is edited until one is picked.
    pub fn set_mode(&mut self, mode: FormMode) {
        if self.mode == mode {
            return;
        }
        *self = Self { mode, ..Self::new_blank() };
    }

    /// Fill the form from a saved alert (Current mode's dropdown).
    pub fn load(&mut self, def: &CustomAlert) {
        self.selected = Some(def.name.clone());
        self.name = def.name.clone();
        self.match_text = def.match_text.clone();
        self.ack_raw = def.ack_after_mins.to_string();
        self.escalate = def.escalate;
        self.wait_raw = if def.escalate {
            def.escalate_wait_mins.to_string()
        } else {
            "5".to_string()
        };
        self.confirm_delete = false;
    }

    /// The alert this form describes, or the first thing wrong with it.
    /// `existing` is the saved list, for the duplicate-name check.
    pub fn check(&self, existing: &[CustomAlert]) -> Result<CustomAlert, Problem> {
        let editing = match self.mode {
            FormMode::New => None,
            FormMode::Current => Some(self.selected.as_deref().ok_or(Problem::NothingSelected)?),
        };
        let name = self.name.trim();
        if name.is_empty() {
            return Err(Problem::BlankName);
        }
        let taken = existing.iter().any(|d| {
            d.name.eq_ignore_ascii_case(name)
                && !editing.is_some_and(|e| e.eq_ignore_ascii_case(&d.name))
        });
        if taken {
            return Err(Problem::DuplicateName);
        }
        let match_text = self.match_text.trim();
        if match_text.is_empty() {
            return Err(Problem::BlankMatchText);
        }
        let ack_after_mins = parse_mins(&self.ack_raw).ok_or(Problem::BadAckMins)?;
        let escalate_wait_mins = if self.escalate {
            parse_mins(&self.wait_raw).ok_or(Problem::BadWaitMins)?
        } else {
            0
        };
        Ok(CustomAlert {
            name: name.to_string(),
            match_text: match_text.to_string(),
            ack_after_mins,
            escalate: self.escalate,
            escalate_wait_mins,
        })
    }

    /// The line under the minute boxes. Empty while a number is unusable.
    pub fn escalates_at_label(&self) -> String {
        let Some(ack) = parse_mins(&self.ack_raw) else {
            return String::new();
        };
        if !self.escalate {
            return format!("Acknowledges at {ack} min, never escalates");
        }
        match parse_mins(&self.wait_raw) {
            Some(wait) => format!(
                "Acknowledges at {ack} min, escalates at {} min",
                u64::from(ack) + u64::from(wait)
            ),
            None => String::new(),
        }
    }
}

// ---- the timer --------------------------------------------------------------

/// What the caller should do about one alert this poll.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Considered {
    /// Not ours, already handled, or not worth acting on.
    Ignore,
    /// A timer started under the saved alert called `name`.
    Watching { name: String },
}

/// One step that has come due.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Due {
    Acknowledge { alert_id: String, name: String },
    Escalate {
        alert_id: String,
        /// `createdAt` verbatim, the only thing besides a code that leaves
        /// the machine.
        created_at: String,
        name: String,
    },
}

/// What a re-read of a watched alert means for its timer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RereadVerdict {
    Closed,
    AckedElsewhere,
    KeepTiming,
}

/// Closed always wins. Acknowledged counts as a person taking it only when
/// our own acknowledge did not succeed; otherwise our acknowledge would
/// cancel our own escalation.
pub fn reread_verdict(status: &str, acknowledged: bool, acked_by_us: bool) -> RereadVerdict {
    if crate::reaper::alert_is_closed(status) {
        RereadVerdict::Closed
    } else if acknowledged && !acked_by_us {
        RereadVerdict::AckedElsewhere
    } else {
        RereadVerdict::KeepTiming
    }
}

#[derive(Clone, Debug)]
struct Watch {
    name: String,
    created_at: String,
    ack_at_ms: u64,
    escalate_at_ms: Option<u64>,
    acked: bool,
    /// Our acknowledge was attempted AND succeeded. Separate from `acked`,
    /// so a re-read cannot mistake our own acknowledge for a person's.
    ack_ok: bool,
    escalated: bool,
}

/// What this process is timing. Owned by the poll thread and never shared, so
/// there is no lock here -- the same property `PingdomState` keeps.
#[derive(Debug, Default)]
pub struct CustomAlertState {
    watching: HashMap<String, Watch>,
    /// Every alert id a timer was started for, so a still-open alert is not
    /// timed again on every poll.
    seen: HashSet<String>,
}

impl CustomAlertState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decide what to do about `alert` and record the decision.
    ///
    /// `claimed_elsewhere` is true when a built-in watcher (reaper, unhealthy
    /// host, instance age, pingdom) identifies the alert; it is never touched
    /// here. An alert already closed, or already acknowledged by a person, is
    /// left alone: they have it.
    pub fn consider(
        &mut self,
        alert: &Alert,
        defs: &[CustomAlert],
        claimed_elsewhere: bool,
        now_ms: u64,
    ) -> Considered {
        if self.seen.contains(&alert.id)
            || claimed_elsewhere
            || alert.acknowledged
            || crate::reaper::alert_is_closed(&alert.status)
        {
            return Considered::Ignore;
        }
        let Some(def) = first_match(defs, alert) else {
            return Considered::Ignore;
        };
        self.seen.insert(alert.id.clone());
        let ack_at_ms = now_ms.saturating_add(u64::from(def.ack_after_mins) * MIN_MS);
        let escalate_at_ms = def
            .escalate
            .then(|| ack_at_ms.saturating_add(u64::from(def.escalate_wait_mins) * MIN_MS));
        self.watching.insert(
            alert.id.clone(),
            Watch {
                name: def.name.clone(),
                created_at: alert.created_at.clone(),
                ack_at_ms,
                escalate_at_ms,
                acked: false,
                ack_ok: false,
                escalated: false,
            },
        );
        Considered::Watching { name: def.name.clone() }
    }

    /// The alert ids being timed, which the caller re-reads each poll.
    pub fn watched_alert_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.watching.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// The steps whose time has come, marked so each fires exactly once.
    /// Sorted by alert id, acknowledge before escalate, so a slow poll that
    /// finds both due does them in the order a person would.
    pub fn due(&mut self, now_ms: u64) -> Vec<Due> {
        let mut out = Vec::new();
        for (id, w) in self.watching.iter_mut() {
            if !w.acked && now_ms >= w.ack_at_ms {
                w.acked = true;
                out.push(Due::Acknowledge { alert_id: id.clone(), name: w.name.clone() });
            }
            if let Some(at) = w.escalate_at_ms {
                if !w.escalated && now_ms >= at {
                    w.escalated = true;
                    out.push(Due::Escalate {
                        alert_id: id.clone(),
                        created_at: w.created_at.clone(),
                        name: w.name.clone(),
                    });
                }
            }
        }
        out.sort_by(|a, b| {
            let key = |d: &Due| match d {
                Due::Acknowledge { alert_id, .. } => (alert_id.clone(), 0u8),
                Due::Escalate { alert_id, .. } => (alert_id.clone(), 1u8),
            };
            key(a).cmp(&key(b))
        });
        out
    }

    /// Our acknowledge of `alert_id` went through.
    pub fn ack_succeeded(&mut self, alert_id: &str) {
        if let Some(w) = self.watching.get_mut(alert_id) {
            w.ack_ok = true;
        }
    }

    /// Whether our own acknowledge of `alert_id` succeeded.
    pub fn acked_by_us(&self, alert_id: &str) -> bool {
        self.watching.get(alert_id).is_some_and(|w| w.ack_ok)
    }

    /// `alert_id` has closed: stop timing it.
    pub fn closed(&mut self, alert_id: &str) {
        self.watching.remove(alert_id);
    }

    /// Drop the watches whose saved alert no longer exists (deleted or
    /// renamed). Returns the alert ids dropped, for the log. Editing an alert
    /// does not call for this: its running watches keep the times they were
    /// started with.
    pub fn forget_removed(&mut self, defs: &[CustomAlert]) -> Vec<String> {
        let mut dropped: Vec<String> = self
            .watching
            .iter()
            .filter(|(_, w)| !defs.iter().any(|d| d.name.eq_ignore_ascii_case(&w.name)))
            .map(|(id, _)| id.clone())
            .collect();
        dropped.sort();
        for id in &dropped {
            self.watching.remove(id);
        }
        dropped
    }

    /// How many alerts are being timed. For the log heartbeat.
    pub fn watching_count(&self) -> usize {
        self.watching.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(name: &str, text: &str, ack: u32, esc: bool, wait: u32) -> CustomAlert {
        CustomAlert {
            name: name.to_string(),
            match_text: text.to_string(),
            ack_after_mins: ack,
            escalate: esc,
            escalate_wait_mins: wait,
        }
    }

    pub(super) fn alert(id: &str, title: &str) -> Alert {
        Alert {
            id: id.to_string(),
            status: "open".to_string(),
            message: title.to_string(),
            created_at: "2026-10-14T14:03:11Z".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn escalation_is_acknowledge_plus_wait() {
        assert_eq!(def("a", "x", 2, true, 5).escalate_at_mins(), Some(7));
        assert_eq!(def("a", "x", 2, false, 0).escalate_at_mins(), None);
    }

    #[test]
    fn the_largest_values_do_not_overflow() {
        let d = def("a", "x", u32::MAX, true, u32::MAX);
        assert_eq!(d.escalate_at_mins(), Some(u64::from(u32::MAX) * 2));
    }

    #[test]
    fn matching_is_case_insensitive_on_the_summary() {
        let d = def("disk", "disk space", 1, false, 0);
        assert!(d.matches(&alert("1", "[Prod] LOW DISK SPACE on host-1")));
        assert!(!d.matches(&alert("2", "[Prod] cpu high")));
    }

    #[test]
    fn a_blank_match_text_matches_nothing() {
        // Blank would otherwise match every alert on the feed.
        assert!(!def("a", "", 1, false, 0).matches(&alert("1", "anything")));
        assert!(!def("a", "   ", 1, false, 0).matches(&alert("1", "anything")));
    }

    #[test]
    fn the_first_listed_alert_wins_when_two_match() {
        let defs = vec![def("first", "disk", 1, false, 0), def("second", "disk", 5, false, 0)];
        assert_eq!(first_match(&defs, &alert("1", "disk full")).unwrap().name, "first");
    }

    #[test]
    fn minutes_are_whole_numbers_of_at_least_one() {
        assert_eq!(parse_mins("1"), Some(1));
        assert_eq!(parse_mins(" 7 "), Some(7));
        for bad in ["", "   ", "0", "-1", "+3", "1.5", "abc", "1 2", "4294967296"] {
            assert_eq!(parse_mins(bad), None, "{bad:?} must be refused");
        }
        assert_eq!(parse_mins("4294967295"), Some(u32::MAX));
    }

    #[test]
    fn upsert_replaces_in_place_and_keeps_order() {
        let mut list = vec![def("a", "x", 1, false, 0), def("b", "y", 1, false, 0)];
        upsert(&mut list, def("A2", "z", 3, false, 0), Some("a"));
        assert_eq!(list.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["A2", "b"]);
        upsert(&mut list, def("c", "w", 1, false, 0), None);
        assert_eq!(list.len(), 3);
    }

    #[test]
    fn remove_is_case_insensitive_and_says_whether_it_removed() {
        let mut list = vec![def("Disk", "x", 1, false, 0)];
        assert!(remove(&mut list, "disk"));
        assert!(list.is_empty());
        assert!(!remove(&mut list, "disk"));
    }

    // ---- the form ---------------------------------------------------------

    fn filled(name: &str) -> AlertForm {
        AlertForm {
            name: name.to_string(),
            match_text: "disk".to_string(),
            ack_raw: "2".to_string(),
            escalate: true,
            wait_raw: "5".to_string(),
            ..AlertForm::new_blank()
        }
    }

    #[test]
    fn a_good_form_builds_the_alert() {
        let built = filled("  Disk  ").check(&[]).unwrap();
        assert_eq!(built, def("Disk", "disk", 2, true, 5));
    }

    #[test]
    fn the_wait_is_ignored_and_stored_as_zero_when_escalate_is_off() {
        let mut f = filled("x");
        f.escalate = false;
        f.wait_raw = "garbage".to_string();
        let built = f.check(&[]).unwrap();
        assert!(!built.escalate);
        assert_eq!(built.escalate_wait_mins, 0);
    }

    #[test]
    fn each_problem_is_reported_in_a_fixed_order() {
        let existing = vec![def("Disk", "x", 1, false, 0)];
        let mut f = filled("");
        assert_eq!(f.check(&existing), Err(Problem::BlankName));
        f.name = "disk".to_string();
        assert_eq!(f.check(&existing), Err(Problem::DuplicateName), "case-insensitive");
        f.name = "Other".to_string();
        f.match_text = "   ".to_string();
        assert_eq!(f.check(&existing), Err(Problem::BlankMatchText));
        f.match_text = "disk".to_string();
        f.ack_raw = "0".to_string();
        assert_eq!(f.check(&existing), Err(Problem::BadAckMins));
        f.ack_raw = "2".to_string();
        f.wait_raw = "x".to_string();
        assert_eq!(f.check(&existing), Err(Problem::BadWaitMins));
    }

    #[test]
    fn updating_an_alert_may_keep_its_own_name_but_not_take_anothers() {
        let existing = vec![def("Disk", "x", 1, false, 0), def("Cpu", "y", 1, false, 0)];
        let mut f = AlertForm::new_blank();
        f.set_mode(FormMode::Current);
        assert_eq!(f.check(&existing), Err(Problem::NothingSelected));
        f.load(&existing[0]);
        assert!(f.check(&existing).is_ok(), "saving unchanged is fine");
        f.name = "cpu".to_string();
        assert_eq!(f.check(&existing), Err(Problem::DuplicateName));
    }

    #[test]
    fn the_label_says_when_it_escalates() {
        let mut f = filled("x");
        assert_eq!(f.escalates_at_label(), "Acknowledges at 2 min, escalates at 7 min");
        f.escalate = false;
        assert_eq!(f.escalates_at_label(), "Acknowledges at 2 min, never escalates");
        f.ack_raw = "oops".to_string();
        assert_eq!(f.escalates_at_label(), "");
    }

    #[test]
    fn switching_to_new_clears_the_selection_and_the_delete_arming() {
        let existing = def("Disk", "x", 3, true, 4);
        let mut f = AlertForm::new_blank();
        f.set_mode(FormMode::Current);
        f.load(&existing);
        f.confirm_delete = true;
        f.set_mode(FormMode::New);
        assert_eq!(f.selected, None);
        assert!(f.name.is_empty());
        assert!(!f.confirm_delete);
    }

    // ---- the timer --------------------------------------------------------

    const T0: u64 = 1_000_000;

    fn watched(defs: &[CustomAlert]) -> (CustomAlertState, Alert) {
        let mut st = CustomAlertState::new();
        let a = alert("A1", "disk space low");
        assert_eq!(
            st.consider(&a, defs, false, T0),
            Considered::Watching { name: defs[0].name.clone() }
        );
        (st, a)
    }

    #[test]
    fn acknowledges_at_two_minutes_and_escalates_at_seven() {
        let defs = vec![def("disk", "disk", 2, true, 5)];
        let (mut st, _) = watched(&defs);
        assert!(st.due(T0 + MIN_MS).is_empty(), "nothing at minute 1");
        assert_eq!(
            st.due(T0 + 2 * MIN_MS),
            vec![Due::Acknowledge { alert_id: "A1".into(), name: "disk".into() }]
        );
        assert!(st.due(T0 + 6 * MIN_MS).is_empty(), "nothing at minute 6");
        assert_eq!(
            st.due(T0 + 7 * MIN_MS),
            vec![Due::Escalate {
                alert_id: "A1".into(),
                created_at: "2026-10-14T14:03:11Z".into(),
                name: "disk".into()
            }]
        );
    }

    #[test]
    fn each_step_fires_exactly_once() {
        let defs = vec![def("disk", "disk", 1, true, 1)];
        let (mut st, _) = watched(&defs);
        assert_eq!(st.due(T0 + 10 * MIN_MS).len(), 2);
        assert!(st.due(T0 + 20 * MIN_MS).is_empty());
    }

    #[test]
    fn a_slow_poll_fires_acknowledge_then_escalate_together() {
        let defs = vec![def("disk", "disk", 1, true, 1)];
        let (mut st, _) = watched(&defs);
        let due = st.due(T0 + 30 * MIN_MS);
        assert!(matches!(due[0], Due::Acknowledge { .. }));
        assert!(matches!(due[1], Due::Escalate { .. }));
    }

    #[test]
    fn without_escalate_it_only_acknowledges() {
        let defs = vec![def("disk", "disk", 2, false, 0)];
        let (mut st, _) = watched(&defs);
        assert_eq!(st.due(T0 + 60 * MIN_MS).len(), 1);
    }

    #[test]
    fn a_closed_alert_is_never_escalated() {
        let defs = vec![def("disk", "disk", 2, true, 5)];
        let (mut st, _) = watched(&defs);
        st.closed("A1");
        assert!(st.due(T0 + 60 * MIN_MS).is_empty());
        assert_eq!(st.watching_count(), 0);
    }

    #[test]
    fn a_sighting_is_only_timed_once() {
        let defs = vec![def("disk", "disk", 2, true, 5)];
        let (mut st, a) = watched(&defs);
        assert_eq!(st.consider(&a, &defs, false, T0 + MIN_MS), Considered::Ignore);
        assert_eq!(st.watching_count(), 1);
    }

    #[test]
    fn a_human_acknowledged_alert_is_left_alone() {
        let defs = vec![def("disk", "disk", 1, true, 1)];
        let mut st = CustomAlertState::new();
        let mut a = alert("A1", "disk space low");
        a.acknowledged = true;
        assert_eq!(st.consider(&a, &defs, false, T0), Considered::Ignore);
        assert_eq!(st.watching_count(), 0);
    }

    #[test]
    fn a_closed_alert_is_never_timed() {
        let defs = vec![def("disk", "disk", 1, true, 1)];
        let mut st = CustomAlertState::new();
        let mut a = alert("A1", "disk space low");
        a.status = "closed".to_string();
        assert_eq!(st.consider(&a, &defs, false, T0), Considered::Ignore);
    }

    #[test]
    fn an_alert_a_built_in_watcher_claims_is_never_touched() {
        let defs = vec![def("disk", "disk", 1, true, 1)];
        let mut st = CustomAlertState::new();
        let a = alert("A1", "disk space low");
        assert_eq!(st.consider(&a, &defs, true, T0), Considered::Ignore);
        assert_eq!(st.watching_count(), 0);
    }

    #[test]
    fn an_alert_matching_nothing_is_ignored() {
        let defs = vec![def("disk", "disk", 1, true, 1)];
        let mut st = CustomAlertState::new();
        assert_eq!(
            st.consider(&alert("A1", "cpu high"), &defs, false, T0),
            Considered::Ignore
        );
    }

    #[test]
    fn deleting_or_renaming_an_alert_drops_its_running_watches() {
        let defs = vec![def("disk", "disk", 1, true, 1), def("cpu", "cpu", 1, true, 1)];
        let mut st = CustomAlertState::new();
        st.consider(&alert("A1", "disk low"), &defs, false, T0);
        st.consider(&alert("A2", "cpu high"), &defs, false, T0);
        let remaining = vec![def("DISK", "disk", 1, true, 1)];
        let dropped = st.forget_removed(&remaining);
        assert_eq!(dropped, vec!["A2".to_string()]);
        assert_eq!(st.watched_alert_ids(), vec!["A1".to_string()]);
        assert!(st
            .due(T0 + 60 * MIN_MS)
            .iter()
            .all(|d| !matches!(d, Due::Escalate { alert_id, .. } if alert_id == "A2")));
    }

    #[test]
    fn editing_an_alert_does_not_retime_a_running_watch() {
        let defs = vec![def("disk", "disk", 2, true, 5)];
        let (mut st, _) = watched(&defs);
        let edited = vec![def("disk", "disk", 60, true, 60)];
        assert!(st.forget_removed(&edited).is_empty());
        assert_eq!(st.due(T0 + 2 * MIN_MS).len(), 1, "still acknowledges at the old minute 2");
    }

    #[test]
    fn the_largest_minutes_do_not_overflow_the_timer() {
        let defs = vec![def("disk", "disk", u32::MAX, true, u32::MAX)];
        let mut st = CustomAlertState::new();
        st.consider(&alert("A1", "disk"), &defs, false, u64::MAX - 10);
        assert!(st.due(u64::MAX - 5).is_empty(), "saturates rather than wrapping to now");
    }

    #[test]
    fn the_reread_verdict_table() {
        use RereadVerdict::*;
        assert_eq!(reread_verdict("closed", true, true), Closed, "closed always wins");
        assert_eq!(reread_verdict("closed", false, false), Closed);
        assert_eq!(reread_verdict("open", true, false), AckedElsewhere);
        assert_eq!(reread_verdict("acked", true, true), KeepTiming);
        assert_eq!(reread_verdict("open", false, false), KeepTiming);
        assert_eq!(reread_verdict("open", false, true), KeepTiming);
    }

    #[test]
    fn our_own_acknowledge_does_not_cancel_our_escalation() {
        let defs = vec![def("disk", "disk", 1, true, 1)];
        let (mut st, _) = watched(&defs);
        assert_eq!(
            st.due(T0 + MIN_MS),
            vec![Due::Acknowledge { alert_id: "A1".into(), name: "disk".into() }]
        );
        st.ack_succeeded("A1");
        assert_eq!(
            reread_verdict("acked", true, st.acked_by_us("A1")),
            RereadVerdict::KeepTiming
        );
        assert_eq!(st.due(T0 + 2 * MIN_MS).len(), 1, "the escalation still fires");
    }

    #[test]
    fn a_failed_acknowledge_does_not_hide_a_person_taking_the_alert() {
        let defs = vec![def("disk", "disk", 1, true, 1)];
        let (mut st, _) = watched(&defs);
        assert_eq!(st.due(T0 + MIN_MS).len(), 1);
        // No ack_succeeded: our acknowledge failed.
        assert!(!st.acked_by_us("A1"));
        assert_eq!(
            reread_verdict("open", true, st.acked_by_us("A1")),
            RereadVerdict::AckedElsewhere
        );
    }
}

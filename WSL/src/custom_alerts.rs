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

use std::sync::{Arc, Mutex};

use crate::alerts::Alert;

/// The saved alerts, shared between the GUI (which edits them) and the poll
/// thread (which reads a snapshot every poll). Only the definitions are
/// shared; [`CustomAlertState`] is owned by the poll thread alone.
pub type SharedDefs = Arc<Mutex<Vec<CustomAlert>>>;

// Used by the timer that Task 3 adds; allowed until then to keep the build warning-free.
#[allow(dead_code)]
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
}

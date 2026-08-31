//! Start / Stop / Restart for a single EC2 instance, from the Inventory tab.
//!
//! Everything here is pure: state strings and durations in, decisions and
//! sentences out. The AWS calls and the thread that sequences them live in
//! the GUI binary; this module exists so the part that can be *wrong* — which
//! action a given state permits, and how long the restart waits — is settled
//! by tests rather than by reading a worker loop.

use std::time::Duration;

/// How long to hold after the instance reports `stopped`, before the start
/// goes out.
///
/// This is the "wait 20s so you can see it is fully shut down" half of the
/// restart, and it is deliberately *additional* to the poll rather than a
/// substitute for it: a real stop takes 30-90s to reach `stopped`, so a bare
/// 20s sleep would call `start-instances` while the instance was still
/// `stopping` and be refused with `IncorrectInstanceState` — leaving the box
/// stopped and the restart half-done.
pub const SETTLE_SECS: u64 = 20;

/// Gap between `describe-instances` polls while waiting for `stopped`.
pub const POLL_INTERVAL_SECS: u64 = 5;

/// How long to wait for `stopped` before giving up. A stop that has not
/// landed in five minutes is not going to; the run reports that the instance
/// was left stopped and was **not** started, which is the state a human then
/// has to finish by hand.
pub const STOP_TIMEOUT_SECS: u64 = 300;

/// Which lifecycle action was asked for.
///
/// `Restart` is a stop followed by a start — **not** `ec2 reboot-instances`,
/// which keeps the same underlying host and never reports `stopped`. The
/// distinction is the whole reason this variant exists, so every string it
/// produces says so.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerAction {
    Start,
    Stop,
    Restart,
}

impl PowerAction {
    /// The context-menu label.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Start => "Start instance",
            Self::Stop => "Stop instance",
            // `->`, never `→`: egui's default font draws nothing for
            // Unicode's Arrows block.
            Self::Restart => "Restart (stop -> start)",
        }
    }

    /// The action's name as it appears mid-sentence in the confirmation.
    pub fn verb(&self) -> &'static str {
        match self {
            Self::Start => "Start",
            Self::Stop => "Stop",
            Self::Restart => "Restart",
        }
    }
}

/// Normalised state string: the column is free text from the API, and the
/// same value arrives padded or cased differently depending on the path.
fn norm(state: &str) -> String {
    state.trim().to_ascii_lowercase()
}

/// True once the instance has actually reached `stopped`.
///
/// `stopping` is the state being waited *out* of and must never satisfy
/// this — reading it as done is precisely the early `start-instances` the
/// poll exists to prevent.
pub fn is_stopped(state: &str) -> bool {
    norm(state) == "stopped"
}

/// True when waiting any longer for `stopped` is pointless: the instance is
/// on its way to `terminated`, which is a state the poll would otherwise sit
/// through for the full timeout before saying anything.
pub fn poll_is_hopeless(state: &str) -> bool {
    matches!(norm(state).as_str(), "terminated" | "terminating" | "shutting-down")
}

/// Whether `action` can be asked of an instance currently in `state`, and
/// why not when it cannot.
///
/// Used twice: to disable the menu entry with the reason as hover text, and
/// again inside the worker before the call goes out — the inventory row can
/// be up to 45 seconds stale, so the menu's answer is a courtesy and the
/// worker's is the one that counts.
pub fn action_allowed(state: &str, action: PowerAction) -> std::result::Result<(), String> {
    match norm(state).as_str() {
        "running" => match action {
            PowerAction::Start => Err("already running".to_string()),
            PowerAction::Stop | PowerAction::Restart => Ok(()),
        },
        "stopped" => match action {
            PowerAction::Start => Ok(()),
            PowerAction::Stop => Err("already stopped".to_string()),
            // Not silently downgraded to a Start: a restart is a stop and a
            // start, and turning a click on one into the other is a
            // different action from the one that was agreed to.
            PowerAction::Restart => {
                Err("already stopped — use Start to bring it up".to_string())
            }
        },
        other @ ("pending" | "stopping" | "shutting-down") => Err(format!(
            "instance is {other} — wait for it to settle"
        )),
        other @ ("terminated" | "terminating") => {
            Err(format!("instance is {other}"))
        }
        other => Err(format!("unrecognised state '{other}'")),
    }
}

/// What is left of the settle hold, given how long it is since the instance
/// reported `stopped`. Saturates at zero rather than wrapping.
pub fn settle_remaining(since_stopped: Duration) -> Duration {
    Duration::from_secs(SETTLE_SECS).saturating_sub(since_stopped)
}

/// True once the stop has been waited on longer than [`STOP_TIMEOUT_SECS`].
pub fn stop_timed_out(waited: Duration) -> bool {
    waited >= Duration::from_secs(STOP_TIMEOUT_SECS)
}

/// Where a run has got to, as reported to the status line above the
/// inventory table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PowerPhase {
    /// The stop call has gone out and `stopped` is being polled for.
    Stopping { waited_secs: u64 },
    /// It reported `stopped`; holding so the shutdown is visible.
    Settling { secs_left: u64 },
    /// The start call has gone out.
    Starting,
}

impl PowerPhase {
    /// The phase alone. The instance id is rendered once by the caller,
    /// beside this, so it is not repeated on every update.
    pub fn describe(&self) -> String {
        match self {
            Self::Stopping { waited_secs } => format!("stopping… {waited_secs}s"),
            Self::Settling { secs_left } => format!("stopped — starting in {secs_left}s"),
            Self::Starting => "starting…".to_string(),
        }
    }
}

/// The body of the confirmation dialog.
///
/// `label` is the instance as the user sees it in the table — `i-0abc
/// (web01)` — so the dialog names the box, not just the action.
pub fn confirm_text(action: PowerAction, label: &str) -> String {
    match action {
        PowerAction::Start => {
            format!("Start {label}?\n\nRuns: aws ec2 start-instances")
        }
        PowerAction::Stop => format!(
            "Stop {label}?\n\nRuns: aws ec2 stop-instances\n\
             Anything running on the instance is shut down."
        ),
        PowerAction::Restart => format!(
            "Restart {label}?\n\n\
             This is a stop and a start, not an EC2 reboot:\n\
             \u{2022} aws ec2 stop-instances\n\
             \u{2022} poll until the instance reports 'stopped'\n\
             \u{2022} hold {SETTLE_SECS}s\n\
             \u{2022} aws ec2 start-instances\n\n\
             The instance keeps its id and private IP, but anything on \
             instance store is lost and a public IP would change."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // ---- action_allowed: the state matrix -------------------------------

    #[test]
    fn a_running_instance_may_be_stopped_or_restarted_but_not_started() {
        assert!(action_allowed("running", PowerAction::Stop).is_ok());
        assert!(action_allowed("running", PowerAction::Restart).is_ok());
        let err = action_allowed("running", PowerAction::Start).unwrap_err();
        assert!(err.contains("already running"), "{err}");
    }

    #[test]
    fn a_stopped_instance_may_only_be_started() {
        assert!(action_allowed("stopped", PowerAction::Start).is_ok());
        let stop = action_allowed("stopped", PowerAction::Stop).unwrap_err();
        assert!(stop.contains("already stopped"), "{stop}");
        // Restart means stop-then-start. On a box that is already down there
        // is nothing to stop, and quietly turning it into a Start would be a
        // different action from the one that was clicked.
        let restart = action_allowed("stopped", PowerAction::Restart).unwrap_err();
        assert!(restart.contains("already stopped"), "{restart}");
        assert!(restart.contains("Start"), "should point at the action that does work: {restart}");
    }

    #[test]
    fn an_instance_mid_transition_is_refused_and_the_state_is_named() {
        for state in ["pending", "stopping", "shutting-down"] {
            for action in [PowerAction::Start, PowerAction::Stop, PowerAction::Restart] {
                let err = action_allowed(state, action).unwrap_err();
                assert!(err.contains(state), "{state}/{action:?}: {err}");
            }
        }
    }

    #[test]
    fn a_terminated_instance_is_refused_for_every_action() {
        for state in ["terminated", "shutting-down"] {
            for action in [PowerAction::Start, PowerAction::Stop, PowerAction::Restart] {
                assert!(action_allowed(state, action).is_err(), "{state}/{action:?}");
            }
        }
    }

    /// An unrecognised state is refused rather than acted on. The column is
    /// fed by `describe-instances`, so anything outside the six documented
    /// values means we are not reading what we think we are — and firing a
    /// `stop-instances` on that guess is the expensive direction to be wrong.
    #[test]
    fn an_unknown_state_is_refused_and_quoted_back() {
        let err = action_allowed("banana", PowerAction::Stop).unwrap_err();
        assert!(err.contains("banana"), "{err}");
    }

    #[test]
    fn state_matching_ignores_case_and_surrounding_space() {
        assert!(action_allowed("  Running  ", PowerAction::Stop).is_ok());
        assert!(action_allowed("STOPPED", PowerAction::Start).is_ok());
    }

    // ---- the restart wait ------------------------------------------------

    /// The settle hold is what "wait 20s to show it's fully shutdown" buys:
    /// the instance has *already* reported `stopped`, and this is the pause
    /// that makes that visible before the start goes out.
    #[test]
    fn the_settle_hold_counts_down_from_twenty_seconds_and_stops_at_zero() {
        assert_eq!(settle_remaining(Duration::from_secs(0)), Duration::from_secs(SETTLE_SECS));
        assert_eq!(settle_remaining(Duration::from_secs(5)), Duration::from_secs(15));
        assert_eq!(settle_remaining(Duration::from_secs(SETTLE_SECS)), Duration::ZERO);
        assert_eq!(settle_remaining(Duration::from_secs(600)), Duration::ZERO);
    }

    #[test]
    fn the_stop_poll_gives_up_after_the_timeout() {
        assert!(!stop_timed_out(Duration::from_secs(0)));
        assert!(!stop_timed_out(Duration::from_secs(STOP_TIMEOUT_SECS - 1)));
        assert!(stop_timed_out(Duration::from_secs(STOP_TIMEOUT_SECS)));
    }

    #[test]
    fn only_the_stopped_state_ends_the_poll() {
        assert!(is_stopped("stopped"));
        assert!(is_stopped(" Stopped "));
        // `stopping` is the state being waited *out* of. Reading it as done
        // is exactly the mistake that would fire `start-instances` early.
        assert!(!is_stopped("stopping"));
        assert!(!is_stopped("running"));
        assert!(!is_stopped("shutting-down"));
    }

    /// A stop that ends in `terminated` never reaches `stopped`, so the poll
    /// would otherwise spin for the full five minutes before saying anything.
    #[test]
    fn a_terminated_instance_abandons_the_poll_early() {
        assert!(poll_is_hopeless("terminated"));
        assert!(poll_is_hopeless("shutting-down"));
        assert!(!poll_is_hopeless("stopping"));
        assert!(!poll_is_hopeless("stopped"));
    }

    // ---- what the status line says ---------------------------------------

    #[test]
    fn each_phase_describes_itself_without_naming_the_instance() {
        // The instance id is rendered by the caller, once, beside the phase.
        assert_eq!(PowerPhase::Stopping { waited_secs: 25 }.describe(), "stopping… 25s");
        assert_eq!(
            PowerPhase::Settling { secs_left: 12 }.describe(),
            "stopped — starting in 12s",
        );
        assert_eq!(PowerPhase::Starting.describe(), "starting…");
    }

    // ---- the confirmation dialog -----------------------------------------

    /// The restart wording is the whole point of the feature: it is a stop
    /// and a start, not an EC2 reboot, and the dialog has to say so before
    /// anyone agrees to it.
    #[test]
    fn the_restart_confirmation_spells_out_stop_wait_start() {
        let text = confirm_text(PowerAction::Restart, "i-0abc (web01)");
        assert!(text.contains("i-0abc (web01)"), "{text}");
        assert!(text.contains("stop"), "{text}");
        assert!(text.contains("stopped"), "{text}");
        assert!(text.contains("20"), "{text}");
        assert!(text.contains("start"), "{text}");
        // It must not be mistaken for an EC2 reboot. Naming the contrast is
        // a stronger guarantee than avoiding the word, since "reboot" is
        // what a reader arrives with in mind.
        assert!(text.contains("not an EC2 reboot"), "{text}");
    }

    #[test]
    fn the_stop_and_start_confirmations_name_the_instance_and_the_action() {
        let stop = confirm_text(PowerAction::Stop, "i-0abc (web01)");
        assert!(stop.contains("i-0abc (web01)") && stop.contains("Stop"), "{stop}");
        let start = confirm_text(PowerAction::Start, "i-0abc (web01)");
        assert!(start.contains("i-0abc (web01)") && start.contains("Start"), "{start}");
    }

    #[test]
    fn every_action_has_a_menu_label_and_restart_says_it_is_a_stop_start() {
        assert_eq!(PowerAction::Start.label(), "Start instance");
        assert_eq!(PowerAction::Stop.label(), "Stop instance");
        assert_eq!(PowerAction::Restart.label(), "Restart (stop -> start)");
    }

    /// egui's default font carries no glyphs from Unicode's Arrows block, so
    /// a `->` written as `→` renders as an empty box. The GUI file has a
    /// scanner for this; these strings live here, so they are checked here.
    #[test]
    fn no_label_uses_a_glyph_the_default_font_cannot_draw() {
        let mut strings: Vec<String> = Vec::new();
        for action in [PowerAction::Start, PowerAction::Stop, PowerAction::Restart] {
            strings.push(action.label().to_string());
            strings.push(confirm_text(action, "i-0abc"));
        }
        strings.push(PowerPhase::Stopping { waited_secs: 1 }.describe());
        strings.push(PowerPhase::Settling { secs_left: 1 }.describe());
        strings.push(PowerPhase::Starting.describe());
        for s in strings {
            for ch in s.chars() {
                assert!(
                    !('\u{2190}'..='\u{21FF}').contains(&ch),
                    "{s:?} contains arrow glyph {ch:?}",
                );
            }
        }
    }
}

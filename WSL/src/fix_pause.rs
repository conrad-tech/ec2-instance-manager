//! Temporary pause switches for the unattended on-call watchers.
//!
//! Edit > Manage Fixes lists every watcher that is actually running and lets
//! a person stop one for a while, to resolve an incident by hand without the
//! app acknowledging, terminating or escalating underneath them.
//!
//! * **Session only, never persisted.** A pause that survived a restart would
//!   leave pages unattended with nothing on screen saying so; every launch
//!   starts with everything running.
//! * **A paused watcher skips its whole poll** -- no new alerts, no timers
//!   advanced, no escalations. Deadlines are absolute, so on resume an
//!   incident already past its deadline acts on the first poll.
//! * Manual Run Remediation / Test Alert Match are not affected.

use std::collections::BTreeSet;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Fix {
    Reaper,
    Pingdom,
    UnhealthyHost,
    InstanceAge,
    Grafana,
    CustomAlerts,
}

impl Fix {
    pub fn label(self) -> &'static str {
        match self {
            Fix::Reaper => "Reaper Down",
            Fix::Pingdom => "Pingdom",
            Fix::UnhealthyHost => "Unhealthy Host",
            Fix::InstanceAge => "Instance Age",
            Fix::Grafana => "Grafana Missing-* Info",
            Fix::CustomAlerts => "Custom Alerts",
        }
    }
}

/// How long a paused watcher sleeps between checks of its switch.
pub const PAUSED_RECHECK: Duration = Duration::from_secs(5);

#[derive(Default)]
struct State {
    running: BTreeSet<Fix>,
    paused: BTreeSet<Fix>,
}

fn state() -> &'static Mutex<State> {
    static S: OnceLock<Mutex<State>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(State::default()))
}

fn lock() -> std::sync::MutexGuard<'static, State> {
    state().lock().unwrap_or_else(|p| p.into_inner())
}

/// Called when a watcher's thread starts, so the menu lists only fixes that
/// exist in this session.
pub fn register(fix: Fix) {
    lock().running.insert(fix);
}

/// Watchers that were started, in a stable order.
pub fn registered() -> Vec<Fix> {
    lock().running.iter().copied().collect()
}

pub fn is_paused(fix: Fix) -> bool {
    lock().paused.contains(&fix)
}

pub fn set_paused(fix: Fix, paused: bool) {
    let mut s = lock();
    if paused {
        s.paused.insert(fix);
    } else {
        s.paused.remove(&fix);
    }
}

/// Top-of-loop guard (also registers the watcher): when paused, sleeps `PAUSED_RECHECK` and returns true,
/// and the caller `continue`s.
pub fn hold(fix: Fix) -> bool {
    // Registering here means a watcher is listed exactly when its loop is
    // really running, with no separate call to forget.
    register(fix);
    if is_paused(fix) {
        std::thread::sleep(PAUSED_RECHECK);
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // One test: the state is process-wide and would race under the
    // parallel runner if split.
    #[test]
    fn fixes_run_by_default_and_pause_independently() {
        assert!(!is_paused(Fix::Reaper));
        register(Fix::Reaper);
        register(Fix::Pingdom);
        register(Fix::Reaper);
        assert_eq!(registered(), vec![Fix::Reaper, Fix::Pingdom]);
        set_paused(Fix::Reaper, true);
        assert!(is_paused(Fix::Reaper));
        assert!(!is_paused(Fix::Pingdom));
        set_paused(Fix::Reaper, false);
        assert!(!is_paused(Fix::Reaper));
        assert!(!hold(Fix::Reaper));
    }
}

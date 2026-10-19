//! Deciding what to do about a **Grafana** `Missing-*-Info` alert.
//!
//! Pure — alerts, auto scaling group members and a clock in, decisions out —
//! so every rule is pinned by a test rather than by reading a poll loop. The
//! GUI owns every network hop: the on-call lookup, the JSM reads, the AWS
//! describes, the `terminate-instances` and the escalation email.
//!
//! The job, on call only:
//!
//! 1. acknowledge the alert;
//! 2. terminate the instance it names (the Grafana group is always 1/1/1, so
//!    there is no health gate to apply);
//! 3. wait for the group's replacement to be `InService`, `Healthy` **and**
//!    `running` in EC2;
//! 4. give the alert `settle_mins` from that moment to clear, and escalate
//!    once if it has not.
//!
//! Five alerts usually fire for one outage. They are keyed on the ASG: the
//! first owns the incident and every later one is acknowledged and otherwise
//! ignored, including those the replacement itself raises while it boots.

use crate::alerts::Alert;
use crate::asg::AsgInstance;
use crate::features::{GrafanaFeature, ReaperFeature, UnhealthyHostFeature};
use crate::instance_age::AlertFacts;
use crate::{reaper, unhealthy_host};
use std::collections::{BTreeMap, HashSet};

/// `true` when `hay` contains `needle`, case-insensitively. **A blank needle
/// matches nothing** — the same rule, copied rather than shared, as
/// `instance_age::contains_ci` and `reaper::contains_ci`.
fn contains_ci(hay: &str, needle: &str) -> bool {
    let n = needle.trim();
    if n.is_empty() {
        return false;
    }
    hay.to_ascii_lowercase().contains(&n.to_ascii_lowercase())
}

/// Is this a Grafana alert we are configured to handle? Reads only the alert
/// **title**, which comes with the list payload, so an alert on somebody
/// else's feed is declined without a per-alert read.
pub fn identifies(alert: &Alert, cfg: &GrafanaFeature) -> bool {
    cfg.title_contains
        .iter()
        .any(|t| contains_ci(&alert.message, t))
}

/// Is this alert ours to act on? **Reaper wins, and so does the unhealthy-host
/// watcher**: they already have a fix of their own for the alarms they claim,
/// and one alarm being handled twice is the failure this exists to prevent —
/// whether or not either happens to be armed on this machine.
pub fn claims(
    alert: &Alert,
    cfg: &GrafanaFeature,
    reaper_cfg: &ReaperFeature,
    unhealthy_cfg: &UnhealthyHostFeature,
) -> bool {
    identifies(alert, cfg)
        && !reaper::identifies(alert, reaper_cfg)
        && !unhealthy_host::identifies(alert, unhealthy_cfg)
}

/// The id of a member that is a fully running replacement for `old_id`, if
/// there is one.
///
/// **Three things must all be true**: it is not the terminated instance
/// (which keeps showing in the group for a while), the ASG calls it
/// `InService` and `Healthy`, and EC2 itself says `running`. The ASG can mark
/// a box serving before the instance is fully up, and a box can be `running`
/// while the group has not accepted it.
///
/// `state_of` is called **only** for members that already pass the ASG
/// checks, so a group with nothing eligible costs no `describe-instances`.
/// Members are examined in id order so the answer is deterministic.
pub fn ready_replacement(
    members: &[AsgInstance],
    old_id: &str,
    state_of: &dyn Fn(&str) -> String,
) -> Option<String> {
    let mut candidates: Vec<&AsgInstance> = members
        .iter()
        .filter(|m| m.instance_id != old_id && m.is_serving())
        .collect();
    candidates.sort_by(|a, b| a.instance_id.cmp(&b.instance_id));
    candidates
        .into_iter()
        .find(|m| state_of(&m.instance_id).trim().eq_ignore_ascii_case("running"))
        .map(|m| m.instance_id.clone())
}

/// The two windows, in milliseconds, so the state machine never reads the
/// feature struct and every test can name its own numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    /// From the replacement being ready to the escalation check.
    pub settle_ms: u64,
    /// From the terminate to giving up on a replacement that never came up.
    pub ceiling_ms: u64,
}

impl Timing {
    pub fn from_feature(cfg: &GrafanaFeature) -> Self {
        Self {
            settle_ms: cfg.settle_window().as_millis() as u64,
            ceiling_ms: cfg.replacement_ceiling().as_millis() as u64,
        }
    }
}

/// Whether a new alert started an incident or joined one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Begin {
    /// First alert for this ASG: acknowledge it, terminate, and track it.
    Owner,
    /// Another alert owns this ASG. Acknowledge it and do nothing else.
    Duplicate { owner: String },
}

/// One incident handed back to the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Due {
    pub alert_id: String,
    /// `createdAt` verbatim, for the escalation subject.
    pub created_at: String,
    pub asg: String,
    pub facts: AlertFacts,
    /// The instance that was terminated, so the replacement is never it.
    pub old_instance_id: String,
}

/// What the caller should do about an incident this poll.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Read the group and call [`ready_replacement`]; on `Some`, call
    /// [`GrafanaState::replacement_ready`].
    WatchReplacement(Due),
    /// The replacement never became ready: escalate, then
    /// [`GrafanaState::finish`].
    CeilingReached(Due),
    /// Re-read the alert. Closed: log and [`GrafanaState::finish`]. Still
    /// open: escalate, then finish. A failed read: do nothing; this step is
    /// handed back next poll.
    SettleEnded(Due),
}

#[derive(Clone, Debug)]
enum Phase {
    /// Begun, terminate not yet issued (or just failed and about to be
    /// finished). Nothing is due.
    Starting,
    Replacing { deadline_ms: u64 },
    Settling { deadline_ms: u64 },
}

#[derive(Clone, Debug)]
struct Incident {
    alert_id: String,
    created_at: String,
    facts: AlertFacts,
    phase: Phase,
}

/// What this process has decided, and what it is still waiting on.
///
/// Owned by the poll thread and never shared, so there is no lock — the
/// property the other watchers' state keeps. Keyed on the ASG name in a
/// `BTreeMap` so [`Self::due`] has a stable order.
#[derive(Debug)]
pub struct GrafanaState {
    timing: Timing,
    seen: HashSet<String>,
    incidents: BTreeMap<String, Incident>,
}

impl GrafanaState {
    pub fn new(timing: Timing) -> Self {
        Self { timing, seen: HashSet::new(), incidents: BTreeMap::new() }
    }

    pub fn is_seen(&self, alert_id: &str) -> bool {
        self.seen.contains(alert_id)
    }

    /// Record an alert as decided, so it is reported once rather than per poll.
    pub fn mark_seen(&mut self, alert_id: &str) {
        self.seen.insert(alert_id.to_string());
    }

    /// Which alert owns `asg`, if any.
    pub fn owner_of(&self, asg: &str) -> Option<&str> {
        self.incidents.get(asg).map(|i| i.alert_id.as_str())
    }

    /// Open an incident on `asg`, or report who already owns it.
    ///
    /// **A duplicate changes nothing.** In particular it never touches the
    /// owner's phase or deadline: a steady trickle of duplicates would
    /// otherwise hold the escalation off indefinitely, which is exactly when
    /// it is needed.
    pub fn begin(
        &mut self,
        alert_id: &str,
        created_at: &str,
        facts: &AlertFacts,
        asg: &str,
    ) -> Begin {
        if let Some(existing) = self.incidents.get(asg) {
            return Begin::Duplicate { owner: existing.alert_id.clone() };
        }
        self.incidents.insert(
            asg.to_string(),
            Incident {
                alert_id: alert_id.to_string(),
                created_at: created_at.to_string(),
                facts: facts.clone(),
                phase: Phase::Starting,
            },
        );
        Begin::Owner
    }

    /// The terminate went out: start waiting for the replacement, with the
    /// ceiling measured from now.
    pub fn terminated(&mut self, asg: &str, now_ms: u64) {
        if let Some(i) = self.incidents.get_mut(asg) {
            if matches!(i.phase, Phase::Starting) {
                i.phase = Phase::Replacing {
                    deadline_ms: now_ms.saturating_add(self.timing.ceiling_ms),
                };
            }
        }
    }

    /// The replacement is fully running: start the settle timer. A no-op in
    /// any other phase, so a second call can never push the deadline out.
    pub fn replacement_ready(&mut self, asg: &str, now_ms: u64) {
        if let Some(i) = self.incidents.get_mut(asg) {
            if matches!(i.phase, Phase::Replacing { .. }) {
                i.phase = Phase::Settling {
                    deadline_ms: now_ms.saturating_add(self.timing.settle_ms),
                };
            }
        }
    }

    /// What needs doing this poll, ordered by ASG name. **Does not consume
    /// anything**: a step is handed back every poll until [`Self::finish`], so
    /// a failed read is retried and an escalation is sent exactly once (the
    /// caller finishes the incident right after sending).
    pub fn due(&self, now_ms: u64) -> Vec<Step> {
        let mut out = Vec::new();
        for (asg, i) in &self.incidents {
            let due = || Due {
                alert_id: i.alert_id.clone(),
                created_at: i.created_at.clone(),
                asg: asg.clone(),
                facts: i.facts.clone(),
                old_instance_id: i.facts.instance_id.clone(),
            };
            match i.phase {
                Phase::Starting => {}
                Phase::Replacing { deadline_ms } if now_ms >= deadline_ms => {
                    out.push(Step::CeilingReached(due()))
                }
                Phase::Replacing { .. } => out.push(Step::WatchReplacement(due())),
                Phase::Settling { deadline_ms } if now_ms >= deadline_ms => {
                    out.push(Step::SettleEnded(due()))
                }
                Phase::Settling { .. } => {}
            }
        }
        out
    }

    /// The incident is over: closed, escalated, refused or failed. Frees the
    /// ASG for the next one.
    pub fn finish(&mut self, asg: &str) {
        self.incidents.remove(asg);
    }

    /// For the heartbeat.
    pub fn active_count(&self) -> usize {
        self.incidents.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(titles: &[&str]) -> GrafanaFeature {
        GrafanaFeature {
            title_contains: titles.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    fn alert(title: &str) -> Alert {
        Alert { message: title.to_string(), ..Default::default() }
    }

    fn member(id: &str, lifecycle: &str, health: &str) -> AsgInstance {
        AsgInstance {
            instance_id: id.to_string(),
            instance_type: None,
            availability_zone: None,
            lifecycle_state: lifecycle.to_string(),
            health_status: health.to_string(),
            protected_from_scale_in: false,
        }
    }

    fn facts(instance: &str) -> AlertFacts {
        AlertFacts { instance_id: instance.to_string(), ..Default::default() }
    }

    fn timing() -> Timing {
        Timing { settle_ms: 12 * 60_000, ceiling_ms: 25 * 60_000 }
    }

    // ---- matching -------------------------------------------------------

    #[test]
    fn each_shipped_title_is_identified_case_insensitively() {
        let c = cfg(&[
            "Grafana-Missing-Filebeat-Info",
            "Grafana-Missing-Qualys-Info",
            "Grafana-Missing-Node-Exporter-Info",
            "Grafana-Missing-SplunkForwarder-Info",
            "Grafana-Missing-Crowdstrike-Info",
        ]);
        for t in [
            "[FIRING] Grafana-Missing-Filebeat-Info",
            "grafana-missing-qualys-info on host",
            "Grafana-Missing-Node-Exporter-Info",
            "Grafana-Missing-SplunkForwarder-Info",
            "GRAFANA-MISSING-CROWDSTRIKE-INFO",
        ] {
            assert!(identifies(&alert(t), &c), "{t}");
        }
        assert!(!identifies(&alert("Grafana-Disk-Full"), &c));
    }

    #[test]
    fn a_blank_title_entry_matches_nothing() {
        let c = cfg(&["", "   "]);
        assert!(!identifies(&alert("anything at all"), &c));
        assert!(!identifies(&alert(""), &c));
        assert!(!identifies(&alert("Grafana-Missing-Qualys-Info"), &GrafanaFeature::default()));
    }

    #[test]
    fn reapers_and_unhealthy_hosts_alerts_are_never_claimed() {
        let c = cfg(&["Grafana-Missing"]);
        let reaper_cfg = ReaperFeature {
            message_contains: "reaper".to_string(),
            ..Default::default()
        };
        let a = alert("Grafana-Missing reaper thing");
        assert!(identifies(&a, &c));
        assert!(!claims(&a, &c, &reaper_cfg, &UnhealthyHostFeature::default()));
        assert!(claims(
            &alert("Grafana-Missing-Qualys-Info"),
            &c,
            &reaper_cfg,
            &UnhealthyHostFeature::default()
        ));
    }

    // ---- the replacement predicate -------------------------------------

    #[test]
    fn the_old_instance_is_never_its_own_replacement() {
        let members = vec![member("i-old", "InService", "Healthy")];
        let running = |_: &str| "running".to_string();
        assert_eq!(ready_replacement(&members, "i-old", &running), None);
    }

    #[test]
    fn a_pending_or_unhealthy_member_is_not_ready() {
        let running = |_: &str| "running".to_string();
        let pending = vec![member("i-new", "Pending", "Healthy")];
        assert_eq!(ready_replacement(&pending, "i-old", &running), None);
        let unhealthy = vec![member("i-new", "InService", "Unhealthy")];
        assert_eq!(ready_replacement(&unhealthy, "i-old", &running), None);
    }

    #[test]
    fn serving_but_not_running_is_not_ready() {
        let members = vec![member("i-new", "InService", "Healthy")];
        let pending = |_: &str| "pending".to_string();
        assert_eq!(ready_replacement(&members, "i-old", &pending), None);
        let unreadable = |_: &str| String::new();
        assert_eq!(ready_replacement(&members, "i-old", &unreadable), None);
        let running = |_: &str| "running".to_string();
        assert_eq!(
            ready_replacement(&members, "i-old", &running),
            Some("i-new".to_string())
        );
    }

    #[test]
    fn the_ec2_state_is_only_read_for_members_that_could_be_ready() {
        use std::cell::Cell;
        let calls = Cell::new(0);
        let members = vec![
            member("i-old", "InService", "Healthy"),
            member("i-boot", "Pending", "Healthy"),
        ];
        let state = |_: &str| {
            calls.set(calls.get() + 1);
            "running".to_string()
        };
        assert_eq!(ready_replacement(&members, "i-old", &state), None);
        assert_eq!(calls.get(), 0, "no describe-instances for a member that cannot qualify");
    }

    // ---- the incident state machine -------------------------------------

    #[test]
    fn the_first_alert_for_an_asg_owns_it() {
        let mut s = GrafanaState::new(timing());
        assert_eq!(s.begin("a1", "t1", &facts("i-old"), "grafana-asg"), Begin::Owner);
        assert_eq!(s.owner_of("grafana-asg"), Some("a1"));
        assert_eq!(
            s.begin("a2", "t2", &facts("i-old"), "grafana-asg"),
            Begin::Duplicate { owner: "a1".to_string() }
        );
        assert_eq!(s.owner_of("grafana-asg"), Some("a1"), "a duplicate never takes the slot");
        assert_eq!(s.active_count(), 1);
    }

    #[test]
    fn a_different_asg_is_a_different_incident() {
        let mut s = GrafanaState::new(timing());
        assert_eq!(s.begin("a1", "t", &facts("i-1"), "asg-1"), Begin::Owner);
        assert_eq!(s.begin("a2", "t", &facts("i-2"), "asg-2"), Begin::Owner);
        assert_eq!(s.active_count(), 2);
    }

    #[test]
    fn a_duplicate_never_owns_or_refreshes() {
        let mut s = GrafanaState::new(timing());
        s.begin("a1", "t1", &facts("i-old"), "g");
        s.terminated("g", 1_000);
        // A duplicate arriving much later must not move the owner's deadline.
        let _ = s.begin("a2", "t2", &facts("i-old"), "g");
        let ceiling = 1_000 + 25 * 60_000;
        assert!(matches!(s.due(ceiling - 1).as_slice(), [Step::WatchReplacement(_)]));
        assert!(matches!(s.due(ceiling).as_slice(), [Step::CeilingReached(d)] if d.alert_id == "a1"));
    }

    #[test]
    fn an_alert_during_settling_is_a_duplicate() {
        // The replacement boots and raises its own Missing-* alerts. Those
        // must not terminate it.
        let mut s = GrafanaState::new(timing());
        s.begin("a1", "t1", &facts("i-old"), "g");
        s.terminated("g", 0);
        s.replacement_ready("g", 60_000);
        assert_eq!(
            s.begin("a-new", "t9", &facts("i-new"), "g"),
            Begin::Duplicate { owner: "a1".to_string() }
        );
    }

    #[test]
    fn nothing_is_due_before_the_terminate() {
        let mut s = GrafanaState::new(timing());
        s.begin("a1", "t1", &facts("i-old"), "g");
        assert!(s.due(10_000_000).is_empty(), "begun but not yet terminated");
    }

    #[test]
    fn the_replacement_is_watched_every_poll_until_the_ceiling() {
        let mut s = GrafanaState::new(timing());
        s.begin("a1", "t1", &facts("i-old"), "g");
        s.terminated("g", 0);
        for now in [0, 30_000, 24 * 60_000] {
            match s.due(now).as_slice() {
                [Step::WatchReplacement(d)] => {
                    assert_eq!(d.old_instance_id, "i-old");
                    assert_eq!(d.asg, "g");
                    assert_eq!(d.alert_id, "a1");
                }
                other => panic!("at {now}: {other:?}"),
            }
        }
        assert!(matches!(s.due(25 * 60_000).as_slice(), [Step::CeilingReached(_)]));
    }

    #[test]
    fn the_settle_timer_starts_when_the_replacement_is_ready_not_at_the_terminate() {
        let mut s = GrafanaState::new(timing());
        s.begin("a1", "t1", &facts("i-old"), "g");
        s.terminated("g", 0);
        s.replacement_ready("g", 5 * 60_000);
        assert!(s.due(16 * 60_000 + 59_999).is_empty(), "12 min after ready, not after terminate");
        assert!(matches!(
            s.due(17 * 60_000).as_slice(),
            [Step::SettleEnded(d)] if d.alert_id == "a1" && d.created_at == "t1"
        ));
    }

    #[test]
    fn once_settling_the_ceiling_no_longer_applies() {
        let mut s = GrafanaState::new(timing());
        s.begin("a1", "t1", &facts("i-old"), "g");
        s.terminated("g", 0);
        s.replacement_ready("g", 24 * 60_000);
        // Past the old ceiling but inside the settle window: nothing due.
        assert!(s.due(26 * 60_000).is_empty());
    }

    #[test]
    fn replacement_ready_is_a_no_op_outside_the_replacing_phase() {
        let mut s = GrafanaState::new(timing());
        s.begin("a1", "t1", &facts("i-old"), "g");
        s.replacement_ready("g", 0); // not terminated yet
        assert!(s.due(100_000_000).is_empty());
        s.terminated("g", 0);
        s.replacement_ready("g", 1_000);
        s.replacement_ready("g", 9_000_000); // second call must not push the deadline
        assert!(matches!(s.due(1_000 + 12 * 60_000).as_slice(), [Step::SettleEnded(_)]));
    }

    #[test]
    fn settle_end_is_handed_back_until_finished() {
        // A failed re-read at the end must be retried next poll: due() does
        // not consume the step; only finish() does.
        let mut s = GrafanaState::new(timing());
        s.begin("a1", "t1", &facts("i-old"), "g");
        s.terminated("g", 0);
        s.replacement_ready("g", 0);
        let end = 12 * 60_000;
        assert_eq!(s.due(end).len(), 1);
        assert_eq!(s.due(end + 30_000).len(), 1, "still handed back, so a failed read retries");
        s.finish("g");
        assert!(s.due(end + 60_000).is_empty());
        assert_eq!(s.active_count(), 0);
    }

    #[test]
    fn finishing_frees_the_asg_for_a_new_incident() {
        let mut s = GrafanaState::new(timing());
        s.begin("a1", "t1", &facts("i-old"), "g");
        s.finish("g");
        assert_eq!(s.begin("a9", "t9", &facts("i-9"), "g"), Begin::Owner);
    }

    #[test]
    fn seen_alerts_are_remembered() {
        let mut s = GrafanaState::new(timing());
        assert!(!s.is_seen("a1"));
        s.mark_seen("a1");
        assert!(s.is_seen("a1"));
    }

    #[test]
    fn due_steps_come_back_in_a_stable_order() {
        let mut s = GrafanaState::new(timing());
        for (alert, asg) in [("a2", "b-asg"), ("a1", "a-asg")] {
            s.begin(alert, "t", &facts("i"), asg);
            s.terminated(asg, 0);
        }
        let ids: Vec<String> = s
            .due(0)
            .into_iter()
            .map(|st| match st {
                Step::WatchReplacement(d) => d.asg,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(ids, vec!["a-asg".to_string(), "b-asg".to_string()]);
    }

    #[test]
    fn timing_comes_from_the_feature_windows() {
        let t = Timing::from_feature(&GrafanaFeature::default());
        assert_eq!(t.settle_ms, 12 * 60_000);
        assert_eq!(t.ceiling_ms, 25 * 60_000);
    }
}

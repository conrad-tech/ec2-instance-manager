//! Deciding what to do about an application `UnHealthyHostCount` alert.
//!
//! Pure — alerts and target health in, decisions out — so every rule is
//! pinned by a test rather than by reading a poll loop. The GUI owns every
//! network hop: the on-call lookup, the JSM reads, the ELB health call, the
//! `terminate-instances`, the escalation email.
//!
//! The job, and all of it, on call only:
//!
//! 1. acknowledge the alert;
//! 2. after `ack_wait`, read the target group and terminate one unhealthy
//!    instance (a Vault group with neither instance healthy gets a second
//!    terminate after `vault_retry`);
//! 3. escalate if the alert is still open `after_terminate` later.
//!
//! And the insurance rule: a second alert for the same target group inside
//! `insurance_window` is not acknowledged and nothing is done to it — the
//! app counts and notifies, and the page reaches a human.
//!
//! Reaper's own `UnHealthyHostCount` alarm is excluded by [`claims`]: wherever
//! `reaper::identifies` is true this module does nothing.

use std::collections::{HashMap, HashSet};

use crate::alerts::Alert;
use crate::features::{ReaperFeature, UnhealthyHostFeature};
use crate::reaper::{self, TargetMember};

/// `true` when `hay` contains `needle`, case-insensitively. A blank needle
/// matches nothing — an unconfigured rule must not match every alert on the
/// feed and start terminating instances.
fn contains_ci(hay: &str, needle: &str) -> bool {
    let n = needle.trim();
    if n.is_empty() {
        return false;
    }
    hay.to_ascii_lowercase().contains(&n.to_ascii_lowercase())
}

/// Is this alert one this watcher's rules match? Reads the same three
/// fields reaper and pingdom read, in the same order, and only
/// `alertname` by name — never the whole `extraProperties` map, which
/// carries a flattened `{{extraProperties}}` copy of itself.
pub fn identifies(alert: &Alert, cfg: &UnhealthyHostFeature) -> bool {
    let alertname = alert.extra.get("alertname").map(String::as_str).unwrap_or("");
    contains_ci(alertname, &cfg.alertname_contains)
        || contains_ci(&alert.app, &cfg.app_contains)
        || contains_ci(&alert.message, &cfg.message_contains)
}

/// Is this alert ours to act on? **Reaper wins.** The reaper alarm is itself
/// an `UnHealthyHostCount` alarm, so the two rule sets overlap by
/// construction, and reaper's fix must never be replaced by a terminate —
/// whether or not reaper happens to be armed on this machine.
pub fn claims(alert: &Alert, cfg: &UnhealthyHostFeature, reaper_cfg: &ReaperFeature) -> bool {
    identifies(alert, cfg) && !reaper::identifies(alert, reaper_cfg)
}

/// What shape of target group the alert is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Any application group: one terminate, then wait.
    Ordinary,
    /// A Vault group holds exactly two instances, one active and one
    /// standby. Neither healthy is the case that needs a second terminate.
    Vault,
}

impl Kind {
    /// How many instances an incident of this kind may terminate in total.
    pub fn budget(self) -> u32 {
        match self {
            Self::Ordinary => 1,
            Self::Vault => 2,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Ordinary => "ordinary",
            Self::Vault => "vault",
        }
    }
}

/// Which kind a target group is, from its name.
pub fn kind_of(tg_name: &str, cfg: &UnhealthyHostFeature) -> Kind {
    if contains_ci(tg_name, &cfg.vault_tg_contains) {
        Kind::Vault
    } else {
        Kind::Ordinary
    }
}

/// How long the incident waits after a check, in the caller's units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    /// `after_terminate_mins`, then escalate if the alert is still open.
    AfterTerminate,
    /// `vault_retry_mins`, then read health and plan again.
    VaultRetry,
}

/// What one health check decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// The instance to terminate, if any. **Always one reported
    /// `unhealthy`**, never a healthy, draining or IP target.
    pub terminate: Option<String>,
    pub wait: Wait,
    /// One sentence for the log saying why.
    pub reason: String,
}

fn no_terminate(reason: String) -> Plan {
    Plan { terminate: None, wait: Wait::AfterTerminate, reason }
}

/// The lowest-id instance target in `members` whose health is `state`.
/// Lowest rather than first so the choice is deterministic across calls
/// and testable; the API's order is not stable.
fn lowest_with(members: &[TargetMember], state: &str) -> Option<String> {
    members
        .iter()
        .filter(|m| m.health == state)
        .filter(|m| reaper::find_instance_id(&m.id).as_deref() == Some(m.id.as_str()))
        .map(|m| m.id.clone())
        .min()
}

/// Decide what to do with a target group whose alert has not cleared.
///
/// `spent` is how many terminates this incident has already made; at
/// [`Kind::budget`] nothing more is terminated whatever health says. For
/// Vault the rule is the same at every check — *a healthy instance exists,
/// so terminate the unhealthy one and wait the long window; none does, so
/// terminate one and re-check after the short window* — which is what
/// makes the second check after a replacement comes up healthy the ordinary
/// case rather than a special one.
pub fn plan(members: &[TargetMember], kind: Kind, spent: u32) -> Plan {
    if spent >= kind.budget() {
        return no_terminate(format!(
            "termination budget spent ({spent} of {}) — terminating nothing more",
            kind.budget()
        ));
    }
    let instances = members
        .iter()
        .filter(|m| reaper::find_instance_id(&m.id).as_deref() == Some(m.id.as_str()))
        .count();
    let unhealthy = lowest_with(members, "unhealthy");
    // Filtered the same way `instances` is: an IP target counted here would
    // flip the Vault "neither instance healthy" case into the "one is
    // healthy" one on a group that also happens to register an IP target,
    // which under-terminates (safe) but logs a false sentence — "one Vault
    // instance is healthy" — about a destructive decision when no Vault
    // INSTANCE is healthy at all.
    let any_healthy = members
        .iter()
        .filter(|m| reaper::find_instance_id(&m.id).as_deref() == Some(m.id.as_str()))
        .any(|m| m.health == "healthy");

    match kind {
        Kind::Ordinary => match unhealthy {
            Some(id) => Plan {
                reason: format!("terminating {id} ({} instance target(s) registered)", instances),
                terminate: Some(id),
                wait: Wait::AfterTerminate,
            },
            None => no_terminate(format!(
                "no unhealthy instance target registered ({} instance target(s)) — nothing to \
                 terminate",
                instances
            )),
        },
        Kind::Vault => {
            if instances != 2 {
                return no_terminate(format!(
                    "a Vault target group should hold exactly two instances, found {instances} — \
                     terminating nothing"
                ));
            }
            match (unhealthy, any_healthy) {
                (Some(id), true) => Plan {
                    reason: format!("one Vault instance is healthy — terminating the unhealthy {id}"),
                    terminate: Some(id),
                    wait: Wait::AfterTerminate,
                },
                (Some(id), false) => Plan {
                    reason: format!(
                        "neither Vault instance is healthy — terminating {id} and re-reading \
                         health after the short window"
                    ),
                    terminate: Some(id),
                    wait: Wait::VaultRetry,
                },
                (None, _) => no_terminate(
                    "no unhealthy Vault instance — nothing to terminate".to_string(),
                ),
            }
        }
    }
}

/// The `aws:autoscaling:groupName` tag value out of a `describe-instances`
/// response, in its full shape (`Reservations[].Instances[].Tags[]`) — not
/// pre-filtered by a `--query`, so this is exactly what the AWS CLI returns
/// and can be tested against a captured payload with no AWS involved. `None`
/// when the instance carries no such tag, or when `json` does not parse.
///
/// **The whole justification for terminating an unhealthy instance is that
/// an auto scaling group replaces it.** A target group can just as well hold
/// standalone instances — hand-built, Terraform-managed, a migration
/// leftover — and terminating one of those loses capacity permanently with
/// nothing to replace it. Every other failure mode in this feature is
/// recoverable; this one is not, which is why the GUI's caller
/// (`instance_asg_group`) refuses the terminate outright on `None`, whether
/// that means "no tag" or "could not read the instance at all". Follows the
/// shape of `reaper::parse_target_health`: pure, parses the raw JSON,
/// tested here without AWS.
pub fn parse_asg_group(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let tags = v
        .get("Reservations")?
        .as_array()?
        .first()?
        .get("Instances")?
        .as_array()?
        .first()?
        .get("Tags")?
        .as_array()?;
    tags.iter().find_map(|t| {
        let key = t.get("Key")?.as_str()?;
        if key != "aws:autoscaling:groupName" {
            return None;
        }
        t.get("Value")?.as_str().map(str::to_string)
    })
}

/// The four windows, in milliseconds, so the state machine never reads the
/// feature struct and every test can name its own numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    pub ack_wait_ms: u64,
    pub after_terminate_ms: u64,
    pub vault_retry_ms: u64,
    pub insurance_window_ms: u64,
}

impl Timing {
    pub fn from_feature(cfg: &UnhealthyHostFeature) -> Self {
        Self {
            ack_wait_ms: cfg.ack_wait().as_millis() as u64,
            after_terminate_ms: cfg.after_terminate().as_millis() as u64,
            vault_retry_ms: cfg.vault_retry().as_millis() as u64,
            insurance_window_ms: cfg.insurance_window().as_millis() as u64,
        }
    }
}

/// When each target group's alerts arrived, for the insurance rule.
#[derive(Debug, Default)]
pub struct InsuranceLedger {
    arrivals: HashMap<String, Vec<u64>>,
}

impl InsuranceLedger {
    /// Record an arrival for `resource` and return how many arrived inside
    /// the last `window_ms`, this one included. The window slides: an
    /// arrival older than the window is forgotten.
    pub fn record(&mut self, resource: &str, now_ms: u64, window_ms: u64) -> usize {
        let list = self.arrivals.entry(resource.to_string()).or_default();
        list.retain(|&t| now_ms.saturating_sub(t) < window_ms);
        list.push(now_ms);
        list.len()
    }
}

/// What the caller should do about one alert.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Already decided on an earlier poll.
    Ignore,
    /// The insurance case: another alert for a target group that alerted
    /// inside the window. **Not acknowledged, nothing done.** `title` and
    /// `count` are for the log line and the toolbar banner.
    Insured { title: String, count: usize },
    /// The window has passed but the earlier incident never closed. Not
    /// acknowledged: nothing here may run two incidents on one group.
    SlotHeld { owner: String },
    /// The first alert of an incident: acknowledge it and start the clock.
    AckAndWatch,
}

/// Where an incident is in its sequence.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Stage {
    /// Acknowledged, or waiting out a Vault retry: at the deadline the
    /// caller reads health and `plan` decides.
    AwaitingCheck,
    /// `due` handed the check to the caller; `advance` moves on.
    Checking,
    /// A decision was made: at the deadline the alert still being open
    /// escalates.
    AwaitingDeadline,
    /// Escalated once. The slot is held until the alert closes.
    Escalated,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Incident {
    owner: String,
    created_at: String,
    account_id: String,
    kind: Kind,
    stage: Stage,
    deadline_ms: u64,
    spent: u32,
}

/// What the caller must do now for one incident.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Read the target group's health, call [`plan`], act, then
    /// [`UnhealthyHostState::advance`].
    Check,
    /// The alert is still open at the deadline: send the escalation.
    Escalate,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Due {
    pub resource: String,
    pub owner: String,
    /// `createdAt` verbatim, for the escalation subject.
    pub created_at: String,
    /// The alert's `Account:` tag, for the AWS context.
    pub account_id: String,
    pub kind: Kind,
    pub spent: u32,
    pub phase: Phase,
}

/// What this process has acknowledged, is timing, and has escalated.
///
/// Owned by the poll thread and never shared, so there is no lock — the
/// property [`crate::pingdom::PingdomState`] keeps.
#[derive(Debug)]
pub struct UnhealthyHostState {
    timing: Timing,
    /// One live incident per target group resource id. Its presence is the
    /// slot.
    incidents: HashMap<String, Incident>,
    seen: HashSet<String>,
    ledger: InsuranceLedger,
}

impl UnhealthyHostState {
    pub fn new(timing: Timing) -> Self {
        Self {
            timing,
            incidents: HashMap::new(),
            seen: HashSet::new(),
            ledger: InsuranceLedger::default(),
        }
    }

    pub fn is_seen(&self, alert_id: &str) -> bool {
        self.seen.contains(alert_id)
    }

    /// Record an alert as decided without deciding anything — for an alert
    /// that names no target group, so it is reported once rather than once
    /// per poll.
    pub fn mark_seen(&mut self, alert_id: &str) {
        self.seen.insert(alert_id.to_string());
    }

    /// Decide what to do about `alert`, and record the decision.
    ///
    /// Insurance is checked before the slot: a second alert inside the
    /// window is insured whether or not the first incident is still
    /// running. Recording here rather than in the caller matches
    /// `mark_handled`-before-spawn in reaper — a failed acknowledge must not
    /// be retried on every poll.
    pub fn consider(&mut self, alert: &Alert, resource: &str, kind: Kind, now_ms: u64) -> Action {
        if self.seen.contains(&alert.id) {
            return Action::Ignore;
        }
        self.seen.insert(alert.id.clone());

        let count = self.ledger.record(resource, now_ms, self.timing.insurance_window_ms);
        if count >= 2 {
            return Action::Insured { title: alert.message.clone(), count };
        }
        if let Some(held) = self.incidents.get(resource) {
            return Action::SlotHeld { owner: held.owner.clone() };
        }
        self.incidents.insert(
            resource.to_string(),
            Incident {
                owner: alert.id.clone(),
                created_at: alert.created_at.clone(),
                account_id: alert.account.clone(),
                kind,
                stage: Stage::AwaitingCheck,
                deadline_ms: now_ms.saturating_add(self.timing.ack_wait_ms),
                spent: 0,
            },
        );
        Action::AckAndWatch
    }

    /// The owning alert ids, which the caller re-reads by id every poll.
    pub fn watched_alert_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.incidents.values().map(|i| i.owner.clone()).collect();
        ids.sort();
        ids
    }

    /// Every incident whose current deadline has passed. A check is handed
    /// out once (the stage moves to `Checking` until `advance`); an
    /// escalation is marked at once so one incident escalates exactly once.
    pub fn due(&mut self, now_ms: u64) -> Vec<Due> {
        let mut out = Vec::new();
        let mut keys: Vec<&String> = self.incidents.keys().collect();
        keys.sort();
        let keys: Vec<String> = keys.into_iter().cloned().collect();
        for resource in keys {
            let inc = self.incidents.get_mut(&resource).expect("key just listed");
            if now_ms < inc.deadline_ms {
                continue;
            }
            let phase = match inc.stage {
                Stage::AwaitingCheck => {
                    inc.stage = Stage::Checking;
                    Phase::Check
                }
                Stage::AwaitingDeadline => {
                    inc.stage = Stage::Escalated;
                    Phase::Escalate
                }
                Stage::Checking | Stage::Escalated => continue,
            };
            out.push(Due {
                resource: resource.clone(),
                owner: inc.owner.clone(),
                created_at: inc.created_at.clone(),
                account_id: inc.account_id.clone(),
                kind: inc.kind,
                spent: inc.spent,
                phase,
            });
        }
        out
    }

    /// The caller has acted on a `Phase::Check`. `terminated` says whether a
    /// terminate call actually succeeded — a failed one spends none of the
    /// budget, and the wait still runs because the alert can clear on its
    /// own.
    pub fn advance(&mut self, resource: &str, wait: Wait, terminated: bool, now_ms: u64) {
        let Some(inc) = self.incidents.get_mut(resource) else { return };
        if terminated {
            inc.spent += 1;
        }
        let (stage, ms) = match wait {
            Wait::AfterTerminate => (Stage::AwaitingDeadline, self.timing.after_terminate_ms),
            Wait::VaultRetry => (Stage::AwaitingCheck, self.timing.vault_retry_ms),
        };
        inc.stage = stage;
        inc.deadline_ms = now_ms.saturating_add(ms);
    }

    /// `alert_id` has closed. Only the owner's close ends an incident; an
    /// insured duplicate closing says nothing about the outage.
    pub fn closed(&mut self, alert_id: &str) {
        self.incidents.retain(|_, i| i.owner != alert_id);
    }

    /// For the heartbeat.
    pub fn incident_count(&self) -> usize {
        self.incidents.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> UnhealthyHostFeature {
        UnhealthyHostFeature {
            enabled: true,
            allowed_users: vec!["bconrad".to_string()],
            message_contains: "UnHealthyHostCount".to_string(),
            vault_tg_contains: "vault".to_string(),
            ..Default::default()
        }
    }

    fn reaper_cfg() -> ReaperFeature {
        ReaperFeature {
            enabled: true,
            allowed_users: vec!["bconrad".to_string()],
            alertname_contains: "reaper".to_string(),
            ..Default::default()
        }
    }

    fn alert(title: &str) -> Alert {
        Alert {
            id: "a1".to_string(),
            status: "open".to_string(),
            message: title.to_string(),
            created_at: "2026-09-11T14:03:11Z".to_string(),
            ..Default::default()
        }
    }

    fn member(id: &str, health: &str) -> TargetMember {
        TargetMember { id: id.to_string(), health: health.to_string() }
    }

    // ---- identification ----

    #[test]
    fn identifies_on_each_field_and_a_blank_rule_matches_nothing() {
        assert!(identifies(&alert("[Target Group]: prod-App-UnHealthyHostCount-Critical"), &cfg()));
        assert!(!identifies(&alert("[Target Group]: prod-App-Latency-Critical"), &cfg()));

        let mut by_name = alert("something");
        by_name.extra.insert("alertname".to_string(), "App-UnHealthyHostCount".to_string());
        let name_rule = UnhealthyHostFeature {
            alertname_contains: "unhealthyhostcount".to_string(),
            message_contains: String::new(),
            ..cfg()
        };
        assert!(identifies(&by_name, &name_rule), "case-insensitive on alertname");

        let blank = UnhealthyHostFeature {
            alertname_contains: String::new(),
            app_contains: String::new(),
            message_contains: String::new(),
            ..cfg()
        };
        assert!(!identifies(&alert("UnHealthyHostCount"), &blank));
    }

    #[test]
    fn an_alert_reaper_identifies_is_never_claimed() {
        // The reaper alarm IS an UnHealthyHostCount alarm, so the rules
        // overlap by construction. Reaper wins.
        let mut both = alert("[Target Group]: prod-Reaper-UnHealthyHostCount-Critical");
        both.extra.insert("alertname".to_string(), "Reaper-UnHealthyHostCount".to_string());
        assert!(identifies(&both, &cfg()), "this watcher's rule does match it");
        assert!(!claims(&both, &cfg(), &reaper_cfg()), "but reaper wins");

        let ours = alert("[Target Group]: prod-App-UnHealthyHostCount-Critical");
        assert!(claims(&ours, &cfg(), &reaper_cfg()));
    }

    // ---- kind ----

    #[test]
    fn a_vault_group_is_named_by_substring_and_blank_disables_it() {
        assert_eq!(kind_of("prod-Vault-tg", &cfg()), Kind::Vault);
        assert_eq!(kind_of("prod-app-tg", &cfg()), Kind::Ordinary);
        let off = UnhealthyHostFeature { vault_tg_contains: String::new(), ..cfg() };
        assert_eq!(kind_of("prod-vault-tg", &off), Kind::Ordinary);
        assert_eq!(Kind::Ordinary.budget(), 1);
        assert_eq!(Kind::Vault.budget(), 2);
    }

    // ---- plan: ordinary ----

    #[test]
    fn an_ordinary_group_terminates_one_unhealthy_instance_and_waits_the_long_window() {
        let p = plan(&[member("i-00000001", "healthy"), member("i-00000002", "unhealthy")], Kind::Ordinary, 0);
        assert_eq!(p.terminate.as_deref(), Some("i-00000002"));
        assert_eq!(p.wait, Wait::AfterTerminate);
    }

    #[test]
    fn several_unhealthy_in_an_ordinary_group_still_terminates_only_one() {
        // The lowest id, so the choice is deterministic and testable.
        let p = plan(
            &[member("i-00000003", "unhealthy"), member("i-00000001", "unhealthy"), member("i-00000002", "unhealthy")],
            Kind::Ordinary,
            0,
        );
        assert_eq!(p.terminate.as_deref(), Some("i-00000001"));
    }

    #[test]
    fn nothing_unhealthy_terminates_nothing_and_keeps_the_deadline() {
        let p = plan(&[member("i-00000001", "healthy"), member("i-00000002", "healthy")], Kind::Ordinary, 0);
        assert_eq!(p.terminate, None);
        assert_eq!(p.wait, Wait::AfterTerminate);
        assert!(p.reason.contains("no unhealthy"), "{}", p.reason);

        let empty = plan(&[], Kind::Ordinary, 0);
        assert_eq!(empty.terminate, None);
    }

    #[test]
    fn only_an_unhealthy_member_is_ever_named() {
        // draining, initial, unused: none of these is "unhealthy", and a
        // draining target during a deploy must not be terminated.
        let p = plan(
            &[member("i-00000001", "draining"), member("i-00000002", "initial"), member("i-00000003", "unused")],
            Kind::Ordinary,
            0,
        );
        assert_eq!(p.terminate, None);
    }

    #[test]
    fn an_ip_target_is_never_named() {
        // An IP-type target cannot be terminated; find_instance_id refuses it.
        let p = plan(&[member("10.0.1.5", "unhealthy")], Kind::Ordinary, 0);
        assert_eq!(p.terminate, None);
    }

    #[test]
    fn a_spent_budget_terminates_nothing_whatever_health_says() {
        let all_bad = [member("i-00000001", "unhealthy"), member("i-00000002", "unhealthy")];
        let p = plan(&all_bad, Kind::Ordinary, 1);
        assert_eq!(p.terminate, None);
        assert_eq!(p.wait, Wait::AfterTerminate);
        assert!(p.reason.contains("budget"), "{}", p.reason);

        let v = plan(&all_bad, Kind::Vault, 2);
        assert_eq!(v.terminate, None);
        assert_eq!(v.wait, Wait::AfterTerminate);
    }

    // ---- plan: vault ----

    #[test]
    fn vault_one_healthy_one_unhealthy_is_the_ordinary_terminate() {
        let p = plan(&[member("i-00000001", "healthy"), member("i-00000002", "unhealthy")], Kind::Vault, 0);
        assert_eq!(p.terminate.as_deref(), Some("i-00000002"));
        assert_eq!(p.wait, Wait::AfterTerminate);
    }

    #[test]
    fn vault_with_neither_healthy_terminates_one_and_waits_the_short_window() {
        let p = plan(&[member("i-00000002", "unhealthy"), member("i-00000001", "unhealthy")], Kind::Vault, 0);
        assert_eq!(p.terminate.as_deref(), Some("i-00000001"));
        assert_eq!(p.wait, Wait::VaultRetry);
    }

    #[test]
    fn the_vault_recheck_applies_the_same_rule_in_both_outcomes() {
        // The replacement came up healthy, the other is still unhealthy:
        // now the ordinary case, terminate it and wait the long window.
        let recovered = plan(&[member("i-00000003", "healthy"), member("i-00000002", "unhealthy")], Kind::Vault, 1);
        assert_eq!(recovered.terminate.as_deref(), Some("i-00000002"));
        assert_eq!(recovered.wait, Wait::AfterTerminate);

        // The replacement did not come up: terminate the other and wait the
        // short window. That spends the budget, so the next check
        // terminates nothing.
        let still_bad = plan(&[member("i-00000003", "unhealthy"), member("i-00000002", "unhealthy")], Kind::Vault, 1);
        assert_eq!(still_bad.terminate.as_deref(), Some("i-00000002"));
        assert_eq!(still_bad.wait, Wait::VaultRetry);

        // Both healthy: the alert should clear on its own.
        let fine = plan(&[member("i-00000003", "healthy"), member("i-00000002", "healthy")], Kind::Vault, 1);
        assert_eq!(fine.terminate, None);
        assert_eq!(fine.wait, Wait::AfterTerminate);
    }

    #[test]
    fn a_vault_group_not_holding_two_instances_is_left_alone() {
        for members in [
            vec![member("i-00000001", "unhealthy")],
            vec![member("i-00000001", "unhealthy"), member("i-00000002", "unhealthy"), member("i-00000003", "healthy")],
            vec![],
        ] {
            let p = plan(&members, Kind::Vault, 0);
            assert_eq!(p.terminate, None, "{:?}", members);
            assert_eq!(p.wait, Wait::AfterTerminate);
            assert!(p.reason.contains("two"), "{}", p.reason);
        }
    }

    #[test]
    fn vault_any_healthy_ignores_a_healthy_ip_target() {
        // Two Vault instances, both unhealthy, plus a registered IP target
        // that happens to be healthy. Before the filter, `any_healthy`
        // counted the IP target and picked the "one instance is healthy"
        // branch — under-terminating safely, but logging a false sentence
        // ("one Vault instance is healthy") about a destructive decision
        // when no Vault INSTANCE is healthy at all.
        let p = plan(
            &[
                member("i-00000001", "unhealthy"),
                member("i-00000002", "unhealthy"),
                member("10.0.1.5", "healthy"),
            ],
            Kind::Vault,
            0,
        );
        assert_eq!(p.terminate.as_deref(), Some("i-00000001"));
        assert_eq!(
            p.wait,
            Wait::VaultRetry,
            "a healthy IP target must not count as the healthy instance: {}",
            p.reason
        );
        assert!(p.reason.contains("neither"), "{}", p.reason);
    }

    // ---- the ASG tag ----

    fn describe_instances_json(tags: &[(&str, &str)]) -> String {
        let pairs: Vec<String> = tags
            .iter()
            .map(|(k, v)| format!(r#"{{"Key":"{k}","Value":"{v}"}}"#))
            .collect();
        format!(
            r#"{{"Reservations":[{{"Instances":[{{"InstanceId":"i-0abc123","Tags":[{}]}}]}}]}}"#,
            pairs.join(",")
        )
    }

    #[test]
    fn parse_asg_group_finds_the_tag_among_others() {
        let json = describe_instances_json(&[
            ("Name", "app-web-1"),
            ("aws:autoscaling:groupName", "app-web-asg"),
            ("Environment", "prod"),
        ]);
        assert_eq!(parse_asg_group(&json).as_deref(), Some("app-web-asg"));
    }

    #[test]
    fn parse_asg_group_is_none_without_the_asg_tag() {
        let json = describe_instances_json(&[("Name", "standalone-box"), ("Environment", "prod")]);
        assert_eq!(parse_asg_group(&json), None);
    }

    #[test]
    fn parse_asg_group_is_none_on_malformed_json() {
        assert_eq!(parse_asg_group("not json"), None);
        assert_eq!(parse_asg_group(r#"{"Reservations":[]}"#), None);
        assert_eq!(parse_asg_group(r#"{"Reservations":[{"Instances":[]}]}"#), None);
        assert_eq!(
            parse_asg_group(r#"{"Reservations":[{"Instances":[{"Tags":"oops"}]}]}"#),
            None
        );
    }

    // ---- insurance ----

    const MIN: u64 = 60_000;

    #[test]
    fn the_ledger_counts_arrivals_inside_a_sliding_window() {
        let mut l = InsuranceLedger::default();
        assert_eq!(l.record("tg/app", 0, 60 * MIN), 1);
        assert_eq!(l.record("tg/app", 50 * MIN, 60 * MIN), 2);
        // Minute 0 has aged out, minute 50 has not.
        assert_eq!(l.record("tg/app", 70 * MIN, 60 * MIN), 2);
        // Another target group is its own count.
        assert_eq!(l.record("tg/other", 70 * MIN, 60 * MIN), 1);
    }

    // ---- state ----

    fn timing() -> Timing {
        Timing {
            ack_wait_ms: 7 * MIN,
            after_terminate_ms: 10 * MIN,
            vault_retry_ms: 7 * MIN,
            insurance_window_ms: 60 * MIN,
        }
    }

    fn open(id: &str, account: &str) -> Alert {
        Alert {
            id: id.to_string(),
            status: "open".to_string(),
            message: "[Target Group]: prod-App-UnHealthyHostCount-Critical".to_string(),
            created_at: "2026-09-11T14:03:11Z".to_string(),
            account: account.to_string(),
            ..Default::default()
        }
    }

    const TG: &str = "targetgroup/prod-app-tg/17bb79ec89f6d7d9";

    #[test]
    fn the_first_alert_acks_and_watches_and_is_decided_once() {
        let mut s = UnhealthyHostState::new(timing());
        assert_eq!(s.consider(&open("a1", "111"), TG, Kind::Ordinary, 0), Action::AckAndWatch);
        assert_eq!(s.consider(&open("a1", "111"), TG, Kind::Ordinary, 30_000), Action::Ignore);
        assert!(s.is_seen("a1"));
        assert_eq!(s.watched_alert_ids(), vec!["a1".to_string()]);
        assert_eq!(s.incident_count(), 1);
    }

    #[test]
    fn a_second_alert_inside_the_hour_is_insured_with_its_count_and_never_acked() {
        let mut s = UnhealthyHostState::new(timing());
        assert_eq!(s.consider(&open("a1", "111"), TG, Kind::Ordinary, 0), Action::AckAndWatch);
        assert_eq!(
            s.consider(&open("a2", "111"), TG, Kind::Ordinary, 20 * MIN),
            Action::Insured {
                title: "[Target Group]: prod-App-UnHealthyHostCount-Critical".to_string(),
                count: 2
            }
        );
        assert_eq!(
            s.consider(&open("a3", "111"), TG, Kind::Ordinary, 40 * MIN),
            Action::Insured {
                title: "[Target Group]: prod-App-UnHealthyHostCount-Critical".to_string(),
                count: 3
            }
        );
        // Still one incident: the insured alerts started nothing.
        assert_eq!(s.incident_count(), 1);
        assert_eq!(s.watched_alert_ids(), vec!["a1".to_string()]);
        // And they are seen, so they are counted once, not once per poll.
        assert!(s.is_seen("a2") && s.is_seen("a3"));
    }

    #[test]
    fn a_held_slot_after_the_hour_is_reported_not_reused() {
        let mut s = UnhealthyHostState::new(timing());
        s.consider(&open("a1", "111"), TG, Kind::Ordinary, 0);
        // 90 minutes on, the first alert never closed. The ledger has aged
        // out, so this is not insurance — but the slot is held, and nothing
        // here may start a second incident on the same group.
        assert_eq!(
            s.consider(&open("a2", "111"), TG, Kind::Ordinary, 90 * MIN),
            Action::SlotHeld { owner: "a1".to_string() }
        );
    }

    #[test]
    fn a_different_target_group_is_its_own_incident() {
        let mut s = UnhealthyHostState::new(timing());
        s.consider(&open("a1", "111"), TG, Kind::Ordinary, 0);
        assert_eq!(
            s.consider(&open("a2", "111"), "targetgroup/prod-vault-tg/0000000000000000", Kind::Vault, MIN),
            Action::AckAndWatch
        );
        assert_eq!(s.incident_count(), 2);
    }

    #[test]
    fn the_check_falls_due_after_ack_wait_and_only_once() {
        let mut s = UnhealthyHostState::new(timing());
        s.consider(&open("a1", "111"), TG, Kind::Vault, 0);
        assert!(s.due(6 * MIN).is_empty());
        let due = s.due(7 * MIN);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].phase, Phase::Check);
        assert_eq!(due[0].resource, TG);
        assert_eq!(due[0].owner, "a1");
        assert_eq!(due[0].account_id, "111");
        assert_eq!(due[0].kind, Kind::Vault);
        assert_eq!(due[0].spent, 0);
        // Not returned again while the caller is reading health.
        assert!(s.due(7 * MIN + 1).is_empty());
    }

    #[test]
    fn advancing_after_a_terminate_schedules_the_escalation_deadline() {
        let mut s = UnhealthyHostState::new(timing());
        s.consider(&open("a1", "111"), TG, Kind::Ordinary, 0);
        let _ = s.due(7 * MIN);
        s.advance(TG, Wait::AfterTerminate, true, 7 * MIN);
        assert!(s.due(16 * MIN).is_empty());
        let due = s.due(17 * MIN);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].phase, Phase::Escalate);
        assert_eq!(due[0].spent, 1);
        assert_eq!(due[0].created_at, "2026-09-11T14:03:11Z");
        // Escalated once. The slot is still held until the alert closes.
        assert!(s.due(60 * MIN).is_empty());
        assert_eq!(s.incident_count(), 1);
    }

    #[test]
    fn a_vault_retry_schedules_another_check_and_a_failed_terminate_spends_nothing() {
        let mut s = UnhealthyHostState::new(timing());
        s.consider(&open("a1", "111"), TG, Kind::Vault, 0);
        let _ = s.due(7 * MIN);
        // Terminate call failed: the budget is untouched, the wait still
        // runs, because the alert can still clear on its own.
        s.advance(TG, Wait::VaultRetry, false, 7 * MIN);
        let again = s.due(14 * MIN);
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].phase, Phase::Check);
        assert_eq!(again[0].spent, 0);
        s.advance(TG, Wait::VaultRetry, true, 14 * MIN);
        let third = s.due(21 * MIN);
        assert_eq!(third[0].phase, Phase::Check);
        assert_eq!(third[0].spent, 1);
        s.advance(TG, Wait::AfterTerminate, true, 21 * MIN);
        let esc = s.due(31 * MIN);
        assert_eq!(esc[0].phase, Phase::Escalate);
        assert_eq!(esc[0].spent, 2);
    }

    #[test]
    fn only_the_owners_close_ends_the_incident() {
        let mut s = UnhealthyHostState::new(timing());
        s.consider(&open("a1", "111"), TG, Kind::Ordinary, 0);
        s.consider(&open("a2", "111"), TG, Kind::Ordinary, MIN); // insured
        s.closed("a2");
        assert_eq!(s.incident_count(), 1, "a duplicate closing says nothing about the outage");
        s.closed("a1");
        assert_eq!(s.incident_count(), 0);
        assert!(s.watched_alert_ids().is_empty());
        // The slot is free: a fresh alert after the hour starts a fresh incident.
        assert_eq!(
            s.consider(&open("a3", "111"), TG, Kind::Ordinary, 61 * MIN),
            Action::AckAndWatch
        );
    }

    #[test]
    fn a_closed_incident_fires_nothing() {
        let mut s = UnhealthyHostState::new(timing());
        s.consider(&open("a1", "111"), TG, Kind::Ordinary, 0);
        s.closed("a1");
        assert!(s.due(60 * MIN).is_empty());
    }

    #[test]
    fn timing_comes_from_the_feature_with_zero_meaning_default() {
        let t = Timing::from_feature(&UnhealthyHostFeature::default());
        assert_eq!(t.ack_wait_ms, 7 * MIN);
        assert_eq!(t.after_terminate_ms, 10 * MIN);
        assert_eq!(t.vault_retry_ms, 7 * MIN);
        assert_eq!(t.insurance_window_ms, 60 * MIN);
    }
}

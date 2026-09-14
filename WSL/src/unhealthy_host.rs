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

#[allow(unused_imports)]
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
    let any_healthy = members.iter().any(|m| m.health == "healthy");

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
}

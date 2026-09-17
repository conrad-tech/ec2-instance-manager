//! Deciding what to do about an **instance age** alert.
//!
//! Pure — alert facts and a view of the auto scaling group in, decisions out —
//! so every rule is pinned by a test rather than by reading a poll loop. The
//! GUI owns every network hop: the on-call lookup, the JSM reads, the four AWS
//! describes, the `terminate-instances` and the escalation email.
//!
//! The job, and all of it, on call only:
//!
//! 1. acknowledge the alert;
//! 2. resolve the instance it names to its auto scaling group;
//! 3. confirm the group has not already lost an instance inside
//!    `recent_terminate_hours`, and that losing this one leaves a healthy
//!    group;
//! 4. terminate it, so the ASG replaces it.
//!
//! Anything that cannot be confirmed escalates instead. Reaper's and the
//! unhealthy-host watcher's alarms are excluded by [`claims`].

use crate::reaper;
use std::collections::{HashMap, HashSet};

/// The five fields these alerts carry, as `Key: value` lines in the alert's
/// **description**.
///
/// Every field is a `String` and empty is the only "absent": a field that is
/// missing and a field that came back as an unrendered template are the same
/// thing to every consumer, and two spellings of absent is a branch somebody
/// forgets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AlertFacts {
    pub application: String,
    /// Normalised through [`reaper::find_instance_id`], so this is either a
    /// real instance id or empty. It ends up on argv for a terminate.
    pub instance_id: String,
    pub environment: String,
    pub account: String,
    pub region: String,
}

/// Characters that mean the feed served a value it never rendered.
///
/// A live pull found two of ten alerts with a templated `App:` tag, and
/// `pingdom` refuses the same set for the same reason: keying an incident —
/// here, a terminate — on a template string files unrelated outages under one
/// name.
const TEMPLATE_CHARS: [char; 5] = ['{', '}', '%', '<', '>'];

/// A value, trimmed, or empty when it is blank or carries a template marker.
fn clean_value(raw: &str) -> String {
    let v = raw.trim();
    if v.is_empty() || v.contains(TEMPLATE_CHARS) {
        return String::new();
    }
    v.to_string()
}

/// Read the five facts out of an alert description.
///
/// **The first occurrence of a key wins, even when it is refused.** A second,
/// well-formed line must not rescue a templated first one: the first is the
/// authoritative field, and picking a later value out of prose is how a
/// terminate ends up aimed by a runbook paragraph.
pub fn parse_facts(description: &str) -> AlertFacts {
    let mut application: Option<String> = None;
    let mut instance_id: Option<String> = None;
    let mut environment: Option<String> = None;
    let mut account: Option<String> = None;
    let mut region: Option<String> = None;

    for line in description.lines() {
        // The FIRST colon only: a value carrying one is otherwise truncated.
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let slot = if key.eq_ignore_ascii_case("Application") {
            &mut application
        } else if key.eq_ignore_ascii_case("Instance ID") {
            &mut instance_id
        } else if key.eq_ignore_ascii_case("Environment") {
            &mut environment
        } else if key.eq_ignore_ascii_case("Account") {
            &mut account
        } else if key.eq_ignore_ascii_case("Region") {
            &mut region
        } else {
            continue;
        };
        if slot.is_none() {
            *slot = Some(clean_value(value));
        }
    }

    AlertFacts {
        application: application.unwrap_or_default(),
        // Whitelisted rather than escaped, the stance reaper takes with every
        // id it puts on argv. A value that is not an instance id is empty.
        instance_id: instance_id
            .and_then(|v| reaper::find_instance_id(&v))
            .unwrap_or_default(),
        environment: environment.unwrap_or_default(),
        account: account.unwrap_or_default(),
        region: region.unwrap_or_default(),
    }
}

/// `true` when `hay` contains `needle`, case-insensitively. **A blank needle
/// matches nothing** — an unconfigured rule must not match every alert on the
/// feed and start terminating instances. The same helper, with the same rule,
/// as `reaper::contains_ci` and `unhealthy_host::contains_ci`.
fn contains_ci(hay: &str, needle: &str) -> bool {
    let n = needle.trim();
    if n.is_empty() {
        return false;
    }
    hay.to_ascii_lowercase().contains(&n.to_ascii_lowercase())
}

/// Is this the right *kind* of alert?
///
/// Reads the three fields the other watchers read, in the same order, and only
/// `alertname` by name — never the whole `extraProperties` map, which carries
/// a flattened `{{extraProperties}}` copy of itself.
///
/// **Answers "is this an instance-age alert", not "is it ours"** — see
/// [`names_an_app`] for the second half. Cheap: every field it reads comes
/// with the alert *list*, so an alert on somebody else's feed is declined
/// without a per-alert read.
pub fn identifies(alert: &crate::alerts::Alert, cfg: &crate::features::InstanceAgeFeature) -> bool {
    let alertname = alert.extra.get("alertname").map(String::as_str).unwrap_or("");
    contains_ci(alertname, &cfg.alertname_contains)
        || contains_ci(&alert.app, &cfg.app_contains)
        || contains_ci(&alert.message, &cfg.message_contains)
}

/// Does the alert name an application this watcher may act on?
///
/// Reads the `Application:` line parsed out of the **description**, not the
/// `App:` tag — that is the field this feed has been observed serving as an
/// unrendered `{{…}}` template, which is also why the config field is called
/// `applications` and not `app_names`: two adjacent `app_*` rules reading
/// different sources is how somebody fills in the wrong one.
///
/// **An empty list matches nothing, and so does a blank entry.** An empty
/// string is a substring of everything, so one stray `""` would quietly turn
/// the list back into "every app on the feed".
pub fn names_an_app(facts: &AlertFacts, cfg: &crate::features::InstanceAgeFeature) -> bool {
    cfg.applications
        .iter()
        .any(|app| contains_ci(&facts.application, app))
}

/// Is this alert ours to act on? **Reaper wins, and so does the unhealthy-host
/// watcher.**
///
/// Those two already have a fix and a terminate of their own for the alarms
/// they claim, and one alarm being handled twice is the failure this exists to
/// prevent — whether or not either happens to be armed on this machine, for
/// the reason `unhealthy_host::claims` ignores reaper's `enabled` flag.
///
/// Note this is only the list-level half; the caller must also check
/// [`names_an_app`] once the description has been read.
pub fn claims(
    alert: &crate::alerts::Alert,
    cfg: &crate::features::InstanceAgeFeature,
    reaper_cfg: &crate::features::ReaperFeature,
    unhealthy_cfg: &crate::features::UnhealthyHostFeature,
) -> bool {
    identifies(alert, cfg)
        && !reaper::identifies(alert, reaper_cfg)
        && !crate::unhealthy_host::identifies(alert, unhealthy_cfg)
}

/// What shape of group the alert is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Any ordinary application group: the health gate applies.
    Ordinary,
    /// A Vault group: active and standby, always two. The health gate is
    /// waived, because the peer being unwell is not a reason to leave an aged
    /// box running.
    Vault,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ordinary => "ordinary",
            Self::Vault => "vault",
        }
    }
}

/// Which kind a group is, from the ASG's own name and every target group
/// attached to it — **either** hitting is enough.
///
/// Both are AWS facts already in hand from calls the watcher makes anyway, and
/// reading both is what lets a Vault ASG with no target group still be
/// recognised. Deliberately not read from the alert's `Application` field:
/// that is free text off a feed this repo has been burned by, and it decides
/// whether a destructive safety check is skipped.
///
/// A blank rule marks nothing as Vault, so the waiver fails closed.
pub fn kind_of(
    asg_name: &str,
    tg_names: &[String],
    cfg: &crate::features::InstanceAgeFeature,
) -> Kind {
    let needle = &cfg.vault_name_contains;
    if contains_ci(asg_name, needle) || tg_names.iter().any(|n| contains_ci(n, needle)) {
        Kind::Vault
    } else {
        Kind::Ordinary
    }
}

/// How far back the recent-terminate guard reads.
///
/// **Its own limit, not `asg::ACTIVITY_LIMIT` (20).** That one sizes a detail
/// panel a human reads; this one has to span a day on a group that flaps.
/// Raising the shared constant would make every ASG detail view five times
/// heavier for a reason that has nothing to do with it.
pub const INSTANCE_AGE_ACTIVITY_LIMIT: usize = 100;

/// What the group's recent scaling activity says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryVerdict {
    /// The whole window was read and holds no termination.
    Clean,
    /// The group lost an instance inside the window.
    RecentTermination { detail: String },
    /// The page was full and its oldest entry is still newer than the cutoff,
    /// so the window was not fully read. **Not the same as clean** — this is
    /// "could not see", and the guard exists precisely to refuse on that.
    NotFullySeen { detail: String },
}

/// Does this activity describe an instance going away?
///
/// Reads `description`, which the API words consistently
/// (`Terminating EC2 instance: i-…`), rather than `cause`, which is prose
/// about *why* and mentions termination in plenty of activities that are not
/// one.
fn is_termination(a: &crate::asg::ScalingActivity) -> bool {
    contains_ci(&a.description, "terminating ec2 instance")
}

/// Parse an activity's `StartTime`. `DateTime::parse_from_rfc3339` is the same
/// parser `reaper::escalation_subject` and `Alert::created_utc` use, so "valid
/// here" and "renderable in the Alerts window" cannot come apart.
fn activity_time(a: &crate::asg::ScalingActivity) -> Option<chrono::DateTime<chrono::Utc>> {
    let raw = a.start_time.as_deref()?.trim();
    if raw.is_empty() {
        return None;
    }
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|t| t.with_timezone(&chrono::Utc))
}

/// Read the group's recent scaling activity for the terminate guard.
///
/// `limit` is what the read was bounded at, and it is load-bearing:
/// **truncation is only possible when the API filled the page**, so
/// `activities.len() < limit` means we were handed everything the group has
/// and a young group whose whole history is an hour old is `Clean` rather than
/// permanently refused.
///
/// Every ambiguity resolves toward refusing — a `Failed` termination counts,
/// and so does one whose `StartTime` cannot be read. Over-counting costs one
/// escalation a human waves through; under-counting costs a second live
/// terminate on a group that just lost an instance.
pub fn termination_history(
    activities: &[crate::asg::ScalingActivity],
    now: chrono::DateTime<chrono::Utc>,
    window: std::time::Duration,
    limit: usize,
) -> HistoryVerdict {
    let cutoff = now - chrono::Duration::from_std(window).unwrap_or_else(|_| chrono::Duration::days(1));

    for a in activities.iter().filter(|a| is_termination(a)) {
        match activity_time(a) {
            Some(t) if t < cutoff => continue,
            Some(t) => {
                return HistoryVerdict::RecentTermination {
                    detail: format!("the group terminated an instance at {t} ({})", a.status_code),
                }
            }
            // Cannot be read, so it cannot be ruled out.
            None => {
                return HistoryVerdict::RecentTermination {
                    detail: format!(
                        "the group has a termination whose start time could not be read \
                         ({:?}), which cannot be ruled out of the window",
                        a.start_time
                    ),
                }
            }
        }
    }

    // Nothing recent in what we read. Did what we read cover the window?
    if activities.len() < limit {
        return HistoryVerdict::Clean;
    }
    let oldest = activities.iter().filter_map(activity_time).min();
    match oldest {
        Some(t) if t < cutoff => HistoryVerdict::Clean,
        Some(t) => HistoryVerdict::NotFullySeen {
            detail: format!(
                "read {limit} activities and the oldest is {t}, which is inside the window — \
                 a termination before it would not have been seen"
            ),
        },
        None => HistoryVerdict::NotFullySeen {
            detail: format!(
                "read {limit} activities and none carried a readable start time, so the \
                 window cannot be shown to be clear"
            ),
        },
    }
}

/// Everything about the group that a decision needs.
#[derive(Clone, Debug, Default)]
pub struct GroupView {
    pub asg_name: String,
    /// The group's members, from `describe-auto-scaling-groups`.
    pub members: Vec<crate::asg::AsgInstance>,
    /// Target health, where the group has a target group attached.
    ///
    /// **`None` means there is no target group — never that the read failed.**
    /// A failed read is a `Refuse` decided by the caller, because being denied
    /// is not the same as being told there is nothing, and the two must not
    /// render alike.
    pub target_health: Option<Vec<crate::reaper::TargetMember>>,
}

/// What to do about one alert.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Terminate { instance_id: String, reason: String },
    /// The group was read fine and is not healthy enough yet. Re-checked on
    /// the next poll until the retry window runs out.
    Wait { reason: String },
    /// Escalate now, and never retry. Everything except `Wait` is an answer we
    /// do not have, and retrying an answer we cannot get is how an alert sits
    /// acknowledged and silent.
    Refuse { reason: String },
}

/// Is this member on its way out?
fn is_leaving(m: &crate::asg::AsgInstance) -> bool {
    let s = m.lifecycle_state.trim();
    s.eq_ignore_ascii_case("Terminated")
        || s.len() >= 11 && s[..11].eq_ignore_ascii_case("Terminating")
}

/// Is this member in good shape?
///
/// The target group is the authority for any member registered in it; a member
/// it does not know about — a `Pending` instance is not registered yet — falls
/// back to its own `InService` + `Healthy`. Skipping such a member instead
/// would let the gate pass on a group that is one box down and one box not up.
fn member_is_good(
    m: &crate::asg::AsgInstance,
    health: Option<&Vec<crate::reaper::TargetMember>>,
) -> bool {
    if let Some(list) = health {
        if let Some(t) = list.iter().find(|t| t.id == m.instance_id) {
            return t.health.eq_ignore_ascii_case("healthy");
        }
    }
    m.is_serving()
}

/// Decide what to do about one instance-age alert.
///
/// Six rules, first to fire wins. **The order is the design** — it decides
/// which sentence a human reads — and the recent-terminate guard sits above
/// the Vault waiver deliberately.
pub fn decide(
    facts: &AlertFacts,
    view: &GroupView,
    kind: Kind,
    history: &HistoryVerdict,
) -> Decision {
    // 1. Is it even in this group?
    let Some(aged) = view
        .members
        .iter()
        .find(|m| m.instance_id == facts.instance_id)
    else {
        return Decision::Refuse {
            reason: format!(
                "{} is not a member of {} ({} member(s): {}) — it moved, or the tag is stale",
                facts.instance_id,
                view.asg_name,
                view.members.len(),
                view.members
                    .iter()
                    .map(|m| m.instance_id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
    };

    // 2. Already going. Worded as a state, not a fault.
    if is_leaving(aged) {
        return Decision::Refuse {
            reason: format!(
                "{} is already terminating (lifecycle {}) — nothing to do",
                facts.instance_id, aged.lifecycle_state
            ),
        };
    }

    // 3. Somebody said this box is not to be recycled.
    if aged.protected_from_scale_in {
        return Decision::Refuse {
            reason: format!(
                "{} has scale-in protection on — this feature does not overrule that",
                facts.instance_id
            ),
        };
    }

    // 4. Has the group already lost one today? Above the Vault waiver.
    match history {
        HistoryVerdict::RecentTermination { detail } => {
            return Decision::Refuse {
                reason: format!("{} already lost an instance recently: {detail}", view.asg_name),
            }
        }
        HistoryVerdict::NotFullySeen { detail } => {
            return Decision::Refuse {
                reason: format!(
                    "{}'s recent activity could not be read far enough back: {detail}",
                    view.asg_name
                ),
            }
        }
        HistoryVerdict::Clean => {}
    }

    // 5. A Vault pair is active/standby and always two: the peer being unwell
    //    is not a reason to leave an aged box running.
    if kind == Kind::Vault && view.members.len() == 2 {
        return Decision::Terminate {
            instance_id: facts.instance_id.clone(),
            reason: format!(
                "vault pair — the health gate does not apply, terminating {}",
                facts.instance_id
            ),
        };
    }

    // 6. Every OTHER member must be in good shape. The alerted instance itself
    //    may read anything: an aged box is often already unhealthy, and
    //    requiring it to be healthy would block every terminate.
    let others: Vec<&crate::asg::AsgInstance> = view
        .members
        .iter()
        .filter(|m| m.instance_id != facts.instance_id)
        .collect();
    let health = view.target_health.as_ref();
    let unwell: Vec<&str> = others
        .iter()
        .filter(|m| !member_is_good(m, health))
        .map(|m| m.instance_id.as_str())
        .collect();

    if unwell.is_empty() {
        Decision::Terminate {
            instance_id: facts.instance_id.clone(),
            reason: format!(
                "every other member of {} is healthy ({} of them) — terminating {}",
                view.asg_name,
                others.len(),
                facts.instance_id
            ),
        }
    } else {
        Decision::Wait {
            reason: format!(
                "not terminating {} yet: {} of {}'s other member(s) are not healthy ({})",
                facts.instance_id,
                unwell.len(),
                view.asg_name,
                unwell.join(", ")
            ),
        }
    }
}

/// The one window, in milliseconds, so the state machine never reads the
/// feature struct and every test can name its own numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    pub retry_window_ms: u64,
}

impl Timing {
    pub fn from_feature(cfg: &crate::features::InstanceAgeFeature) -> Self {
        Self { retry_window_ms: cfg.retry_window().as_millis() as u64 }
    }
}

/// What the caller should do about one alert off the feed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Already decided on an earlier poll.
    Ignore,
    /// Another alert holds this group for this poll. **Deliberately not
    /// marked seen** — it is reconsidered next poll rather than swallowed.
    Deferred { holder: String },
    /// A first sighting: acknowledge it and act now.
    AckAndAct,
}

/// One incident handed back to the caller to re-check or to escalate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Due {
    pub alert_id: String,
    /// `createdAt` verbatim, for the escalation subject.
    pub created_at: String,
    /// Parsed once, when the alert was first read in full, and carried here so
    /// the description is not re-parsed on every poll.
    pub facts: AlertFacts,
    pub asg: String,
}

#[derive(Clone, Debug)]
struct Waiting {
    created_at: String,
    facts: AlertFacts,
    asg: String,
    deadline_ms: u64,
}

/// What this process has decided, and what it is still waiting on.
///
/// Owned by the poll thread and never shared, so there is no lock — the
/// property [`crate::pingdom::PingdomState`] and
/// [`crate::unhealthy_host::UnhealthyHostState`] both keep.
#[derive(Debug)]
pub struct InstanceAgeState {
    timing: Timing,
    seen: HashSet<String>,
    /// Keyed by alert id: one waiting incident per alert.
    waiting: HashMap<String, Waiting>,
    /// Groups already acted on this poll. Cleared by [`Self::begin_poll`].
    claimed: HashMap<String, String>,
}

impl InstanceAgeState {
    pub fn new(timing: Timing) -> Self {
        Self {
            timing,
            seen: HashSet::new(),
            waiting: HashMap::new(),
            claimed: HashMap::new(),
        }
    }

    /// Start of a poll: every group is unclaimed again.
    pub fn begin_poll(&mut self) {
        self.claimed.clear();
    }

    pub fn is_seen(&self, alert_id: &str) -> bool {
        self.seen.contains(alert_id)
    }

    /// Record an alert as decided without deciding anything — for one that
    /// names nothing actionable, so it is reported once rather than per poll.
    pub fn mark_seen(&mut self, alert_id: &str) {
        self.seen.insert(alert_id.to_string());
    }

    /// Decide what to do about an alert off the feed, and claim its group.
    pub fn consider(&mut self, alert_id: &str, asg: &str, _now_ms: u64) -> Action {
        if self.seen.contains(alert_id) {
            return Action::Ignore;
        }
        if let Some(holder) = self.claimed.get(asg) {
            return Action::Deferred { holder: holder.clone() };
        }
        self.claimed.insert(asg.to_string(), alert_id.to_string());
        self.seen.insert(alert_id.to_string());
        Action::AckAndAct
    }

    /// The group was read fine and is not healthy enough yet: keep re-checking
    /// until the window runs out.
    ///
    /// **Never refreshes an existing deadline.** Pushing it out on every poll
    /// is how an incident waits forever and never escalates.
    pub fn begin_wait(
        &mut self,
        alert_id: &str,
        created_at: &str,
        facts: &AlertFacts,
        asg: &str,
        now_ms: u64,
    ) {
        if self.waiting.contains_key(alert_id) {
            return;
        }
        self.waiting.insert(
            alert_id.to_string(),
            Waiting {
                created_at: created_at.to_string(),
                facts: facts.clone(),
                asg: asg.to_string(),
                deadline_ms: now_ms.saturating_add(self.timing.retry_window_ms),
            },
        );
    }

    /// The incident is over — terminated, refused, or the alert closed.
    pub fn finish(&mut self, alert_id: &str) {
        self.waiting.remove(alert_id);
    }

    /// Every waiting incident past its deadline, removed as it is handed back
    /// so one incident escalates exactly once.
    pub fn expired(&mut self, now_ms: u64) -> Vec<Due> {
        let mut ids: Vec<String> = self
            .waiting
            .iter()
            .filter(|(_, w)| now_ms >= w.deadline_ms)
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        ids.into_iter()
            .filter_map(|id| self.waiting.remove(&id).map(|w| Due {
                alert_id: id,
                created_at: w.created_at,
                facts: w.facts,
                asg: w.asg,
            }))
            .collect()
    }

    /// Waiting incidents to re-check this poll — **at most one per group**,
    /// and claiming that group so a new alert on it defers.
    ///
    /// Sorted by alert id: the feed's order is not stable, and a poll that
    /// acts in a different order every time is untestable and unreadable in a
    /// log.
    pub fn pending(&mut self, now_ms: u64) -> Vec<Due> {
        let mut ids: Vec<String> = self
            .waiting
            .iter()
            .filter(|(_, w)| now_ms < w.deadline_ms)
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();

        let mut out = Vec::new();
        for id in ids {
            let Some(w) = self.waiting.get(&id) else { continue };
            if self.claimed.contains_key(&w.asg) {
                continue;
            }
            self.claimed.insert(w.asg.clone(), id.clone());
            out.push(Due {
                alert_id: id,
                created_at: w.created_at.clone(),
                facts: w.facts.clone(),
                asg: w.asg.clone(),
            });
        }
        out
    }

    /// For the heartbeat.
    pub fn waiting_count(&self) -> usize {
        self.waiting.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alerts::Alert;
    use crate::asg::AsgInstance;
    use crate::asg::ScalingActivity;
    use crate::features::{InstanceAgeFeature, ReaperFeature, UnhealthyHostFeature};
    use crate::reaper::TargetMember;

    const REAL: &str = "\
Instance has exceeded its maximum age and should be recycled.

Application: cassandra
Instance ID: i-0abc123def4567890
Environment: prod
Account: acme-prod
Region: us-east-1

Runbook: https://example.invalid/runbook
";

    #[test]
    fn the_five_facts_are_read_out_of_a_real_description() {
        let f = parse_facts(REAL);
        assert_eq!(f.application, "cassandra");
        assert_eq!(f.instance_id, "i-0abc123def4567890");
        assert_eq!(f.environment, "prod");
        assert_eq!(f.account, "acme-prod");
        assert_eq!(f.region, "us-east-1");
    }

    #[test]
    fn the_keys_are_case_insensitive_and_order_does_not_matter() {
        let f = parse_facts(
            "region: US-EAST-2\r\nACCOUNT: acme-dev\r\n  instance id : i-0123abcd\r\n\
             application: Kafka\r\nEnvironment: DEV1\r\n",
        );
        assert_eq!(f.region, "US-EAST-2");
        assert_eq!(f.account, "acme-dev");
        assert_eq!(f.instance_id, "i-0123abcd", "CRLF and padding are trimmed");
        assert_eq!(f.application, "Kafka");
        assert_eq!(f.environment, "DEV1");
    }

    #[test]
    fn a_missing_key_is_empty_and_never_a_panic() {
        let f = parse_facts("Application: cassandra\n");
        assert_eq!(f.application, "cassandra");
        assert!(f.instance_id.is_empty());
        assert!(f.environment.is_empty());
        assert!(f.account.is_empty());
        assert!(f.region.is_empty());
        assert_eq!(parse_facts(""), AlertFacts::default());
    }

    #[test]
    fn the_first_occurrence_of_a_key_wins_even_when_it_is_refused() {
        // A second, well-formed line must NOT rescue a templated first one:
        // that would silently pick a value out of prose further down the body
        // after the authoritative field came back unrendered.
        let f = parse_facts("Application: {{app}}\nApplication: cassandra\n");
        assert!(
            f.application.is_empty(),
            "the first Application line was templated, so there is no answer"
        );
        let g = parse_facts("Account: acme-prod\nAccount: acme-dev\n");
        assert_eq!(g.account, "acme-prod");
    }

    #[test]
    fn a_templated_value_is_refused_on_every_field() {
        // This feed has been observed serving unrendered {{...}} and &{%...%}%
        // where values should be. Keying a terminate on a template string is
        // how unrelated outages get filed as one application.
        for bad in [
            "{{Application}}",
            "&{%app%}%",
            "<not set>",
            "100%",
            "}leftover",
        ] {
            let f = parse_facts(&format!(
                "Application: {bad}\nInstance ID: {bad}\nEnvironment: {bad}\n\
                 Account: {bad}\nRegion: {bad}\n"
            ));
            assert_eq!(
                f,
                AlertFacts::default(),
                "{bad:?} must not survive as a value on any field"
            );
        }
    }

    #[test]
    fn the_value_keeps_everything_after_the_first_colon() {
        // Split on the FIRST colon only, or a value carrying one is truncated.
        let f = parse_facts("Region: us-east-1\nAccount: team:platform\n");
        assert_eq!(f.account, "team:platform");
        assert_eq!(f.region, "us-east-1");
    }

    #[test]
    fn the_instance_id_is_normalised_and_a_non_id_is_refused() {
        // Whitelisted, not escaped -- the same stance reaper takes, because
        // this string ends up on argv for a terminate.
        let f = parse_facts("Instance ID: i-0abc123def4567890 (us-east-1a)\n");
        assert_eq!(f.instance_id, "i-0abc123def4567890");
        let g = parse_facts("Instance ID: not-an-instance\n");
        assert!(g.instance_id.is_empty());
        let h = parse_facts("Instance ID: i-00zz\n");
        assert!(h.instance_id.is_empty(), "wrong length and not hex");
    }

    #[test]
    fn a_line_with_no_colon_and_an_unknown_key_are_both_ignored() {
        let f = parse_facts("just some prose\nSeverity: critical\nRegion: eu-west-1\n");
        assert_eq!(f.region, "eu-west-1");
        assert!(f.application.is_empty());
    }

    fn cfg() -> InstanceAgeFeature {
        InstanceAgeFeature {
            enabled: true,
            allowed_users: vec!["bconrad".to_string()],
            message_contains: "InstanceAge".to_string(),
            applications: vec!["cassandra".to_string()],
            vault_name_contains: "vault".to_string(),
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

    fn uh_cfg() -> UnhealthyHostFeature {
        UnhealthyHostFeature {
            enabled: true,
            allowed_users: vec!["bconrad".to_string()],
            message_contains: "UnHealthyHostCount".to_string(),
            title_app_names: vec!["prod".to_string()],
            ..Default::default()
        }
    }

    fn alert(title: &str) -> Alert {
        Alert {
            id: "a1".to_string(),
            status: "open".to_string(),
            message: title.to_string(),
            created_at: "2026-09-17T14:03:11Z".to_string(),
            ..Default::default()
        }
    }

    fn facts(app: &str) -> AlertFacts {
        AlertFacts { application: app.to_string(), ..Default::default() }
    }

    #[test]
    fn each_contains_rule_identifies_on_its_own_field() {
        let mut c = InstanceAgeFeature::default();

        c.message_contains = "InstanceAge".to_string();
        assert!(identifies(&alert("prod-cassandra-InstanceAge-Warning"), &c));
        assert!(identifies(&alert("PROD-INSTANCEAGE"), &c), "case-insensitive");
        assert!(!identifies(&alert("prod-cassandra-UnHealthyHostCount"), &c));

        let mut c = InstanceAgeFeature::default();
        c.alertname_contains = "instance_age".to_string();
        let mut a = alert("anything");
        a.extra.insert("alertname".to_string(), "aws_instance_age".to_string());
        assert!(identifies(&a, &c));

        let mut c = InstanceAgeFeature::default();
        c.app_contains = "cass".to_string();
        let mut a = alert("anything");
        a.app = "cassandra".to_string();
        assert!(identifies(&a, &c));
    }

    #[test]
    fn a_build_with_every_rule_blank_identifies_nothing() {
        // The shipped state. An unconfigured watcher must not claim every
        // alert on somebody else's feed.
        let c = InstanceAgeFeature::default();
        assert!(!identifies(&alert("prod-cassandra-InstanceAge-Warning"), &c));
        let mut a = alert("x");
        a.app = "cassandra".to_string();
        a.extra.insert("alertname".to_string(), "instance_age".to_string());
        assert!(!identifies(&a, &c));
    }

    #[test]
    fn the_application_list_narrows_and_an_empty_or_blank_list_names_nobody() {
        let c = cfg();
        assert!(names_an_app(&facts("cassandra"), &c));
        assert!(names_an_app(&facts("CASSANDRA-01"), &c), "substring, any case");
        assert!(!names_an_app(&facts("kafka"), &c));

        let empty = InstanceAgeFeature { applications: vec![], ..cfg() };
        assert!(!names_an_app(&facts("cassandra"), &empty));

        let blank = InstanceAgeFeature { applications: vec!["".to_string()], ..cfg() };
        assert!(
            !names_an_app(&facts("cassandra"), &blank),
            "an empty needle is a substring of everything and must claim nothing"
        );

        assert!(
            !names_an_app(&facts(""), &c),
            "an application that could not be read names nobody"
        );
    }

    #[test]
    fn reaper_and_the_unhealthy_host_watcher_both_win_over_this_one() {
        // One alarm must never be both fixed and terminated, whether or not
        // those watchers happen to be armed on this machine.
        let mut reaper_alert = alert("prod-reaper-InstanceAge-Warning");
        reaper_alert
            .extra
            .insert("alertname".to_string(), "reaper-down".to_string());
        assert!(identifies(&reaper_alert, &cfg()));
        assert!(!claims(&reaper_alert, &cfg(), &reaper_cfg(), &uh_cfg()));

        let uh_alert = alert("prod-cassandra-UnHealthyHostCount-InstanceAge");
        assert!(identifies(&uh_alert, &cfg()));
        assert!(!claims(&uh_alert, &cfg(), &reaper_cfg(), &uh_cfg()));

        let ours = alert("prod-cassandra-InstanceAge-Warning");
        assert!(claims(&ours, &cfg(), &reaper_cfg(), &uh_cfg()));
    }

    #[test]
    fn a_vault_group_is_recognised_by_its_asg_name_or_any_attached_target_group() {
        let c = cfg();
        assert_eq!(kind_of("prod-vault-asg", &[], &c), Kind::Vault);
        assert_eq!(kind_of("PROD-VAULT-ASG", &[], &c), Kind::Vault, "case-insensitive");
        assert_eq!(
            kind_of("prod-cassandra-asg", &["prod-vault-tg".to_string()], &c),
            Kind::Vault,
            "an attached target group names it even when the ASG does not"
        );
        assert_eq!(
            kind_of(
                "prod-cassandra-asg",
                &["prod-cassandra-tg".to_string(), "prod-vault-tg".to_string()],
                &c
            ),
            Kind::Vault,
            "any one of them is enough"
        );
        assert_eq!(kind_of("prod-cassandra-asg", &[], &c), Kind::Ordinary);
        assert_eq!(
            kind_of("prod-cassandra-asg", &["prod-kafka-tg".to_string()], &c),
            Kind::Ordinary
        );
    }

    #[test]
    fn a_blank_vault_rule_marks_nothing_as_vault() {
        // Blank disables the waiver. An empty needle is a substring of
        // everything, and this decides whether a destructive safety check is
        // skipped -- so it must fail closed, not open.
        let c = InstanceAgeFeature { vault_name_contains: String::new(), ..cfg() };
        assert_eq!(kind_of("prod-vault-asg", &["prod-vault-tg".to_string()], &c), Kind::Ordinary);
        let c = InstanceAgeFeature { vault_name_contains: "   ".to_string(), ..cfg() };
        assert_eq!(kind_of("prod-vault-asg", &[], &c), Kind::Ordinary);
    }

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-09-17T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    fn day() -> std::time::Duration {
        std::time::Duration::from_secs(24 * 60 * 60)
    }

    fn activity(desc: &str, start: &str, status: &str) -> ScalingActivity {
        ScalingActivity {
            start_time: if start.is_empty() { None } else { Some(start.to_string()) },
            status_code: status.to_string(),
            description: desc.to_string(),
            cause: String::new(),
            status_message: None,
        }
    }

    fn terminating(start: &str) -> ScalingActivity {
        activity("Terminating EC2 instance: i-0abc123def4567890", start, "Successful")
    }

    fn launching(start: &str) -> ScalingActivity {
        activity("Launching a new EC2 instance: i-0abc123def4567890", start, "Successful")
    }

    #[test]
    fn a_termination_inside_the_window_refuses_and_one_outside_it_does_not() {
        let recent = vec![terminating("2026-09-17T09:00:00Z"), launching("2026-09-16T01:00:00Z")];
        assert!(matches!(
            termination_history(&recent, now(), day(), 100),
            HistoryVerdict::RecentTermination { .. }
        ));

        // 25 hours ago, and an older launch behind it so the window is fully
        // seen.
        let old = vec![terminating("2026-09-16T11:00:00Z"), launching("2026-09-15T01:00:00Z")];
        assert_eq!(termination_history(&old, now(), day(), 100), HistoryVerdict::Clean);
    }

    #[test]
    fn a_launch_is_not_a_termination() {
        let only_launches = vec![launching("2026-09-17T09:00:00Z"), launching("2026-09-10T09:00:00Z")];
        assert_eq!(
            termination_history(&only_launches, now(), day(), 100),
            HistoryVerdict::Clean
        );
    }

    #[test]
    fn an_empty_history_is_clean_not_unseen() {
        // A group with no recorded activity has nothing hiding past the end.
        assert_eq!(termination_history(&[], now(), day(), 100), HistoryVerdict::Clean);
    }

    #[test]
    fn a_failed_termination_still_counts() {
        // This is a safety guard: over-counting costs one escalation a human
        // waves through, under-counting costs a second live terminate.
        let failed = vec![
            activity("Terminating EC2 instance: i-0abc123def4567890", "2026-09-17T09:00:00Z", "Failed"),
            launching("2026-09-15T01:00:00Z"),
        ];
        assert!(matches!(
            termination_history(&failed, now(), day(), 100),
            HistoryVerdict::RecentTermination { .. }
        ));
    }

    #[test]
    fn an_unreadable_start_time_on_a_termination_counts_as_recent() {
        for stamp in ["", "yesterday-ish"] {
            let odd = vec![terminating(stamp), launching("2026-09-15T01:00:00Z")];
            assert!(
                matches!(
                    termination_history(&odd, now(), day(), 100),
                    HistoryVerdict::RecentTermination { .. }
                ),
                "a termination whose time cannot be read must not be assumed old ({stamp:?})"
            );
        }
    }

    #[test]
    fn a_full_page_that_does_not_reach_the_cutoff_is_not_fully_seen() {
        // Activities come back newest-first and the read is bounded, so a
        // group that flaps fills the page with the last twenty minutes while a
        // termination five hours ago sits just past the end. "Could not see"
        // is not "nothing happened".
        let page: Vec<ScalingActivity> = (0..3).map(|_| launching("2026-09-17T11:50:00Z")).collect();
        assert!(matches!(
            termination_history(&page, now(), day(), 3),
            HistoryVerdict::NotFullySeen { .. }
        ));
    }

    #[test]
    fn a_short_page_is_everything_the_group_has_even_when_it_is_all_recent() {
        // len < limit means the API handed us the lot. Without this a young
        // group would refuse every alert forever.
        let page: Vec<ScalingActivity> = (0..3).map(|_| launching("2026-09-17T11:50:00Z")).collect();
        assert_eq!(termination_history(&page, now(), day(), 100), HistoryVerdict::Clean);
    }

    #[test]
    fn a_recent_termination_outranks_an_unseen_window() {
        // Both true: the answer that refuses for the more specific reason wins,
        // so the log names the termination rather than the truncation.
        let page = vec![terminating("2026-09-17T11:50:00Z"), launching("2026-09-17T11:55:00Z")];
        assert!(matches!(
            termination_history(&page, now(), day(), 2),
            HistoryVerdict::RecentTermination { .. }
        ));
    }

    #[test]
    fn a_full_page_of_unreadable_timestamps_is_not_fully_seen() {
        let page: Vec<ScalingActivity> = (0..2).map(|_| launching("nonsense")).collect();
        assert!(matches!(
            termination_history(&page, now(), day(), 2),
            HistoryVerdict::NotFullySeen { .. }
        ));
    }

    const AGED: &str = "i-0aaa1111bbbb2222c";
    const PEER: &str = "i-0ddd3333eeee4444f";

    fn member(id: &str, lifecycle: &str, health: &str) -> AsgInstance {
        AsgInstance {
            instance_id: id.to_string(),
            lifecycle_state: lifecycle.to_string(),
            health_status: health.to_string(),
            protected_from_scale_in: false,
            ..Default::default()
        }
    }

    fn healthy(id: &str) -> AsgInstance {
        member(id, "InService", "Healthy")
    }

    fn target(id: &str, health: &str) -> TargetMember {
        TargetMember { id: id.to_string(), health: health.to_string() }
    }

    fn aged_facts() -> AlertFacts {
        AlertFacts {
            application: "cassandra".to_string(),
            instance_id: AGED.to_string(),
            environment: "prod".to_string(),
            account: "acme-prod".to_string(),
            region: "us-east-1".to_string(),
        }
    }

    fn view(members: Vec<AsgInstance>, health: Option<Vec<TargetMember>>) -> GroupView {
        GroupView {
            asg_name: "prod-cassandra-asg".to_string(),
            members,
            target_health: health,
        }
    }

    #[test]
    fn an_instance_that_is_not_a_member_of_the_group_is_refused() {
        let v = view(vec![healthy(PEER)], None);
        assert!(matches!(
            decide(&aged_facts(), &v, Kind::Ordinary, &HistoryVerdict::Clean),
            Decision::Refuse { .. }
        ));
    }

    #[test]
    fn an_instance_already_leaving_is_refused_and_says_so() {
        for state in ["Terminating", "Terminating:Wait", "Terminating:Proceed", "Terminated"] {
            let v = view(vec![member(AGED, state, "Healthy"), healthy(PEER)], None);
            let d = decide(&aged_facts(), &v, Kind::Ordinary, &HistoryVerdict::Clean);
            match d {
                Decision::Refuse { reason } => assert!(
                    reason.contains("already terminating"),
                    "{state}: the reason must read as already terminating, not as a fault: {reason}"
                ),
                other => panic!("{state}: expected a refusal, got {other:?}"),
            }
        }
    }

    #[test]
    fn scale_in_protection_refuses_and_outranks_the_health_gate() {
        let mut aged = healthy(AGED);
        aged.protected_from_scale_in = true;
        let v = view(vec![aged, healthy(PEER)], None);
        let d = decide(&aged_facts(), &v, Kind::Ordinary, &HistoryVerdict::Clean);
        match d {
            Decision::Refuse { reason } => {
                assert!(reason.contains("scale-in protection"), "{reason}")
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_recent_termination_refuses_an_ordinary_group() {
        let v = view(vec![healthy(AGED), healthy(PEER)], None);
        let history = HistoryVerdict::RecentTermination { detail: "lost one at 09:00".to_string() };
        match decide(&aged_facts(), &v, Kind::Ordinary, &history) {
            Decision::Refuse { reason } => assert!(reason.contains("lost one at 09:00"), "{reason}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_recent_termination_blocks_a_vault_pair_too() {
        // The guard sits ABOVE the Vault waiver on purpose: a Vault pair is
        // exactly where a second terminate inside a day is most dangerous,
        // because the peer that answered for the first one may be the only
        // thing serving.
        let v = view(vec![healthy(AGED), healthy(PEER)], None);
        let history = HistoryVerdict::RecentTermination { detail: "lost one at 09:00".to_string() };
        assert!(matches!(
            decide(&aged_facts(), &v, Kind::Vault, &history),
            Decision::Refuse { .. }
        ));
    }

    #[test]
    fn a_window_that_was_not_fully_seen_refuses() {
        let v = view(vec![healthy(AGED), healthy(PEER)], None);
        let history = HistoryVerdict::NotFullySeen { detail: "oldest is 11:50".to_string() };
        assert!(matches!(
            decide(&aged_facts(), &v, Kind::Ordinary, &history),
            Decision::Refuse { .. }
        ));
    }

    #[test]
    fn a_vault_pair_terminates_whatever_the_peer_reads() {
        for peer_health in ["Unhealthy", "Healthy"] {
            for peer_state in ["InService", "Pending"] {
                let v = view(
                    vec![healthy(AGED), member(PEER, peer_state, peer_health)],
                    Some(vec![target(AGED, "healthy"), target(PEER, "unhealthy")]),
                );
                assert_eq!(
                    decide(&aged_facts(), &v, Kind::Vault, &HistoryVerdict::Clean),
                    Decision::Terminate {
                        instance_id: AGED.to_string(),
                        reason: "vault pair — the health gate does not apply, terminating \
                                 i-0aaa1111bbbb2222c"
                            .to_string()
                    },
                    "peer {peer_state}/{peer_health}"
                );
            }
        }
    }

    #[test]
    fn a_vault_group_that_is_not_a_pair_falls_through_to_the_ordinary_gate() {
        // Safe, and no surprise: the waiver is justified by the two-box
        // active/standby shape, so a group that is not that shape gets the
        // ordinary rule rather than an outright refusal.
        let three = view(
            vec![healthy(AGED), member(PEER, "InService", "Unhealthy"), healthy("i-0fff5555aaaa6666b")],
            None,
        );
        assert!(matches!(
            decide(&aged_facts(), &three, Kind::Vault, &HistoryVerdict::Clean),
            Decision::Wait { .. }
        ));

        let alone = view(vec![healthy(AGED)], None);
        assert!(matches!(
            decide(&aged_facts(), &alone, Kind::Vault, &HistoryVerdict::Clean),
            Decision::Terminate { .. }
        ));
    }

    #[test]
    fn every_other_member_healthy_terminates_and_one_that_is_not_waits() {
        let good = view(
            vec![healthy(AGED), healthy(PEER)],
            Some(vec![target(AGED, "unhealthy"), target(PEER, "healthy")]),
        );
        assert!(
            matches!(
                decide(&aged_facts(), &good, Kind::Ordinary, &HistoryVerdict::Clean),
                Decision::Terminate { .. }
            ),
            "the alerted instance itself may read anything — an aged box is often unhealthy"
        );

        for bad in ["unhealthy", "draining", "initial", "unused"] {
            let v = view(
                vec![healthy(AGED), healthy(PEER)],
                Some(vec![target(AGED, "healthy"), target(PEER, bad)]),
            );
            assert!(
                matches!(
                    decide(&aged_facts(), &v, Kind::Ordinary, &HistoryVerdict::Clean),
                    Decision::Wait { .. }
                ),
                "a peer reading {bad} is not a healthy group"
            );
        }
    }

    #[test]
    fn a_one_instance_group_terminates_with_no_carve_out() {
        // There are no other members, so "every other member healthy" is
        // vacuously true. This is the rule falling out, not an exception.
        let v = view(vec![healthy(AGED)], Some(vec![target(AGED, "unhealthy")]));
        assert!(matches!(
            decide(&aged_facts(), &v, Kind::Ordinary, &HistoryVerdict::Clean),
            Decision::Terminate { .. }
        ));
    }

    #[test]
    fn with_no_target_group_the_groups_own_view_answers() {
        let serving = view(vec![healthy(AGED), healthy(PEER)], None);
        assert!(matches!(
            decide(&aged_facts(), &serving, Kind::Ordinary, &HistoryVerdict::Clean),
            Decision::Terminate { .. }
        ));

        // is_serving needs BOTH halves: InService and Healthy.
        for (state, health) in [("InService", "Unhealthy"), ("Pending", "Healthy"), ("Standby", "Healthy")] {
            let v = view(vec![healthy(AGED), member(PEER, state, health)], None);
            assert!(
                matches!(
                    decide(&aged_facts(), &v, Kind::Ordinary, &HistoryVerdict::Clean),
                    Decision::Wait { .. }
                ),
                "a peer that is {state}/{health} is not serving"
            );
        }
    }

    #[test]
    fn a_member_missing_from_the_target_group_falls_back_to_its_own_state() {
        // A Pending instance is not registered yet. Skipping it would let the
        // gate pass on a group that is one box down and one box not up.
        let v = view(
            vec![healthy(AGED), member(PEER, "Pending", "Healthy")],
            Some(vec![target(AGED, "healthy")]),
        );
        assert!(matches!(
            decide(&aged_facts(), &v, Kind::Ordinary, &HistoryVerdict::Clean),
            Decision::Wait { .. }
        ));
    }

    #[test]
    fn a_terminating_peer_is_not_a_healthy_group() {
        let v = view(vec![healthy(AGED), member(PEER, "Terminating", "Healthy")], None);
        assert!(matches!(
            decide(&aged_facts(), &v, Kind::Ordinary, &HistoryVerdict::Clean),
            Decision::Wait { .. }
        ));
    }

    #[test]
    fn an_ip_target_in_the_health_list_is_ignored_rather_than_read_as_a_member() {
        // The gate is over MEMBERS; an IP target is not one of them and has no
        // instance to match. It must not silently fail the gate.
        let v = view(
            vec![healthy(AGED), healthy(PEER)],
            Some(vec![target(PEER, "healthy"), target("10.1.2.3", "unhealthy")]),
        );
        assert!(matches!(
            decide(&aged_facts(), &v, Kind::Ordinary, &HistoryVerdict::Clean),
            Decision::Terminate { .. }
        ));
    }

    const ASG: &str = "prod-cassandra-asg";
    const OTHER_ASG: &str = "prod-kafka-asg";

    fn timing() -> Timing {
        Timing { retry_window_ms: 20 * 60 * 1000 }
    }

    fn state() -> InstanceAgeState {
        InstanceAgeState::new(timing())
    }

    #[test]
    fn a_first_sighting_acts_and_a_second_look_at_the_same_alert_does_not() {
        let mut s = state();
        s.begin_poll();
        assert_eq!(s.consider("a1", ASG, 0), Action::AckAndAct);
        assert!(s.is_seen("a1"));
        s.begin_poll();
        assert_eq!(s.consider("a1", ASG, 0), Action::Ignore);
    }

    #[test]
    fn only_one_alert_per_group_acts_in_a_poll_and_the_loser_is_not_marked_seen() {
        // Two alerts on one group landing together would otherwise both read a
        // healthy group and both terminate, before either read reflected the
        // other.
        let mut s = state();
        s.begin_poll();
        assert_eq!(s.consider("a1", ASG, 0), Action::AckAndAct);
        match s.consider("a2", ASG, 0) {
            Action::Deferred { holder } => assert_eq!(holder, "a1"),
            other => panic!("expected the second alert to defer, got {other:?}"),
        }
        assert!(
            !s.is_seen("a2"),
            "a deferred alert must be reconsidered next poll, not swallowed"
        );
        // A different group is unaffected.
        assert_eq!(s.consider("a3", OTHER_ASG, 0), Action::AckAndAct);

        // Next poll the claim is gone and a2 gets its turn.
        s.begin_poll();
        assert_eq!(s.consider("a2", ASG, 0), Action::AckAndAct);
    }

    #[test]
    fn a_waiting_incident_is_handed_back_every_poll_until_its_window_runs_out() {
        let mut s = state();
        let f = aged_facts();
        s.begin_poll();
        assert_eq!(s.consider("a1", ASG, 0), Action::AckAndAct);
        s.begin_wait("a1", "2026-09-17T14:03:11Z", &f, ASG, 0);
        assert_eq!(s.waiting_count(), 1);

        // Five minutes later: still pending, not expired.
        s.begin_poll();
        assert!(s.expired(5 * 60 * 1000).is_empty());
        let p = s.pending(5 * 60 * 1000);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].alert_id, "a1");
        assert_eq!(p[0].asg, ASG);
        assert_eq!(p[0].created_at, "2026-09-17T14:03:11Z");
        assert_eq!(p[0].facts, f, "the facts travel with the incident, parsed once");

        // Past the window: expired exactly once, and gone.
        s.begin_poll();
        let e = s.expired(21 * 60 * 1000);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].alert_id, "a1");
        assert_eq!(s.waiting_count(), 0);
        s.begin_poll();
        assert!(s.expired(60 * 60 * 1000).is_empty(), "it escalates once, not every poll");
    }

    #[test]
    fn waiting_again_never_pushes_the_deadline_out() {
        // Refreshing on every poll is how an incident waits forever and never
        // escalates.
        let mut s = state();
        let f = aged_facts();
        s.begin_wait("a1", "t", &f, ASG, 0);
        s.begin_wait("a1", "t", &f, ASG, 10 * 60 * 1000);
        s.begin_wait("a1", "t", &f, ASG, 19 * 60 * 1000);
        s.begin_poll();
        assert_eq!(
            s.expired(21 * 60 * 1000).len(),
            1,
            "the deadline is still 20 minutes after the FIRST wait"
        );
    }

    #[test]
    fn a_pending_re_check_claims_its_group_so_a_new_alert_on_it_defers() {
        let mut s = state();
        let f = aged_facts();
        s.begin_wait("a1", "t", &f, ASG, 0);
        s.begin_poll();
        assert_eq!(s.pending(1000).len(), 1);
        assert!(
            matches!(s.consider("a2", ASG, 1000), Action::Deferred { .. }),
            "the waiting incident holds the group for this poll"
        );
    }

    #[test]
    fn only_one_waiting_incident_per_group_is_handed_back_per_poll() {
        let mut s = state();
        let f = aged_facts();
        s.begin_wait("a1", "t", &f, ASG, 0);
        s.begin_wait("a2", "t", &f, ASG, 0);
        s.begin_wait("a3", "t", &f, OTHER_ASG, 0);
        s.begin_poll();
        let p = s.pending(1000);
        assert_eq!(p.len(), 2, "one per group, not one per incident");
        let groups: Vec<&str> = p.iter().map(|d| d.asg.as_str()).collect();
        assert!(groups.contains(&ASG) && groups.contains(&OTHER_ASG));
    }

    #[test]
    fn finishing_an_incident_drops_it_and_frees_nothing_else() {
        let mut s = state();
        let f = aged_facts();
        s.begin_wait("a1", "t", &f, ASG, 0);
        s.begin_wait("a2", "t", &f, OTHER_ASG, 0);
        s.finish("a1");
        assert_eq!(s.waiting_count(), 1);
        s.begin_poll();
        let left: Vec<String> = s.pending(1000).into_iter().map(|d| d.alert_id).collect();
        assert_eq!(left, vec!["a2".to_string()], "the finished one is gone, the other is not");
        s.finish("nobody");
        assert_eq!(s.waiting_count(), 1, "finishing an unknown id is a no-op");
    }

    #[test]
    fn the_handed_back_order_is_stable() {
        // The API's order is not, and a poll that acts in a different order
        // every time is untestable and unreadable in a log.
        let mut s = state();
        let f = aged_facts();
        for id in ["a3", "a1", "a2"] {
            s.begin_wait(id, "t", &f, &format!("asg-{id}"), 0);
        }
        s.begin_poll();
        let ids: Vec<String> = s.pending(1000).into_iter().map(|d| d.alert_id).collect();
        assert_eq!(ids, vec!["a1".to_string(), "a2".to_string(), "a3".to_string()]);
    }

    #[test]
    fn the_timing_comes_from_the_feature_and_zero_means_the_default() {
        let t = Timing::from_feature(&InstanceAgeFeature::default());
        assert_eq!(t.retry_window_ms, 20 * 60 * 1000);
        let t = Timing::from_feature(&InstanceAgeFeature {
            retry_window_mins: 3,
            ..Default::default()
        });
        assert_eq!(t.retry_window_ms, 3 * 60 * 1000);
    }
}

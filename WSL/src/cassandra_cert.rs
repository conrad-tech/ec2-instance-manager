//! Cassandra cert renewal and rollback: the decisions, with no egui and no
//! AWS. Orchestration lives in `cassandra_flow`.


use std::collections::BTreeMap;

use crate::models::Instance;
use crate::script_env::env_matches;

/// One Cassandra instance the dialog can offer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub instance_id: String,
    pub name: String,
    /// The `NNN` of `cassandra-NNN`.
    pub number: u32,
    /// Only a `running` instance can be reached over SSM and updated.
    pub running: bool,
}

impl Node {
    /// The cluster set: the hundreds digit of the node number.
    pub fn set(&self) -> u32 {
        self.number / 100
    }
}

/// The `NNN` out of a `cassandra-NNN` name, tolerating a prefix and an FQDN
/// suffix. One to four digits only: `cassandra-reaper` (which shares the
/// prefix) and `cassandra-12345` are not nodes.
pub fn parse_node_number(name: &str) -> Option<u32> {
    let lower = name.to_ascii_lowercase();
    let at = lower.find("cassandra-")? + "cassandra-".len();
    let digits: String = lower[at..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() || digits.len() > 4 {
        return None;
    }
    digits.parse().ok()
}

/// The environment's Cassandra nodes, ordered by node number.
pub fn nodes_from_instances(instances: &[Instance], env: &str) -> Vec<Node> {
    let mut nodes: Vec<Node> = instances
        .iter()
        .filter(|i| env_matches(i.env.as_deref(), env))
        .filter_map(|i| {
            let name = i.name.as_deref()?;
            let number = parse_node_number(name)?;
            Some(Node {
                instance_id: i.instance_id.clone(),
                name: name.to_string(),
                number,
                running: i.state == "running",
            })
        })
        .collect();
    nodes.sort_by(|a, b| {
        a.number
            .cmp(&b.number)
            .then_with(|| a.instance_id.cmp(&b.instance_id))
    });
    nodes
}

/// Heading for a set, e.g. `Set 1xx`.
pub fn set_label(set: u32) -> String {
    format!("Set {set}xx")
}

/// Instance ids the picker lets the user tick: running nodes only.
pub fn selectable_ids(all: &[Node]) -> Vec<String> {
    all.iter()
        .filter(|n| n.running)
        .map(|n| n.instance_id.clone())
        .collect()
}

/// What a selection means for the confirmation the dialog must ask.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SelectionAssessment {
    /// The set with the most selected nodes (tie: lowest set number).
    pub dominant_set: Option<u32>,
    /// Selected instance ids outside the dominant set, shown in red. Empty
    /// when every node of every set is selected, which gets `all_sets`.
    pub outside: Vec<String>,
    /// `Some(n)` when every node of `n > 1` sets is selected.
    pub all_sets: Option<usize>,
}

impl SelectionAssessment {
    pub fn needs_confirmation(&self) -> bool {
        !self.outside.is_empty() || self.all_sets.is_some()
    }
}

pub fn assess_selection(all: &[Node], selected: &[String]) -> SelectionAssessment {
    let chosen: Vec<&Node> = all
        .iter()
        .filter(|n| selected.contains(&n.instance_id))
        .collect();
    if chosen.is_empty() {
        return SelectionAssessment::default();
    }
    let mut counts: BTreeMap<u32, usize> = BTreeMap::new();
    for n in &chosen {
        *counts.entry(n.set()).or_default() += 1;
    }
    // Highest count wins; on a tie the lower set number is "greater".
    let dominant = counts
        .iter()
        .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
        .map(|(set, _)| *set);
    let mut out = SelectionAssessment {
        dominant_set: dominant,
        ..Default::default()
    };
    if counts.len() > 1 {
        // "All" means every selectable (running) node; stopped ones can't be ticked.
        let every_selectable = selectable_ids(all).iter().all(|id| selected.contains(id));
        if every_selectable {
            out.all_sets = Some(counts.len());
        } else {
            out.outside = chosen
                .iter()
                .filter(|n| Some(n.set()) != dominant)
                .map(|n| n.instance_id.clone())
                .collect();
        }
    }
    out
}

use chrono::{DateTime, NaiveDateTime};

/// What `openssl x509 -noout -subject -issuer -dates -serial` reports for the
/// cert a node is serving. Dates are epoch seconds (UTC).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CertInfo {
    pub subject: String,
    pub issuer: String,
    pub not_before: i64,
    pub not_after: i64,
    pub serial: Option<String>,
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `Sep  3 07:01:23 2025 GMT` -> epoch seconds. openssl pads single-digit
/// days with a space, so whitespace is collapsed first.
fn parse_openssl_date(s: &str) -> Option<i64> {
    let squashed = squash(s);
    NaiveDateTime::parse_from_str(&squashed, "%b %d %H:%M:%S %Y GMT")
        .ok()
        .map(|d| d.and_utc().timestamp())
}

/// `None` unless the output carries a parseable `notAfter`: a guess about
/// when a cert expires is worse than saying nothing was read.
pub fn parse_openssl(out: &str) -> Option<CertInfo> {
    let mut subject = String::new();
    let mut issuer = String::new();
    let mut not_before = None;
    let mut not_after = None;
    let mut serial = None;
    for line in out.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("subject=") {
            subject = squash(v);
        } else if let Some(v) = line.strip_prefix("issuer=") {
            issuer = squash(v);
        } else if let Some(v) = line.strip_prefix("notBefore=") {
            not_before = parse_openssl_date(v);
        } else if let Some(v) = line.strip_prefix("notAfter=") {
            not_after = parse_openssl_date(v);
        } else if let Some(v) = line.strip_prefix("serial=") {
            let v = v.trim().to_ascii_uppercase();
            if !v.is_empty() {
                serial = Some(v);
            }
        }
    }
    if subject.is_empty() || issuer.is_empty() {
        return None;
    }
    Some(CertInfo {
        subject,
        issuer,
        not_before: not_before?,
        not_after: not_after?,
        serial,
    })
}

pub fn is_expired(cert: &CertInfo, now: i64) -> bool {
    cert.not_after <= now
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CertChange {
    /// A later expiry, same subject and issuer.
    Renewed,
    /// The expiry did not move forward.
    NotRenewed,
    /// The expiry moved but the cert does not look like the one it replaced.
    Flagged(Vec<String>),
}

pub fn compare_certs(before: &CertInfo, after: &CertInfo) -> CertChange {
    if after.not_after <= before.not_after {
        return CertChange::NotRenewed;
    }
    let mut diffs = Vec::new();
    if squash(&before.subject) != squash(&after.subject) {
        diffs.push(format!(
            "subject changed: {} -> {}",
            before.subject, after.subject
        ));
    }
    if squash(&before.issuer) != squash(&after.issuer) {
        diffs.push(format!(
            "issuer changed: {} -> {}",
            before.issuer, after.issuer
        ));
    }
    if diffs.is_empty() {
        CertChange::Renewed
    } else {
        CertChange::Flagged(diffs)
    }
}

/// The cert a rollback is returning to, as far as it can be identified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OldCert {
    pub not_after: i64,
    pub serial: Option<String>,
}

impl From<&CertInfo> for OldCert {
    fn from(c: &CertInfo) -> Self {
        OldCert {
            not_after: c.not_after,
            serial: c.serial.clone(),
        }
    }
}

/// Whether what a node serves is `old`. The serial is compared only when both
/// sides have one (a keystore backup read through keytool may not).
pub fn served_matches(served: &CertInfo, old: &OldCert) -> bool {
    served.not_after == old.not_after
        && match (&served.serial, &old.serial) {
            (Some(a), Some(b)) => a == b,
            _ => true,
        }
}

/// The environment name as it appears in SSM paths: lowercased.
pub fn env_domain(env_name: &str) -> String {
    env_name.trim().to_ascii_lowercase()
}

/// A token safe to interpolate into a shell command and an SSM path.
pub fn valid_domain_token(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && s.len() <= 63
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

pub fn expand_parameter(template: &str, env_domain: &str) -> Result<String, String> {
    if !template.contains("$env_domain") {
        return Err(format!(
            "parameter template '{template}' has no $env_domain; refusing to guess which parameter it means"
        ));
    }
    if !valid_domain_token(env_domain) {
        return Err(format!("'{env_domain}' is not a valid environment domain"));
    }
    Ok(template.replace("$env_domain", env_domain))
}

/// `-d` for `cassandra.sh`: `None` (autodetect) when no suffix is configured.
pub fn domain_arg(env_domain: &str, suffix: &str) -> Result<Option<String>, String> {
    if suffix.trim().is_empty() {
        return Ok(None);
    }
    let full = format!("{env_domain}{}", suffix.trim());
    if !valid_domain_token(&full) {
        return Err(format!("'{full}' is not a valid cert domain"));
    }
    Ok(Some(full))
}

/// `ssm describe-parameters` prints `LastModifiedDate` as ISO-8601 on CLI v2
/// and as epoch seconds (with a fraction) on v1.
pub fn parse_param_date(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Ok(d) = DateTime::parse_from_rfc3339(s) {
        return Some(d.timestamp());
    }
    s.parse::<f64>()
        .ok()
        .filter(|f| f.is_finite() && *f >= 1.0 && *f < 1e11)
        .map(|f| f.floor() as i64)
}

/// A parameter last changed before the current cert was issued has not been
/// renewed: running the update would reinstall the cert already there.
pub fn parameter_is_stale(modified: i64, current: &CertInfo) -> bool {
    modified <= current.not_before
}

/// Names of unselected nodes that do not serve `result`, `(unreadable)` for
/// the ones that could not be read.
pub fn stale_unselected(result: &CertInfo, others: &[(String, Option<CertInfo>)]) -> Vec<String> {
    stale_against(&OldCert::from(result), others)
}

/// Names of nodes in `others` that do not serve `want`, `(unreadable)` for
/// the ones that could not be read.
pub fn stale_against(want: &OldCert, others: &[(String, Option<CertInfo>)]) -> Vec<String> {
    others
        .iter()
        .filter_map(|(name, cert)| match cert {
            None => Some(format!("{name} (unreadable)")),
            Some(c) if !served_matches(c, want) => Some(name.clone()),
            Some(_) => None,
        })
        .collect()
}

/// One `__CC_PF_BACKUP__` line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackupFact {
    pub ts: String,
    pub path: String,
    pub not_after: i64,
    pub serial: Option<String>,
    pub opens: bool,
    pub readable: bool,
    pub perms_ok: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreflightRaw {
    /// Newest first, as the script lists them.
    pub backups: Vec<BackupFact>,
    pub space_ok: bool,
}

/// `None` unless both the BEGIN and END markers are present and every
/// `__CC_PF_BACKUP__` line is well formed (7 fields, integer epoch, 0/1 flags):
/// a preflight cut short or with a dropped backup line must never read as
/// "no backups found" or let an older backup stand in for the newest.
pub fn parse_preflight(out: &str) -> Option<PreflightRaw> {
    if !out.contains("__CC_PF_BEGIN__") || !out.contains("__CC_PF_END__") {
        return None;
    }
    fn flag(s: &str) -> Option<bool> {
        match s {
            "1" => Some(true),
            "0" => Some(false),
            _ => None,
        }
    }
    let mut backups = Vec::new();
    let mut space_ok = false;
    for line in out.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("__CC_PF_BACKUP__ ") {
            let f: Vec<&str> = rest.split_whitespace().collect();
            if f.len() != 7 {
                return None;
            }
            backups.push(BackupFact {
                ts: f[0].to_string(),
                path: f[1].to_string(),
                not_after: f[2].parse().ok()?,
                serial: (f[3] != "-").then(|| f[3].to_ascii_uppercase()),
                opens: flag(f[4])?,
                readable: flag(f[5])?,
                perms_ok: flag(f[6])?,
            });
        } else if let Some(rest) = line.strip_prefix("__CC_PF_SPACE__ ") {
            space_ok = rest.trim() == "1";
        }
    }
    Some(PreflightRaw { backups, space_ok })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// A backup passes every check; restore it.
    Restorable { ts: String },
    /// No backup, but the node already serves the old cert.
    NothingToRollBack,
    /// Cannot be returned to the old cert. Blocks the whole rollback.
    Blocked(Vec<String>),
}

pub fn classify_preflight(
    raw: &PreflightRaw,
    served: Option<&CertInfo>,
    chosen_ts: Option<&str>,
    old: &OldCert,
) -> Verdict {
    if raw.backups.is_empty() {
        return match served {
            Some(c) if served_matches(c, old) => Verdict::NothingToRollBack,
            Some(_) => Verdict::Blocked(vec![
                "no backup on this node and it is serving the new cert".into(),
            ]),
            None => Verdict::Blocked(vec![
                "no backup on this node and the cert it serves could not be read".into(),
            ]),
        };
    }
    let chosen = match chosen_ts {
        Some(ts) => raw.backups.iter().find(|b| b.ts == ts),
        None => raw.backups.first(),
    };
    let Some(b) = chosen else {
        return Verdict::Blocked(vec![format!(
            "the chosen backup {} does not exist on this node",
            chosen_ts.unwrap_or("?")
        )]);
    };
    let mut why = Vec::new();
    if !b.readable {
        why.push(format!("backup {} is not readable", b.ts));
    }
    if !b.opens {
        why.push(format!(
            "keytool cannot open backup {} with the store password",
            b.ts
        ));
    }
    if !b.perms_ok {
        why.push(format!(
            "backup {} has a different owner/group from the live keystore",
            b.ts
        ));
    }
    if !raw.space_ok {
        why.push("not enough free space for the .rollback safety copy".into());
    }
    if why.is_empty() {
        Verdict::Restorable { ts: b.ts.clone() }
    } else {
        Verdict::Blocked(why)
    }
}

/// Enabled only when nothing is blocked and at least one node would change.
pub fn can_confirm_rollback(verdicts: &[Verdict]) -> bool {
    !verdicts.iter().any(|v| matches!(v, Verdict::Blocked(_)))
        && verdicts
            .iter()
            .any(|v| matches!(v, Verdict::Restorable { .. }))
}

/// True when the nodes to be restored would use different backup timestamps.
pub fn timestamps_disagree(verdicts: &[Verdict]) -> bool {
    let mut seen: Vec<&str> = Vec::new();
    for v in verdicts {
        if let Verdict::Restorable { ts } = v {
            if !seen.contains(&ts.as_str()) {
                seen.push(ts);
            }
        }
    }
    seen.len() > 1
}

/// The cert the environment is returning to, taken from what the nodes' newest
/// backups say: the most common expiry, ties going to the later one.
pub fn infer_old_cert(newest: &[&BackupFact]) -> Option<OldCert> {
    let mut counts: BTreeMap<i64, (usize, Option<String>)> = BTreeMap::new();
    for b in newest.iter().filter(|b| b.opens && b.not_after > 0) {
        let e = counts.entry(b.not_after).or_insert((0, b.serial.clone()));
        e.0 += 1;
    }
    counts
        .into_iter()
        .max_by(|a, b| a.1 .0.cmp(&b.1 .0).then(a.0.cmp(&b.0)))
        .map(|(not_after, (_, serial))| OldCert { not_after, serial })
}

/// `cassandra_check.sh` output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckRaw {
    pub cert: Option<CertInfo>,
    pub active: String,
}

pub fn parse_check(out: &str) -> Option<CheckRaw> {
    if !out.contains("__CC_BEGIN__") || !out.contains("__CC_END__") {
        return None;
    }
    let cert_text = out
        .split("__CC_CERT_BEGIN__")
        .nth(1)
        .and_then(|s| s.split("__CC_CERT_END__").next())
        .unwrap_or("");
    let active = out
        .lines()
        .find_map(|l| l.trim().strip_prefix("__CC_ACTIVE__ "))
        .unwrap_or("")
        .trim()
        .to_string();
    Some(CheckRaw {
        cert: parse_openssl(cert_text),
        active,
    })
}

/// The `__CC_RC__<n>` the app appends to every script invocation. Absent or
/// unparseable means the verdict is unknown, which callers treat as failure.
pub fn parse_rc(out: &str) -> Option<i32> {
    out.lines()
        .rev()
        .find_map(|l| l.trim().strip_prefix("__CC_RC__"))
        .and_then(|n| n.trim().parse().ok())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchState {
    Pending,
    Stable,
    Failed,
}

/// One node's post-restart watch. Pure: the caller supplies `now` (seconds
/// from any monotonic origin) and whether the node reported `active`.
#[derive(Clone, Debug)]
pub struct StabilityWatch {
    required: u64,
    ceiling: u64,
    started: Option<u64>,
    stable_since: Option<u64>,
    done: Option<WatchState>,
}

impl StabilityWatch {
    pub fn new(required_secs: u64, ceiling_secs: u64) -> Self {
        Self {
            required: required_secs,
            ceiling: ceiling_secs,
            started: None,
            stable_since: None,
            done: None,
        }
    }

    pub fn observe(&mut self, now: u64, active: bool) -> WatchState {
        if let Some(done) = self.done {
            return done;
        }
        let started = *self.started.get_or_insert(now);
        if active {
            let since = *self.stable_since.get_or_insert(now);
            if now.saturating_sub(since) >= self.required {
                self.done = Some(WatchState::Stable);
                return WatchState::Stable;
            }
        } else {
            self.stable_since = None;
        }
        if now.saturating_sub(started) >= self.ceiling {
            self.done = Some(WatchState::Failed);
            return WatchState::Failed;
        }
        WatchState::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Instance;

    fn inst(id: &str, name: &str, env: &str, state: &str) -> Instance {
        let mut i = Instance::new(id.to_string(), state.to_string());
        i.name = Some(name.to_string());
        i.env = Some(env.to_string());
        i
    }

    fn node(id: &str, number: u32) -> Node {
        Node {
            instance_id: id.into(),
            name: format!("cassandra-{number:03}"),
            number,
            running: true,
        }
    }

    #[test]
    fn a_node_number_is_read_from_the_name() {
        assert_eq!(parse_node_number("cassandra-001"), Some(1));
        assert_eq!(parse_node_number("cassandra-107"), Some(107));
        assert_eq!(parse_node_number("dev1-cassandra-203.dev1.example.net"), Some(203));
        assert_eq!(parse_node_number("CASSANDRA-042"), Some(42));
    }

    #[test]
    fn names_that_are_not_nodes_are_refused() {
        // Reaper boxes share the prefix and must never be offered.
        assert_eq!(parse_node_number("cassandra-reaper"), None);
        assert_eq!(parse_node_number("cassandra-reaper-01"), None);
        assert_eq!(parse_node_number("cassandra-"), None);
        assert_eq!(parse_node_number("cassandra-12345"), None, "five digits is not a node");
        assert_eq!(parse_node_number("kafka-001"), None);
        assert_eq!(parse_node_number(""), None);
    }

    #[test]
    fn nodes_are_filtered_by_environment_and_sorted_by_number() {
        let all = vec![
            inst("i-3", "cassandra-103", "DEV1", "running"),
            inst("i-1", "cassandra-001", "dev1", "running"),
            inst("i-x", "cassandra-002", "DEV2", "running"),
            inst("i-r", "cassandra-reaper", "DEV1", "running"),
            inst("i-2", "cassandra-002", "DEV1", "stopped"),
        ];
        let nodes = nodes_from_instances(&all, "DEV1");
        let ids: Vec<&str> = nodes.iter().map(|n| n.instance_id.as_str()).collect();
        assert_eq!(ids, ["i-1", "i-2", "i-3"]);
        assert!(!nodes[1].running, "a stopped node is listed but not running");
    }

    #[test]
    fn sets_group_by_the_hundreds_digit() {
        assert_eq!(node("a", 1).set(), 0);
        assert_eq!(node("a", 99).set(), 0);
        assert_eq!(node("a", 100).set(), 1);
        assert_eq!(node("a", 107).set(), 1);
        assert_eq!(node("a", 203).set(), 2);
        assert_eq!(set_label(0), "Set 0xx");
        assert_eq!(set_label(2), "Set 2xx");
    }

    #[test]
    fn only_running_nodes_are_selectable() {
        let mut stopped = node("i-2", 2);
        stopped.running = false;
        let all = vec![node("i-1", 1), stopped, node("i-3", 3)];
        assert_eq!(selectable_ids(&all), ["i-1", "i-3"]);
    }

    #[test]
    fn one_set_or_one_node_raises_nothing() {
        let all = vec![node("i-1", 1), node("i-2", 2), node("i-3", 3)];
        let a = assess_selection(&all, &["i-1".into(), "i-3".into()]);
        assert!(!a.needs_confirmation());
        assert_eq!(a.dominant_set, Some(0));
        // A single-set environment selecting everything is not "both clusters".
        let a = assess_selection(&all, &["i-1".into(), "i-2".into(), "i-3".into()]);
        assert!(!a.needs_confirmation());
        assert_eq!(a.all_sets, None);
        // Nothing selected.
        assert!(!assess_selection(&all, &[]).needs_confirmation());
    }

    fn two_sets() -> Vec<Node> {
        vec![
            node("i-1", 1),
            node("i-2", 2),
            node("i-3", 3),
            node("i-101", 101),
            node("i-102", 102),
            node("i-103", 103),
        ]
    }

    #[test]
    fn nodes_outside_the_dominant_set_are_flagged() {
        let a = assess_selection(&two_sets(), &["i-1".into(), "i-2".into(), "i-102".into()]);
        assert_eq!(a.dominant_set, Some(0));
        assert_eq!(a.outside, ["i-102"]);
        assert!(a.needs_confirmation());
        assert_eq!(a.all_sets, None);
    }

    #[test]
    fn a_tie_goes_to_the_lowest_set() {
        let a = assess_selection(&two_sets(), &["i-3".into(), "i-101".into()]);
        assert_eq!(a.dominant_set, Some(0));
        assert_eq!(a.outside, ["i-101"]);
    }

    #[test]
    fn all_of_one_set_plus_some_of_another_is_mixed() {
        let sel = ["i-1", "i-2", "i-3", "i-103"].map(String::from);
        let a = assess_selection(&two_sets(), &sel);
        assert_eq!(a.outside, ["i-103"]);
        assert_eq!(a.all_sets, None);
    }

    #[test]
    fn selecting_every_node_of_several_sets_asks_about_both_clusters() {
        let sel: Vec<String> = two_sets().iter().map(|n| n.instance_id.clone()).collect();
        let a = assess_selection(&two_sets(), &sel);
        assert_eq!(a.all_sets, Some(2));
        assert!(a.outside.is_empty(), "the one confirmation replaces per-node flags");
        assert!(a.needs_confirmation());
    }

    #[test]
    fn select_all_ignores_stopped_nodes() {
        let mut all = two_sets();
        all[4].running = false; // i-102 stopped
        let sel: Vec<String> = selectable_ids(&all);
        let a = assess_selection(&all, &sel);
        assert_eq!(a.all_sets, Some(2));
        assert!(a.outside.is_empty());
        assert!(a.needs_confirmation());
    }

    #[test]
    fn a_tie_between_sets_one_and_two_goes_to_one() {
        let all = vec![node("i-101", 101), node("i-201", 201), node("i-202", 202)];
        let a = assess_selection(&all, &["i-101".into(), "i-201".into()]);
        assert_eq!(a.dominant_set, Some(1));
        assert_eq!(a.outside, ["i-201"]);
    }

    const SCREENSHOT: &str = "subject= /CN=*.dev1.example.net\n\
        issuer= /C=US/O=Example/ST=X/CN=ca.example.net/L=Y\n\
        notBefore=Sep  3 07:01:23 2025 GMT\n\
        notAfter=Oct  3 08:01:22 2026 GMT\n\
        serial=0AB1C2\n";

    #[test]
    fn the_screenshot_output_parses_including_padded_days() {
        let c = parse_openssl(SCREENSHOT).expect("parses");
        assert_eq!(c.subject, "/CN=*.dev1.example.net");
        assert_eq!(c.serial.as_deref(), Some("0AB1C2"));
        // `Sep  3 07:01:23 2025 GMT` has a space-padded day.
        assert_eq!(c.not_before, 1_756_882_883);
        // `Oct  3 08:01:22 2026 GMT`
        assert_eq!(c.not_after, 1_791_014_482);
    }

    #[test]
    fn unparseable_output_is_none_not_a_guess() {
        assert!(parse_openssl("").is_none());
        assert!(parse_openssl("unable to load certificate").is_none());
        assert!(parse_openssl("notBefore=garbage\nnotAfter=garbage\n").is_none());
        // No notAfter at all.
        assert!(parse_openssl("subject= /CN=x\nissuer= /CN=y\n").is_none());
    }

    #[test]
    fn output_missing_any_of_the_four_fields_is_none() {
        assert!(parse_openssl("notBefore=Sep  3 07:01:23 2025 GMT\nnotAfter=Oct  3 08:01:22 2026 GMT\n").is_none());
        let no_issuer = "subject= /CN=x\nnotBefore=Sep  3 07:01:23 2025 GMT\nnotAfter=Oct  3 08:01:22 2026 GMT\n";
        assert!(parse_openssl(no_issuer).is_none());
        let no_before = "subject= /CN=x\nissuer= /CN=y\nnotAfter=Oct  3 08:01:22 2026 GMT\n";
        assert!(parse_openssl(no_before).is_none());
    }

    #[test]
    fn domain_tokens_must_start_alphanumeric() {
        for bad in ["-x", ".dev1", ""] {
            assert!(!valid_domain_token(bad), "{bad}");
        }
        for ok in ["dev1", "dev1.net", "a-b.c"] {
            assert!(valid_domain_token(ok), "{ok}");
        }
    }

    #[test]
    fn expiry_is_judged_against_now() {
        let c = parse_openssl(SCREENSHOT).unwrap();
        assert!(is_expired(&c, c.not_after));
        assert!(is_expired(&c, c.not_after + 1));
        assert!(!is_expired(&c, c.not_after - 1));
    }

    fn cert(subject: &str, issuer: &str, after: i64, serial: &str) -> CertInfo {
        CertInfo {
            subject: subject.into(),
            issuer: issuer.into(),
            not_before: after - 1000,
            not_after: after,
            serial: Some(serial.into()),
        }
    }

    #[test]
    fn a_renewal_needs_a_later_expiry_and_the_same_shape() {
        let before = cert("/CN=*.a", "/CN=ca", 1000, "01");
        assert_eq!(compare_certs(&before, &cert("/CN=*.a", "/CN=ca", 2000, "02")), CertChange::Renewed);
        assert_eq!(compare_certs(&before, &cert("/CN=*.a", "/CN=ca", 1000, "01")), CertChange::NotRenewed);
        assert_eq!(compare_certs(&before, &cert("/CN=*.a", "/CN=ca", 500, "00")), CertChange::NotRenewed);
    }

    #[test]
    fn a_new_date_with_a_different_subject_or_issuer_is_flagged() {
        // The 2026-10 incident: the date moved but the output did not look the same.
        let before = cert("/CN=*.a", "/CN=ca", 1000, "01");
        match compare_certs(&before, &cert("/CN=*.b", "/CN=other", 2000, "02")) {
            CertChange::Flagged(d) => {
                assert_eq!(d.len(), 2);
                assert!(d[0].contains("subject"));
                assert!(d[1].contains("issuer"));
            }
            other => panic!("expected Flagged, got {other:?}"),
        }
        // Whitespace differences alone are not a change.
        assert_eq!(
            compare_certs(&before, &cert("/CN=*.a  ", " /CN=ca", 2000, "02")),
            CertChange::Renewed
        );
    }

    #[test]
    fn a_served_cert_matches_the_old_one_by_date_and_serial() {
        let served = cert("/CN=x", "/CN=y", 1000, "AB");
        assert!(served_matches(&served, &OldCert { not_after: 1000, serial: Some("AB".into()) }));
        assert!(served_matches(&served, &OldCert { not_after: 1000, serial: None }), "no serial to compare");
        assert!(!served_matches(&served, &OldCert { not_after: 1000, serial: Some("CD".into()) }));
        assert!(!served_matches(&served, &OldCert { not_after: 999, serial: None }));
    }

    #[test]
    fn the_domain_is_the_lowercased_environment_name() {
        assert_eq!(env_domain(" DEV1 "), "dev1");
    }

    #[test]
    fn a_parameter_template_must_name_the_domain() {
        assert_eq!(
            expand_parameter("/certs/$env_domain/key", "dev1").unwrap(),
            "/certs/dev1/key"
        );
        assert!(expand_parameter("/certs/static/key", "dev1").is_err(), "no $env_domain");
        assert!(expand_parameter("/certs/$env_domain/key", "dev1; rm -rf /").is_err());
        assert!(expand_parameter("/certs/$env_domain/key", "").is_err());
    }

    #[test]
    fn the_cassandra_domain_flag_is_optional() {
        assert_eq!(domain_arg("dev1", "").unwrap(), None);
        assert_eq!(domain_arg("dev1", ".net").unwrap(), Some("dev1.net".to_string()));
        assert!(domain_arg("dev1", "; x").is_err());
    }

    #[test]
    fn parameter_dates_parse_from_either_cli_format() {
        // CLI v2: ISO-8601, fractional seconds dropped.
        assert_eq!(parse_param_date("2026-10-03T08:01:22.123000+00:00"), Some(1_791_014_482));
        // A non-UTC offset is honoured.
        assert_eq!(parse_param_date("2026-10-03T03:01:22-05:00"), Some(1_791_014_482));
        // CLI v1: epoch seconds with a fraction.
        assert_eq!(parse_param_date("1791014482.123"), Some(1_791_014_482));
        assert_eq!(parse_param_date("not a date"), None);
        for bad in ["inf", "NaN", "1e30", "-5", "0"] {
            assert_eq!(parse_param_date(bad), None, "{bad}");
        }
        assert_eq!(parse_param_date(""), None);
    }

    #[test]
    fn a_parameter_older_than_the_current_cert_has_not_been_renewed() {
        let current = cert("/CN=x", "/CN=y", 5000, "01"); // not_before = 4000
        assert!(parameter_is_stale(3999, &current));
        assert!(parameter_is_stale(4000, &current));
        assert!(!parameter_is_stale(4001, &current));
    }

    #[test]
    fn unselected_nodes_still_on_another_cert_are_listed() {
        let result = cert("/CN=x", "/CN=y", 2000, "02");
        let same = cert("/CN=x", "/CN=y", 2000, "02");
        let old = cert("/CN=x", "/CN=y", 1000, "01");
        let others = vec![
            ("cassandra-101".to_string(), Some(old)),
            ("cassandra-102".to_string(), Some(same)),
            ("cassandra-103".to_string(), None),
        ];
        assert_eq!(
            stale_unselected(&result, &others),
            ["cassandra-101", "cassandra-103 (unreadable)"]
        );
    }

    #[test]
    fn stale_against_compares_with_an_old_cert_and_flags_unreadable() {
        let want = OldCert { not_after: 1000, serial: Some("01".into()) };
        let others = vec![
            ("a".to_string(), Some(cert("/CN=x", "/CN=y", 1000, "01"))),
            ("b".to_string(), Some(cert("/CN=x", "/CN=y", 2000, "02"))),
            ("c".to_string(), None),
        ];
        assert_eq!(stale_against(&want, &others), ["b", "c (unreadable)"]);
        assert!(stale_against(&want, &[]).is_empty());
    }

    fn backup(ts: &str, not_after: i64) -> BackupFact {
        BackupFact {
            ts: ts.into(),
            path: format!("/etc/cassandra/conf/cassandra-keystore.jks.bak.{ts}"),
            not_after,
            serial: Some("01".into()),
            opens: true,
            readable: true,
            perms_ok: true,
        }
    }

    const OLD: OldCert = OldCert { not_after: 1000, serial: None };

    #[test]
    fn the_rollback_check_output_parses() {
        let out = "__CC_PF_BEGIN__\n\
            __CC_PF_KEYSTORE__ /etc/cassandra/conf/cassandra-keystore.jks\n\
            __CC_PF_BACKUP__ 20260903070123 /k.bak.20260903070123 1790000000 0AB1 1 1 1\n\
            __CC_PF_BACKUP__ 20250903070123 /k.bak.20250903070123 0 - 0 1 0\n\
            __CC_PF_SPACE__ 1\n__CC_PF_END__\n";
        let raw = parse_preflight(out).expect("parses");
        assert_eq!(raw.backups.len(), 2);
        assert_eq!(raw.backups[0].ts, "20260903070123");
        assert_eq!(raw.backups[0].not_after, 1_790_000_000);
        assert_eq!(raw.backups[0].serial.as_deref(), Some("0AB1"));
        assert!(raw.backups[0].opens && raw.backups[0].perms_ok);
        assert!(!raw.backups[1].opens && !raw.backups[1].perms_ok);
        assert_eq!(raw.backups[1].serial, None, "- means no serial");
        assert!(raw.space_ok);
    }

    #[test]
    fn a_truncated_check_is_not_a_preflight() {
        assert!(parse_preflight("").is_none());
        assert!(parse_preflight("__CC_PF_BEGIN__\n__CC_PF_SPACE__ 1\n").is_none(), "no END marker");
    }

    #[test]
    fn a_clean_backup_is_restorable() {
        let raw = PreflightRaw { backups: vec![backup("20260903070123", 1000)], space_ok: true };
        assert_eq!(
            classify_preflight(&raw, None, None, &OLD),
            Verdict::Restorable { ts: "20260903070123".into() }
        );
    }

    #[test]
    fn the_chosen_backup_is_the_one_that_is_checked() {
        let mut bad = backup("20250101000000", 900);
        bad.opens = false;
        let raw = PreflightRaw {
            backups: vec![backup("20260903070123", 1000), bad],
            space_ok: true,
        };
        assert!(matches!(classify_preflight(&raw, None, None, &OLD), Verdict::Restorable { .. }));
        match classify_preflight(&raw, None, Some("20250101000000"), &OLD) {
            Verdict::Blocked(why) => assert!(why[0].contains("keytool"), "{why:?}"),
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn every_failed_check_is_named() {
        let mut b = backup("20260903070123", 1000);
        b.readable = false;
        b.perms_ok = false;
        let raw = PreflightRaw { backups: vec![b], space_ok: false };
        match classify_preflight(&raw, None, None, &OLD) {
            Verdict::Blocked(why) => assert_eq!(why.len(), 3, "{why:?}"),
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn no_backup_but_already_on_the_old_cert_is_nothing_to_roll_back() {
        let raw = PreflightRaw { backups: vec![], space_ok: true };
        let served = cert("/CN=x", "/CN=y", 1000, "01");
        assert_eq!(classify_preflight(&raw, Some(&served), None, &OLD), Verdict::NothingToRollBack);
    }

    #[test]
    fn no_backup_on_the_new_cert_blocks() {
        let raw = PreflightRaw { backups: vec![], space_ok: true };
        let served = cert("/CN=x", "/CN=y", 2000, "02");
        assert!(matches!(
            classify_preflight(&raw, Some(&served), None, &OLD),
            Verdict::Blocked(_)
        ));
        // And if the served cert cannot be read at all, that is not "fine".
        assert!(matches!(classify_preflight(&raw, None, None, &OLD), Verdict::Blocked(_)));
    }

    #[test]
    fn only_a_blocked_node_stops_the_rollback() {
        let ok = Verdict::Restorable { ts: "1".into() };
        let skip = Verdict::NothingToRollBack;
        let bad = Verdict::Blocked(vec!["x".into()]);
        assert!(can_confirm_rollback(&[ok.clone(), skip.clone()]));
        assert!(!can_confirm_rollback(&[ok.clone(), bad]));
        assert!(!can_confirm_rollback(&[skip.clone()]), "nothing would be restored");
        assert!(!can_confirm_rollback(&[]));
        assert!(timestamps_disagree(&[
            Verdict::Restorable { ts: "1".into() },
            Verdict::Restorable { ts: "2".into() }
        ]));
        assert!(!timestamps_disagree(&[ok.clone(), ok, skip]));
    }

    #[test]
    fn the_old_cert_is_the_most_common_newest_backup() {
        let a = backup("2", 1000);
        let b = backup("2", 1000);
        let c = backup("2", 500);
        let old = infer_old_cert(&[&a, &b, &c]).expect("some");
        assert_eq!(old.not_after, 1000);
        assert!(infer_old_cert(&[]).is_none());
    }

    #[test]
    fn the_check_script_output_parses() {
        let out = format!(
            "__CC_BEGIN__\n__CC_HOST__ h\n__CC_CERT_BEGIN__\n{SCREENSHOT}__CC_CERT_END__\n__CC_ACTIVE__ active\n__CC_END__\n"
        );
        let c = parse_check(&out).expect("parses");
        assert!(c.cert.is_some());
        assert_eq!(c.active, "active");
        // A node not serving a cert still reports its service state.
        let none = "__CC_BEGIN__\n__CC_CERT_BEGIN__\nunable to load\n__CC_CERT_END__\n__CC_ACTIVE__ failed\n__CC_END__\n";
        let c = parse_check(none).expect("parses");
        assert!(c.cert.is_none());
        assert_eq!(c.active, "failed");
        assert!(parse_check("garbage").is_none());
    }

    #[test]
    fn a_missing_return_code_is_a_failure_not_a_success() {
        assert_eq!(parse_rc("INFO: done\n__CC_RC__0\n"), Some(0));
        assert_eq!(parse_rc("ERROR: nope\n__CC_RC__1\n"), Some(1));
        assert_eq!(parse_rc("INFO: still going"), None, "truncated output has no verdict");
        assert_eq!(parse_rc(""), None);
        assert_eq!(parse_rc("__CC_RC__abc"), None);
    }

    #[test]
    fn a_node_must_stay_active_for_the_whole_requirement() {
        let mut w = StabilityWatch::new(60, 300);
        assert_eq!(w.observe(0, false), WatchState::Pending);
        assert_eq!(w.observe(5, true), WatchState::Pending);
        assert_eq!(w.observe(64, true), WatchState::Pending, "59s is not 60s");
        assert_eq!(w.observe(65, true), WatchState::Stable);
        // Terminal: a later blip does not un-stabilise a node already judged.
        assert_eq!(w.observe(70, false), WatchState::Stable);
    }

    #[test]
    fn leaving_active_restarts_the_clock() {
        let mut w = StabilityWatch::new(60, 300);
        assert_eq!(w.observe(0, true), WatchState::Pending);
        assert_eq!(w.observe(50, false), WatchState::Pending, "flapped at 50s");
        assert_eq!(w.observe(55, true), WatchState::Pending);
        assert_eq!(w.observe(114, true), WatchState::Pending);
        assert_eq!(w.observe(115, true), WatchState::Stable);
    }

    #[test]
    fn a_node_that_never_gets_there_fails_at_the_ceiling() {
        let mut w = StabilityWatch::new(60, 300);
        assert_eq!(w.observe(0, false), WatchState::Pending);
        assert_eq!(w.observe(299, false), WatchState::Pending);
        assert_eq!(w.observe(300, false), WatchState::Failed);
        assert_eq!(w.observe(310, true), WatchState::Failed, "terminal");
    }

    #[test]
    fn reaching_the_requirement_at_the_ceiling_still_counts() {
        let mut w = StabilityWatch::new(60, 300);
        assert_eq!(w.observe(0, false), WatchState::Pending);
        assert_eq!(w.observe(240, true), WatchState::Pending);
        assert_eq!(w.observe(300, true), WatchState::Stable);
    }

    #[test]
    fn a_malformed_backup_line_fails_the_whole_preflight() {
        let wrap = |l: &str| format!("__CC_PF_BEGIN__\n{l}\n__CC_PF_SPACE__ 1\n__CC_PF_END__\n");
        let good = "__CC_PF_BACKUP__ 2 /k.bak.2 1790000000 0AB1 1 1 1";
        assert!(parse_preflight(&wrap(good)).is_some());
        assert!(parse_preflight(&wrap("__CC_PF_BACKUP__ 2 /a b/k.bak.2 1790000000 0AB1 1 1 1")).is_none(), "8 fields");
        assert!(parse_preflight(&wrap("__CC_PF_BACKUP__ 2 /k.bak.2 abc 0AB1 1 1 1")).is_none(), "bad epoch");
        assert!(parse_preflight(&wrap("__CC_PF_BACKUP__ 2 /k.bak.2 1790000000 0AB1 1 1 2")).is_none(), "bad flag");
        let unopenable = parse_preflight(&wrap("__CC_PF_BACKUP__ 1 /k.bak.1 0 - 0 1 0")).expect("valid");
        assert_eq!(unopenable.backups.len(), 1);
        let mixed = format!(
            "__CC_PF_BEGIN__\n{good}\n__CC_PF_BACKUP__ 1 /k.bak.1 xyz - 0 1 0\n__CC_PF_END__\n"
        );
        assert!(parse_preflight(&mixed).is_none(), "a bad older line still fails");
    }

    #[test]
    fn infer_old_cert_tie_goes_to_the_later_expiry() {
        let a = backup("2", 1000);
        let b = backup("2", 2000);
        assert_eq!(infer_old_cert(&[&a, &b]).expect("some").not_after, 2000);
    }

    #[test]
    fn a_chosen_backup_that_does_not_exist_is_blocked_by_name() {
        let raw = PreflightRaw { backups: vec![backup("20260903070123", 1000)], space_ok: true };
        match classify_preflight(&raw, None, Some("19990101000000"), &OLD) {
            Verdict::Blocked(why) => assert!(why[0].contains("19990101000000"), "{why:?}"),
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn the_last_return_code_marker_wins() {
        assert_eq!(parse_rc("__CC_RC__1\nmore\n__CC_RC__0\n"), Some(0));
    }

    #[test]
    fn no_free_space_does_not_block_when_nothing_would_be_written() {
        let raw = PreflightRaw { backups: vec![], space_ok: false };
        let served = cert("/CN=x", "/CN=y", 1000, "01");
        assert_eq!(classify_preflight(&raw, Some(&served), None, &OLD), Verdict::NothingToRollBack);
    }
}

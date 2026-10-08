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
}

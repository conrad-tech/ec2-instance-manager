//! Deciding where a discovered account's instances actually live.
//!
//! A discovered account already gets a *working* region: `resolve_region` is a
//! fallback chain that never fails. What it does not get is a *correct* one,
//! and nothing in the credentials file says -- `fed_role` is an IAM ARN, and
//! IAM is global.
//!
//! So the app does not ask. **The first inventory load is the probe** -- a
//! `describe-instances` it makes anyway -- and **an empty result is the
//! signal.** Only then is a search worth its calls, which is why the common
//! case (every account in one region) costs nothing.
//!
//! `describe-regions` is deliberately not the primary answer: it reports which
//! regions are *enabled*, around seventeen for a standard account, which does
//! not say where the instances are. Only `describe-instances` answers that.

use crate::models::Mode;

/// Whether an inventory load that has just landed is the signal to sweep.
///
/// **The gate is the mode, not the credentials.** Sim fakes `auth_status: Ok`,
/// and sim's whole promise is that it makes no real AWS calls -- the same rule
/// `power.rs` and the reaper follow.
///
/// `already_searched` is the record of a sweep that has already run for this
/// account. An account with genuinely zero instances is indistinguishable from
/// a wrong region, so without it an empty account costs a full seventeen-region
/// sweep every session, forever.
pub fn should_search(mode: Mode, instance_count: usize, already_searched: bool) -> bool {
    mode == Mode::Live && instance_count == 0 && !already_searched
}

/// The regions to probe, in the order to probe them.
///
/// `enabled` is what `describe-regions` reported; `preferred` is the distinct
/// regions the user's *other* accounts already resolve to. Preferred regions
/// come first, because on a single-region site that turns the search into one
/// call instead of seventeen -- but they only *order* the list, they never
/// shorten it, so an account somewhere unexpected is still found.
///
/// Two rules that are load-bearing:
///
/// - **A preferred region that is not enabled is dropped**, never invented. A
///   region the account cannot use answers nothing and costs a failed call.
/// - **An empty `enabled` list is a denied `describe-regions`, not an account
///   with no regions.** That is not a failure: the preferred list is then the
///   whole search, and it is almost certainly right.
pub fn search_order(enabled: &[String], preferred: &[String]) -> Vec<String> {
    let clean = |s: &String| {
        let t = s.trim();
        (!t.is_empty()).then(|| t.to_string())
    };

    // `describe-regions` was denied, so the regions other accounts use are all
    // we have to go on.
    if enabled.iter().filter_map(clean).next().is_none() {
        return dedup(preferred.iter().filter_map(clean));
    }

    let enabled: Vec<String> = dedup(enabled.iter().filter_map(clean));
    let is_enabled =
        |r: &str| enabled.iter().any(|e| e.eq_ignore_ascii_case(r));

    let first = preferred.iter().filter_map(clean).filter(|r| is_enabled(r));
    dedup(first.chain(enabled.iter().cloned()))
}

/// The region an account's instances actually live in, or `None`.
///
/// The app is single-region-per-account by design -- the region is part of the
/// inventory cache key -- so two populated regions is not an error. The busier
/// one is the answer, and a tie goes to whichever was searched first, which is
/// the preferred ordering [`search_order`] already applied.
///
/// **Nothing found anywhere is `None`, never a guess.** That is what lets the
/// caller record "searched" without writing a wrong region over a working one.
pub fn best_region(counts: &[(String, usize)]) -> Option<String> {
    // `Reverse` on the position because `max_by_key` returns the **last**
    // maximum, and the probe asks each region for at most one instance -- so
    // every count is 0 or 1 and a tie is the ordinary case, not the exception.
    counts
        .iter()
        .enumerate()
        .filter(|(_, (_, count))| *count > 0)
        .max_by_key(|(position, (_, count))| (*count, std::cmp::Reverse(*position)))
        .map(|(_, (region, _))| region.clone())
}

/// Order-preserving dedupe, case-insensitive because a region name typed into
/// config.ini by hand is free text.
fn dedup(items: impl Iterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in items {
        if !out.iter().any(|seen| seen.eq_ignore_ascii_case(&item)) {
            out.push(item);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Mode;

    /// An empty inventory in live mode is the signal, and the only one.
    #[test]
    fn an_empty_live_inventory_triggers_a_search() {
        assert!(should_search(Mode::Live, 0, false));
    }

    /// Instances found means the region was right. Nothing to do, no calls.
    #[test]
    fn a_populated_inventory_triggers_nothing() {
        assert!(!should_search(Mode::Live, 12, false));
    }

    /// An account with genuinely zero instances is indistinguishable from a
    /// wrong region, so the search must run once and never again -- otherwise
    /// an empty account costs a full region sweep every session, forever.
    #[test]
    fn a_search_already_run_never_runs_again() {
        assert!(!should_search(Mode::Live, 0, true));
    }

    /// Sim fakes auth_status Ok, so the gate is the mode. Sim's whole promise
    /// is that it makes no real AWS calls.
    #[test]
    fn sim_never_searches() {
        assert!(!should_search(Mode::Sim, 0, false));
    }

    /// Regions other accounts already use are tried first: on a single-region
    /// site that makes the search one call instead of seventeen.
    #[test]
    fn preferred_regions_are_searched_first() {
        let enabled = vec![
            "ap-south-1".to_string(),
            "us-east-1".to_string(),
            "eu-west-1".to_string(),
        ];
        let preferred = vec!["eu-west-1".to_string()];
        let order = search_order(&enabled, &preferred);
        assert_eq!(order.first().map(String::as_str), Some("eu-west-1"));
        assert_eq!(order.len(), 3, "every enabled region is still searched");
    }

    /// A preferred region that is not enabled is not invented.
    #[test]
    fn a_preferred_region_that_is_not_enabled_is_dropped() {
        let order = search_order(&["us-east-1".to_string()], &["mars-central-1".to_string()]);
        assert_eq!(order, vec!["us-east-1"]);
    }

    /// `describe-regions` can be denied. Then the regions other accounts use
    /// are all we have, and they are almost certainly right.
    #[test]
    fn with_no_enabled_list_the_preferred_regions_are_the_search() {
        let order = search_order(&[], &["us-east-1".to_string(), "eu-west-1".to_string()]);
        assert_eq!(order, vec!["us-east-1", "eu-west-1"]);
    }

    /// The app is single-region-per-account by design (region is in the
    /// inventory cache key), so two populated regions is not an error -- the
    /// busier one is the answer.
    #[test]
    fn the_region_with_the_most_instances_wins() {
        let counts = vec![
            ("us-east-1".to_string(), 2),
            ("eu-west-1".to_string(), 9),
        ];
        assert_eq!(best_region(&counts).as_deref(), Some("eu-west-1"));
    }

    /// The probe asks each region for at most one instance, so every count is
    /// 0 or 1 and a tie is the ordinary case -- it must go to whichever was
    /// searched first, which is the preferred ordering `search_order` already
    /// applied. `max_by_key` on its own returns the *last* maximum, which is
    /// the opposite.
    #[test]
    fn a_tie_goes_to_whichever_was_searched_first() {
        let counts = vec![
            ("eu-west-1".to_string(), 1),
            ("us-east-1".to_string(), 1),
        ];
        assert_eq!(best_region(&counts).as_deref(), Some("eu-west-1"));
    }

    /// Nothing anywhere means the account really is empty. Returning None is
    /// what lets the caller record "searched" without writing a wrong region.
    #[test]
    fn nothing_found_anywhere_is_no_answer() {
        let counts = vec![("us-east-1".to_string(), 0)];
        assert_eq!(best_region(&counts), None);
    }
}

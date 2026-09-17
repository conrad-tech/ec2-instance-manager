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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alerts::Alert;
    use crate::features::{InstanceAgeFeature, ReaperFeature, UnhealthyHostFeature};

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
}

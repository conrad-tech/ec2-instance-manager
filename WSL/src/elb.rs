//! Elastic Load Balancing v2: target groups (and, from phase 2, application
//! and network load balancers).
//!
//! Classic ELB (the `elb` API) is deliberately not covered — this site runs
//! ALB and NLB only, both of which are `elbv2`.
//!
//! Parsing is pure and tested against captured payloads; the `fetch_*`
//! functions are thin wrappers over `run_aws_cli`. That split is what keeps
//! the GUI binary free of resource models, and what lets these be tested
//! without AWS.

/// One target group, as the Target Groups sub-tab lists it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TargetGroup {
    pub arn: String,
    pub name: String,
    /// Absent for a `lambda` target group, which has neither.
    pub protocol: Option<String>,
    pub port: Option<u16>,
    /// `instance`, `ip`, `lambda` or `alb`.
    pub target_type: String,
    pub vpc_id: Option<String>,
    pub load_balancer_arns: Vec<String>,
    pub health_check: HealthCheck,
    /// Filled in by the caller from the AWS context, not by the API — the
    /// account is how two accounts' identically-named groups are told apart,
    /// and it is also which credentials the detail calls must use.
    pub account_id: String,
}

/// A target group's health-check configuration, as the detail view shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HealthCheck {
    pub enabled: bool,
    pub protocol: Option<String>,
    /// `traffic-port` is a legal value, so this is never a number.
    pub port: Option<String>,
    pub path: Option<String>,
    pub interval_secs: Option<u32>,
    pub timeout_secs: Option<u32>,
    pub healthy_threshold: Option<u32>,
    pub unhealthy_threshold: Option<u32>,
    /// `Matcher.HttpCode`, or `Matcher.GrpcCode` for a gRPC target group.
    pub matcher: Option<String>,
}

/// Read `elbv2 describe-target-groups --output json`.
///
/// An unreadable payload yields no groups rather than an error: this fills one
/// table in one sub-tab, and "None found" is recoverable where a panic is not.
/// The caller distinguishes an empty account from a failed call by whether the
/// *fetch* returned `Err`, never by an empty list.
pub fn parse_target_groups(raw: &str) -> Vec<TargetGroup> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    let Some(entries) = value.get("TargetGroups").and_then(|g| g.as_array()) else {
        return Vec::new();
    };

    let mut out: Vec<TargetGroup> = entries
        .iter()
        .map(|g| TargetGroup {
            arn: str_field(g, "TargetGroupArn").unwrap_or_default(),
            name: str_field(g, "TargetGroupName").unwrap_or_default(),
            protocol: str_field(g, "Protocol"),
            port: g.get("Port").and_then(|p| p.as_u64()).map(|p| p as u16),
            target_type: str_field(g, "TargetType").unwrap_or_default(),
            vpc_id: str_field(g, "VpcId"),
            load_balancer_arns: g
                .get("LoadBalancerArns")
                .and_then(|a| a.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            health_check: HealthCheck {
                enabled: g
                    .get("HealthCheckEnabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                protocol: str_field(g, "HealthCheckProtocol"),
                port: str_field(g, "HealthCheckPort"),
                path: str_field(g, "HealthCheckPath"),
                interval_secs: u32_field(g, "HealthCheckIntervalSeconds"),
                timeout_secs: u32_field(g, "HealthCheckTimeoutSeconds"),
                healthy_threshold: u32_field(g, "HealthyThresholdCount"),
                unhealthy_threshold: u32_field(g, "UnhealthyThresholdCount"),
                // A gRPC target group matches on GrpcCode instead; one field
                // holds whichever the group actually uses.
                matcher: g.get("Matcher").and_then(|m| {
                    m.get("HttpCode")
                        .or_else(|| m.get("GrpcCode"))
                        .and_then(|c| c.as_str())
                        .map(str::to_string)
                }),
            },
            account_id: String::new(),
        })
        .collect();

    // The API's order is not stable between calls, and a table that
    // reshuffles between visits is hard to read.
    out.sort_by(|a, b| {
        a.name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
            .then_with(|| a.arn.cmp(&b.arn))
    });
    out
}

fn str_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn u32_field(value: &serde_json::Value, key: &str) -> Option<u32> {
    value.get(key).and_then(|v| v.as_u64()).map(|v| v as u32)
}

/// `HTTP:80`, or an em dash for a lambda target group, which has neither.
///
/// An em dash rather than `-`: a lambda group genuinely has no protocol, and a
/// hyphen in that column reads as a parse failure.
pub fn protocol_port_label(tg: &TargetGroup) -> String {
    match (tg.protocol.as_deref(), tg.port) {
        (Some(proto), Some(port)) => format!("{proto}:{port}"),
        (Some(proto), None) => proto.to_string(),
        _ => "—".to_string(),
    }
}

/// The whole haystack the search box filters this row on.
///
/// **Every column the table shows must appear here.** A visible column that is
/// not searchable reads as "search is broken" — the exact bug the EC2 Private
/// DNS column had before it was added to `filter::searchable_text`.
pub fn target_group_searchable_text(tg: &TargetGroup) -> String {
    let mut out = String::new();
    for field in [
        tg.name.as_str(),
        tg.arn.as_str(),
        tg.target_type.as_str(),
        tg.account_id.as_str(),
    ] {
        out.push_str(&field.to_ascii_lowercase());
        out.push('\n');
    }
    out.push_str(&protocol_port_label(tg).to_ascii_lowercase());
    out.push('\n');
    if let Some(vpc) = &tg.vpc_id {
        out.push_str(&vpc.to_ascii_lowercase());
        out.push('\n');
    }
    out
}

/// One registered target, as the detail view's target table shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Target {
    pub id: String,
    pub port: Option<u16>,
    /// Absent for a lambda or an `ip` target outside a zone.
    pub az: Option<String>,
    /// `healthy`, `unhealthy`, `initial`, `draining`, `unavailable` or
    /// `unused`.
    pub state: String,
    pub reason: Option<String>,
    pub description: Option<String>,
}

/// The Healthy/Total column's value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HealthSummary {
    pub healthy: usize,
    pub total: usize,
}

/// Read `elbv2 describe-target-health --output json`.
pub fn parse_target_health(raw: &str) -> Vec<Target> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    let Some(entries) = value
        .get("TargetHealthDescriptions")
        .and_then(|d| d.as_array())
    else {
        return Vec::new();
    };

    entries
        .iter()
        .map(|d| {
            let target = d.get("Target");
            let health = d.get("TargetHealth");
            Target {
                id: target.and_then(|t| str_field(t, "Id")).unwrap_or_default(),
                port: target
                    .and_then(|t| t.get("Port"))
                    .and_then(|p| p.as_u64())
                    .map(|p| p as u16),
                az: target.and_then(|t| str_field(t, "AvailabilityZone")),
                state: health
                    .and_then(|h| str_field(h, "State"))
                    .unwrap_or_else(|| "unknown".to_string()),
                reason: health.and_then(|h| str_field(h, "Reason")),
                description: health.and_then(|h| str_field(h, "Description")),
            }
        })
        .collect()
}

/// Healthy over registered.
///
/// **Only `healthy` counts as healthy**, and the total is every registered
/// target whatever its state. A `draining` target is registered and is not
/// healthy; counting it as either would misreport a deploy in progress.
pub fn health_summary(targets: &[Target]) -> HealthSummary {
    HealthSummary {
        healthy: targets.iter().filter(|t| t.state == "healthy").count(),
        total: targets.len(),
    }
}

/// The cell's text. A group with nothing registered reads as an em dash rather
/// than `0/0` — having no targets is a different fact from having broken ones.
pub fn health_label(summary: HealthSummary) -> String {
    if summary.total == 0 {
        return "—".to_string();
    }
    format!("{}/{}", summary.healthy, summary.total)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real `describe-target-groups` payload: one ordinary HTTP group and
    /// one lambda group, which carries no protocol, no port and no VPC.
    const TARGET_GROUPS_JSON: &str = r#"{
      "TargetGroups": [
        {
          "TargetGroupArn": "arn:aws:elasticloadbalancing:us-east-1:111122223333:targetgroup/app-web/73e2d6bc24d8a067",
          "TargetGroupName": "app-web",
          "Protocol": "HTTP",
          "Port": 80,
          "VpcId": "vpc-3ac0fb5f",
          "HealthCheckProtocol": "HTTP",
          "HealthCheckPort": "traffic-port",
          "HealthCheckEnabled": true,
          "HealthCheckIntervalSeconds": 30,
          "HealthCheckTimeoutSeconds": 5,
          "HealthyThresholdCount": 5,
          "UnhealthyThresholdCount": 2,
          "HealthCheckPath": "/health",
          "Matcher": { "HttpCode": "200" },
          "LoadBalancerArns": [
            "arn:aws:elasticloadbalancing:us-east-1:111122223333:loadbalancer/app/my-alb/50dc6c495c0c9188"
          ],
          "TargetType": "instance",
          "ProtocolVersion": "HTTP1"
        },
        {
          "TargetGroupArn": "arn:aws:elasticloadbalancing:us-east-1:111122223333:targetgroup/lambda-fn/1234567890abcdef",
          "TargetGroupName": "lambda-fn",
          "HealthCheckEnabled": false,
          "LoadBalancerArns": [],
          "TargetType": "lambda"
        }
      ]
    }"#;

    #[test]
    fn parses_an_ordinary_target_group() {
        let groups = parse_target_groups(TARGET_GROUPS_JSON);
        assert_eq!(groups.len(), 2);
        let web = &groups[0];
        assert_eq!(web.name, "app-web");
        assert_eq!(web.protocol.as_deref(), Some("HTTP"));
        assert_eq!(web.port, Some(80));
        assert_eq!(web.target_type, "instance");
        assert_eq!(web.vpc_id.as_deref(), Some("vpc-3ac0fb5f"));
        assert_eq!(web.load_balancer_arns.len(), 1);
        assert_eq!(web.health_check.path.as_deref(), Some("/health"));
        assert_eq!(web.health_check.port.as_deref(), Some("traffic-port"));
        assert_eq!(web.health_check.interval_secs, Some(30));
        assert_eq!(web.health_check.matcher.as_deref(), Some("200"));
        assert!(web.health_check.enabled);
    }

    /// A lambda target group has no protocol, no port and no VPC. Defaulting
    /// those to a placeholder string would put "-" in a column that should
    /// read "—" and make the row look misparsed.
    #[test]
    fn a_lambda_target_group_has_no_protocol_port_or_vpc() {
        let groups = parse_target_groups(TARGET_GROUPS_JSON);
        let lambda = &groups[1];
        assert_eq!(lambda.name, "lambda-fn");
        assert_eq!(lambda.protocol, None);
        assert_eq!(lambda.port, None);
        assert_eq!(lambda.vpc_id, None);
        assert!(lambda.load_balancer_arns.is_empty());
        assert!(!lambda.health_check.enabled);
    }

    /// An unexpected payload yields no groups, never a panic. This is one
    /// section of a panel; "None found" is recoverable where a panic is not.
    #[test]
    fn an_unreadable_payload_yields_no_groups() {
        assert!(parse_target_groups("not json").is_empty());
        assert!(parse_target_groups("{}").is_empty());
        assert!(parse_target_groups(r#"{"TargetGroups": "nope"}"#).is_empty());
    }

    /// The API's order is not stable between calls, and a table that
    /// reshuffles between visits is hard to read.
    #[test]
    fn groups_come_back_sorted_by_name() {
        let raw = r#"{"TargetGroups":[
            {"TargetGroupArn":"arn:z","TargetGroupName":"zulu","TargetType":"instance"},
            {"TargetGroupArn":"arn:a","TargetGroupName":"Alpha","TargetType":"instance"}
        ]}"#;
        let names: Vec<String> = parse_target_groups(raw)
            .iter()
            .map(|g| g.name.clone())
            .collect();
        assert_eq!(names, vec!["Alpha".to_string(), "zulu".to_string()]);
    }

    #[test]
    fn the_protocol_port_label_reads_as_the_console_writes_it() {
        let groups = parse_target_groups(TARGET_GROUPS_JSON);
        assert_eq!(protocol_port_label(&groups[0]), "HTTP:80");
        // A lambda group has neither, and an em dash says so without looking
        // like a parse failure.
        assert_eq!(protocol_port_label(&groups[1]), "—");
    }

    /// The search box must find a group by name, by ARN, by VPC and by the
    /// protocol:port the table shows. A column visible in the table but
    /// missing here reads as "search is broken" — the exact bug the EC2
    /// Private DNS column had.
    #[test]
    fn every_column_in_the_table_is_searchable() {
        let groups = parse_target_groups(TARGET_GROUPS_JSON);
        let text = target_group_searchable_text(&groups[0]);
        for needle in [
            "app-web",
            "73e2d6bc24d8a067",
            "vpc-3ac0fb5f",
            "http:80",
            "instance",
        ] {
            assert!(text.contains(needle), "{needle} missing from {text}");
        }
    }

    /// Lower-cased, because `filter::text_matches` compares against a
    /// lower-cased query — the same contract `filter::searchable_text` keeps.
    #[test]
    fn searchable_text_is_lower_cased() {
        let raw = r#"{"TargetGroups":[{"TargetGroupArn":"arn:A","TargetGroupName":"APP-Web","TargetType":"instance"}]}"#;
        let groups = parse_target_groups(raw);
        let text = target_group_searchable_text(&groups[0]);
        assert!(text.contains("app-web"));
        assert!(!text.contains("APP-Web"));
    }

    /// A real `describe-target-health` payload: one healthy target, one
    /// unhealthy with a reason, and one draining.
    const TARGET_HEALTH_JSON: &str = r#"{
      "TargetHealthDescriptions": [
        {
          "Target": { "Id": "i-0f76fade", "Port": 80, "AvailabilityZone": "us-east-1a" },
          "HealthCheckPort": "80",
          "TargetHealth": { "State": "healthy" }
        },
        {
          "Target": { "Id": "i-0f76fadf", "Port": 80, "AvailabilityZone": "us-east-1b" },
          "HealthCheckPort": "80",
          "TargetHealth": {
            "State": "unhealthy",
            "Reason": "Target.ResponseCodeMismatch",
            "Description": "Health checks failed with these codes: [500]"
          }
        },
        {
          "Target": { "Id": "i-0f76fae0", "Port": 80, "AvailabilityZone": "us-east-1c" },
          "TargetHealth": {
            "State": "draining",
            "Reason": "Target.DeregistrationInProgress",
            "Description": "Target deregistration is in progress"
          }
        }
      ]
    }"#;

    #[test]
    fn parses_every_target_with_its_reason() {
        let targets = parse_target_health(TARGET_HEALTH_JSON);
        assert_eq!(targets.len(), 3);
        assert_eq!(targets[0].id, "i-0f76fade");
        assert_eq!(targets[0].state, "healthy");
        assert_eq!(targets[0].az.as_deref(), Some("us-east-1a"));
        assert_eq!(targets[1].state, "unhealthy");
        assert_eq!(
            targets[1].reason.as_deref(),
            Some("Target.ResponseCodeMismatch")
        );
        assert!(targets[1].description.as_deref().unwrap().contains("500"));
    }

    /// Only `healthy` counts as healthy, and the total is every registered
    /// target. A draining target is registered and is not healthy — counting
    /// it either way would misreport a deploy in progress.
    #[test]
    fn the_summary_counts_only_healthy_against_every_registered_target() {
        let targets = parse_target_health(TARGET_HEALTH_JSON);
        let summary = health_summary(&targets);
        assert_eq!(summary.healthy, 1);
        assert_eq!(summary.total, 3);
    }

    /// A target group with nothing registered is not "0 of 0 healthy" — it has
    /// no targets, which is a different thing from having broken ones.
    #[test]
    fn a_group_with_no_targets_is_a_dash_not_a_zero() {
        let empty = parse_target_health(r#"{"TargetHealthDescriptions":[]}"#);
        assert!(empty.is_empty());
        assert_eq!(health_label(health_summary(&empty)), "—");
    }

    #[test]
    fn the_health_label_reads_as_the_console_writes_it() {
        assert_eq!(health_label(HealthSummary { healthy: 3, total: 3 }), "3/3");
        assert_eq!(health_label(HealthSummary { healthy: 0, total: 2 }), "0/2");
    }

    #[test]
    fn an_unreadable_health_payload_yields_no_targets() {
        assert!(parse_target_health("not json").is_empty());
        assert!(parse_target_health("{}").is_empty());
    }
}

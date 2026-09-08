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

use crate::aws_cli::run_aws_cli;
use crate::error::AppError;

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
        // Words, not a dash. This was an em dash, and an em dash in a column
        // whose other states are a blank cell and an ellipsis reads as "we
        // never got an answer" — which is the opposite of what it means. It
        // was reported as the health column failing to load.
        return "no targets".to_string();
    }
    format!("{}/{}", summary.healthy, summary.total)
}

/// Why a fetch did not produce data.
///
/// `Denied` is separate from `Failed` because the caller acts on it: the
/// Healthy/Total column switches itself off for an account on the first
/// denial, rather than issuing one refused call per row for as long as
/// somebody keeps scrolling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchError {
    Denied,
    Failed(String),
}

impl FetchError {
    /// Classify a failed CLI invocation.
    ///
    /// `Absence::Absent` has no meaning for a list call — nothing here answers
    /// "nothing configured" — so it is kept as a failure rather than silently
    /// becoming an empty list, which would render as "no target groups in this
    /// account".
    pub fn from_stderr(stderr: &str) -> Self {
        match crate::resources::classify_absent(stderr) {
            crate::resources::Absence::Denied => Self::Denied,
            crate::resources::Absence::Absent | crate::resources::Absence::Failed(_) => {
                Self::Failed(stderr.trim().to_string())
            }
        }
    }

    /// The sentence to put on screen.
    pub fn message(&self) -> String {
        match self {
            Self::Denied => "not permitted".to_string(),
            Self::Failed(text) => text.clone(),
        }
    }
}

/// Map a `run_aws_cli` error onto a `FetchError`, keeping the API's own
/// explanation — `CommandFailed` carries stderr, which is the thing worth
/// reading.
fn fetch_error(err: AppError) -> FetchError {
    match err {
        AppError::CommandFailed { stderr, .. } => FetchError::from_stderr(&stderr),
        other => FetchError::Failed(other.to_string()),
    }
}

fn run(profile: &str, region: &str, args: &[&str]) -> std::result::Result<String, FetchError> {
    run_aws_cli(Some(profile), Some(region), args).map_err(fetch_error)
}

/// Every target group in one account and region.
///
/// `account_id` is stamped onto each row: it is how two accounts'
/// identically-named groups are told apart, and which credentials the detail
/// calls must use.
pub fn fetch_target_groups(
    profile: &str,
    region: &str,
    account_id: &str,
) -> std::result::Result<Vec<TargetGroup>, FetchError> {
    let raw = run(
        profile,
        region,
        &["elbv2", "describe-target-groups", "--output", "json"],
    )?;
    let mut groups = parse_target_groups(&raw);
    for group in &mut groups {
        group.account_id = account_id.to_string();
    }
    Ok(groups)
}

/// The registered targets of one group and their health.
///
/// **One group per call — the API has no bulk form.** That is the whole reason
/// the Healthy/Total column is filled lazily rather than up front.
pub fn fetch_target_health(
    profile: &str,
    region: &str,
    arn: &str,
) -> std::result::Result<Vec<Target>, FetchError> {
    let raw = run(
        profile,
        region,
        &[
            "elbv2",
            "describe-target-health",
            "--target-group-arn",
            arn,
            "--output",
            "json",
        ],
    )?;
    Ok(parse_target_health(&raw))
}

pub fn fetch_target_group_attributes(
    profile: &str,
    region: &str,
    arn: &str,
) -> std::result::Result<Vec<(String, String)>, FetchError> {
    let raw = run(
        profile,
        region,
        &[
            "elbv2",
            "describe-target-group-attributes",
            "--target-group-arn",
            arn,
            "--output",
            "json",
        ],
    )?;
    Ok(parse_target_group_attributes(&raw))
}

pub fn fetch_elb_tags(
    profile: &str,
    region: &str,
    arn: &str,
) -> std::result::Result<Vec<(String, String)>, FetchError> {
    let raw = run(
        profile,
        region,
        &[
            "elbv2",
            "describe-tags",
            "--resource-arns",
            arn,
            "--output",
            "json",
        ],
    )?;
    Ok(parse_elb_tags(&raw))
}

/// `describe-target-group-attributes` as sorted name/value pairs.
pub fn parse_target_group_attributes(raw: &str) -> Vec<(String, String)> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    let Some(entries) = value.get("Attributes").and_then(|a| a.as_array()) else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = entries
        .iter()
        .filter_map(|a| {
            Some((
                a.get("Key").and_then(|k| k.as_str())?.to_string(),
                a.get("Value")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            ))
        })
        .collect();
    out.sort();
    out
}

/// `describe-tags` for a single resource, as sorted key/value pairs.
///
/// The call is made one ARN at a time, so the first description is the only
/// one; taking it by position rather than matching the ARN back keeps this
/// working whether or not the API echoes it in the shape expected.
pub fn parse_elb_tags(raw: &str) -> Vec<(String, String)> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    let Some(tags) = value
        .get("TagDescriptions")
        .and_then(|d| d.as_array())
        .and_then(|d| d.first())
        .and_then(|d| d.get("Tags"))
        .and_then(|t| t.as_array())
    else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = tags
        .iter()
        .filter_map(|t| {
            Some((
                t.get("Key").and_then(|k| k.as_str())?.to_string(),
                t.get("Value")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            ))
        })
        .collect();
    out.sort();
    out
}

/// One application or network load balancer, as the Load Balancers sub-tab
/// lists it.
///
/// Classic ELB (the `elb` API) is a different service and is deliberately not
/// covered — see this module's header.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LoadBalancer {
    pub arn: String,
    pub name: String,
    pub dns_name: String,
    /// `application`, `network` or `gateway`, as the API spells it.
    pub kind: String,
    /// `internet-facing` or `internal`.
    pub scheme: Option<String>,
    /// `State.Code`: `active`, `provisioning`, `active_impaired` or `failed`.
    pub state: String,
    pub vpc_id: Option<String>,
    pub created: Option<String>,
    pub ip_address_type: Option<String>,
    /// Zone name and subnet, one per entry.
    pub zones: Vec<(String, String)>,
    /// Absent on a network load balancer, which historically has none — an
    /// empty list and "this kind does not have them" are different facts.
    pub security_groups: Vec<String>,
    /// Stamped in by the caller from the AWS context, as `TargetGroup`'s is.
    pub account_id: String,
}

/// One listener on a load balancer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Listener {
    pub arn: String,
    pub protocol: Option<String>,
    pub port: Option<u16>,
    /// What the listener does with a request no rule matched — usually
    /// `forward -> <target group name>`, sometimes a redirect or a fixed
    /// response.
    pub default_action: String,
    pub certificate_count: usize,
    pub ssl_policy: Option<String>,
}

/// One rule on a listener, in priority order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ListenerRule {
    /// `default` for the catch-all, otherwise a number as a string — the API
    /// reports it that way and sorting is done on the parsed value.
    pub priority: String,
    pub conditions: Vec<String>,
    pub action: String,
}

/// Read `elbv2 describe-load-balancers --output json`.
///
/// Empty on unreadable input, for the reason `parse_target_groups` is: this
/// fills one table, and the caller tells an empty account from a failed call
/// by whether the *fetch* returned `Err`.
pub fn parse_load_balancers(raw: &str) -> Vec<LoadBalancer> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    let Some(entries) = value.get("LoadBalancers").and_then(|v| v.as_array()) else {
        return Vec::new();
    };

    let mut out: Vec<LoadBalancer> = entries
        .iter()
        .map(|lb| LoadBalancer {
            arn: str_field(lb, "LoadBalancerArn").unwrap_or_default(),
            name: str_field(lb, "LoadBalancerName").unwrap_or_default(),
            dns_name: str_field(lb, "DNSName").unwrap_or_default(),
            kind: str_field(lb, "Type").unwrap_or_default(),
            scheme: str_field(lb, "Scheme"),
            state: lb
                .get("State")
                .and_then(|s| str_field(s, "Code"))
                .unwrap_or_else(|| "unknown".to_string()),
            vpc_id: str_field(lb, "VpcId"),
            created: str_field(lb, "CreatedTime"),
            ip_address_type: str_field(lb, "IpAddressType"),
            zones: lb
                .get("AvailabilityZones")
                .and_then(|z| z.as_array())
                .map(|zones| {
                    zones
                        .iter()
                        .map(|z| {
                            (
                                str_field(z, "ZoneName").unwrap_or_default(),
                                str_field(z, "SubnetId").unwrap_or_default(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
            security_groups: lb
                .get("SecurityGroups")
                .and_then(|g| g.as_array())
                .map(|g| g.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default(),
            account_id: String::new(),
        })
        .collect();

    // The API's order is not stable between calls.
    out.sort_by(|a, b| {
        a.name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
            .then_with(|| a.arn.cmp(&b.arn))
    });
    out
}

/// `ALB` / `NLB` / `GWLB`, or the API's own word if it ever grows a fourth.
///
/// The table has no room for `application`, and nobody says it out loud
/// either. The raw value stays searchable — see `load_balancer_searchable_text`
/// — so typing `application` still finds it.
pub fn load_balancer_kind_label(lb: &LoadBalancer) -> String {
    match lb.kind.as_str() {
        "application" => "ALB".to_string(),
        "network" => "NLB".to_string(),
        "gateway" => "GWLB".to_string(),
        "" => "—".to_string(),
        other => other.to_string(),
    }
}

/// The whole haystack the search box filters a load balancer row on.
///
/// **Every column the table shows must appear here**, plus the fields the
/// table dropped — the account, the VPC and the API's own spelling of the
/// type — so a search still reaches them even though no column does.
pub fn load_balancer_searchable_text(lb: &LoadBalancer) -> String {
    let mut out = String::new();
    for field in [
        lb.name.as_str(),
        lb.arn.as_str(),
        lb.dns_name.as_str(),
        lb.kind.as_str(),
        lb.state.as_str(),
        lb.account_id.as_str(),
    ] {
        out.push_str(&field.to_ascii_lowercase());
        out.push('\n');
    }
    out.push_str(&load_balancer_kind_label(lb).to_ascii_lowercase());
    out.push('\n');
    for v in [lb.scheme.as_deref(), lb.vpc_id.as_deref()]
        .into_iter()
        .flatten()
    {
        out.push_str(&v.to_ascii_lowercase());
        out.push('\n');
    }
    for (zone, subnet) in &lb.zones {
        out.push_str(&zone.to_ascii_lowercase());
        out.push('\n');
        out.push_str(&subnet.to_ascii_lowercase());
        out.push('\n');
    }
    out
}

/// Read `elbv2 describe-listeners --output json`.
pub fn parse_listeners(raw: &str) -> Vec<Listener> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    let Some(entries) = value.get("Listeners").and_then(|v| v.as_array()) else {
        return Vec::new();
    };

    let mut out: Vec<Listener> = entries
        .iter()
        .map(|l| Listener {
            arn: str_field(l, "ListenerArn").unwrap_or_default(),
            protocol: str_field(l, "Protocol"),
            port: l.get("Port").and_then(|p| p.as_u64()).map(|p| p as u16),
            default_action: describe_actions(l.get("DefaultActions")),
            certificate_count: l
                .get("Certificates")
                .and_then(|c| c.as_array())
                .map(|c| c.len())
                .unwrap_or(0),
            ssl_policy: str_field(l, "SslPolicy"),
        })
        .collect();
    // By port, which is how anyone looking for one thinks of it.
    out.sort_by_key(|l| l.port.unwrap_or(u16::MAX));
    out
}

/// Read `elbv2 describe-rules --output json`, in priority order.
pub fn parse_listener_rules(raw: &str) -> Vec<ListenerRule> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    let Some(entries) = value.get("Rules").and_then(|v| v.as_array()) else {
        return Vec::new();
    };

    let mut out: Vec<ListenerRule> = entries
        .iter()
        .map(|r| ListenerRule {
            priority: str_field(r, "Priority").unwrap_or_else(|| "default".to_string()),
            conditions: describe_conditions(r.get("Conditions")),
            action: describe_actions(r.get("Actions")),
        })
        .collect();
    // Numeric where it is a number; the catch-all sorts last, which is where
    // it fires.
    out.sort_by_key(|r| r.priority.parse::<u32>().unwrap_or(u32::MAX));
    out
}

/// One human sentence for a rule's actions.
///
/// A forward names its target group by NAME, taken from the ARN's own path,
/// not the whole ARN: the ARN is 100-odd characters of which about ten carry
/// the answer, and the panel is read, not parsed.
fn describe_actions(actions: Option<&serde_json::Value>) -> String {
    let Some(actions) = actions.and_then(|a| a.as_array()) else {
        return "—".to_string();
    };
    let parts: Vec<String> = actions
        .iter()
        .map(|a| {
            let kind = str_field(a, "Type").unwrap_or_else(|| "?".to_string());
            match kind.as_str() {
                "forward" => {
                    let names = forward_target_names(a);
                    if names.is_empty() {
                        "forward".to_string()
                    } else {
                        format!("forward -> {}", names.join(", "))
                    }
                }
                "redirect" => {
                    let code = a
                        .get("RedirectConfig")
                        .and_then(|c| str_field(c, "StatusCode"))
                        .unwrap_or_default();
                    format!("redirect {code}").trim_end().to_string()
                }
                "fixed-response" => {
                    let code = a
                        .get("FixedResponseConfig")
                        .and_then(|c| str_field(c, "StatusCode"))
                        .unwrap_or_default();
                    format!("fixed {code}").trim_end().to_string()
                }
                other => other.to_string(),
            }
        })
        .collect();
    if parts.is_empty() {
        "—".to_string()
    } else {
        parts.join("; ")
    }
}

/// The target group names a forward action points at.
///
/// Both shapes the API uses: a single `TargetGroupArn`, and the weighted
/// `ForwardConfig.TargetGroups` list. A rule written in the console produces
/// the first; one written by Terraform often produces the second, and reading
/// only one of them makes half the rules look actionless.
fn forward_target_names(action: &serde_json::Value) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    if let Some(arn) = str_field(action, "TargetGroupArn") {
        names.push(target_group_name_from_arn(&arn));
    }
    if let Some(groups) = action
        .get("ForwardConfig")
        .and_then(|c| c.get("TargetGroups"))
        .and_then(|g| g.as_array())
    {
        for g in groups {
            if let Some(arn) = str_field(g, "TargetGroupArn") {
                let name = target_group_name_from_arn(&arn);
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
    }
    names
}

/// `…:targetgroup/app-web/73e2d6bc` -> `app-web`.
///
/// Falls back to the whole string rather than to nothing: a shape this does
/// not recognise is better shown than swallowed.
fn target_group_name_from_arn(arn: &str) -> String {
    arn.split("targetgroup/")
        .nth(1)
        .and_then(|tail| tail.split('/').next())
        .unwrap_or(arn)
        .to_string()
}

/// One sentence per rule condition.
fn describe_conditions(conditions: Option<&serde_json::Value>) -> Vec<String> {
    let Some(conditions) = conditions.and_then(|c| c.as_array()) else {
        return Vec::new();
    };
    conditions
        .iter()
        .map(|c| {
            let field = str_field(c, "Field").unwrap_or_else(|| "?".to_string());
            // `Values` is the old spelling and the per-field config the new
            // one; a rule made in the console carries the config, so reading
            // only `Values` shows an empty condition on most modern rules.
            let mut values: Vec<String> = c
                .get("Values")
                .and_then(|v| v.as_array())
                .map(|v| v.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            for key in [
                "HostHeaderConfig",
                "PathPatternConfig",
                "HttpRequestMethodConfig",
                "SourceIpConfig",
            ] {
                if let Some(vs) = c.get(key).and_then(|k| k.get("Values")).and_then(|v| v.as_array()) {
                    for v in vs {
                        if let Some(s) = v.as_str() {
                            if !values.iter().any(|existing| existing == s) {
                                values.push(s.to_string());
                            }
                        }
                    }
                }
            }
            if values.is_empty() {
                field
            } else {
                format!("{field} = {}", values.join(", "))
            }
        })
        .collect()
}

/// Every load balancer in one account and region.
pub fn fetch_load_balancers(
    profile: &str,
    region: &str,
    account_id: &str,
) -> std::result::Result<Vec<LoadBalancer>, FetchError> {
    let raw = run(
        profile,
        region,
        &["elbv2", "describe-load-balancers", "--output", "json"],
    )?;
    let mut lbs = parse_load_balancers(&raw);
    for lb in &mut lbs {
        lb.account_id = account_id.to_string();
    }
    Ok(lbs)
}

/// One load balancer's listeners.
pub fn fetch_listeners(
    profile: &str,
    region: &str,
    lb_arn: &str,
) -> std::result::Result<Vec<Listener>, FetchError> {
    let raw = run(
        profile,
        region,
        &[
            "elbv2",
            "describe-listeners",
            "--load-balancer-arn",
            lb_arn,
            "--output",
            "json",
        ],
    )?;
    Ok(parse_listeners(&raw))
}

/// One listener's rules.
pub fn fetch_listener_rules(
    profile: &str,
    region: &str,
    listener_arn: &str,
) -> std::result::Result<Vec<ListenerRule>, FetchError> {
    let raw = run(
        profile,
        region,
        &[
            "elbv2",
            "describe-rules",
            "--listener-arn",
            listener_arn,
            "--output",
            "json",
        ],
    )?;
    Ok(parse_listener_rules(&raw))
}

/// A load balancer's attributes, as sorted name/value pairs.
///
/// Same reply shape as a target group's, so it shares the parser.
pub fn fetch_load_balancer_attributes(
    profile: &str,
    region: &str,
    lb_arn: &str,
) -> std::result::Result<Vec<(String, String)>, FetchError> {
    let raw = run(
        profile,
        region,
        &[
            "elbv2",
            "describe-load-balancer-attributes",
            "--load-balancer-arn",
            lb_arn,
            "--output",
            "json",
        ],
    )?;
    Ok(parse_target_group_attributes(&raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real `describe-load-balancers` payload: an internet-facing ALB with
    /// security groups, and an internal NLB with none.
    const LOAD_BALANCERS_JSON: &str = r#"{
      "LoadBalancers": [
        {
          "LoadBalancerArn": "arn:aws:elasticloadbalancing:us-east-1:111122223333:loadbalancer/net/zulu-nlb/abc123",
          "DNSName": "zulu-nlb-abc123.elb.us-east-1.amazonaws.com",
          "CreatedTime": "2026-02-01T09:00:00.000Z",
          "LoadBalancerName": "zulu-nlb",
          "Scheme": "internal",
          "VpcId": "vpc-3ac0fb5f",
          "State": { "Code": "provisioning" },
          "Type": "network",
          "AvailabilityZones": [ { "ZoneName": "us-east-1a", "SubnetId": "subnet-1" } ],
          "IpAddressType": "ipv4"
        },
        {
          "LoadBalancerArn": "arn:aws:elasticloadbalancing:us-east-1:111122223333:loadbalancer/app/alpha-alb/50dc6c49",
          "DNSName": "alpha-alb-50dc6c49.us-east-1.elb.amazonaws.com",
          "CreatedTime": "2026-01-15T10:30:00.000Z",
          "LoadBalancerName": "alpha-alb",
          "Scheme": "internet-facing",
          "VpcId": "vpc-3ac0fb5f",
          "State": { "Code": "active" },
          "Type": "application",
          "AvailabilityZones": [
            { "ZoneName": "us-east-1a", "SubnetId": "subnet-8360a9e7" },
            { "ZoneName": "us-east-1b", "SubnetId": "subnet-b7d581c0" }
          ],
          "SecurityGroups": [ "sg-5943793c" ],
          "IpAddressType": "ipv4"
        }
      ]
    }"#;

    #[test]
    fn parses_an_application_load_balancer() {
        let lbs = parse_load_balancers(LOAD_BALANCERS_JSON);
        assert_eq!(lbs.len(), 2);
        // Sorted by name, so the ALB comes first whatever order the API used.
        let alb = &lbs[0];
        assert_eq!(alb.name, "alpha-alb");
        assert_eq!(alb.kind, "application");
        assert_eq!(alb.scheme.as_deref(), Some("internet-facing"));
        assert_eq!(alb.state, "active");
        assert_eq!(alb.dns_name, "alpha-alb-50dc6c49.us-east-1.elb.amazonaws.com");
        assert_eq!(alb.zones.len(), 2);
        assert_eq!(alb.zones[0], ("us-east-1a".to_string(), "subnet-8360a9e7".to_string()));
        assert_eq!(alb.security_groups, vec!["sg-5943793c".to_string()]);
    }

    /// A network load balancer has no security groups, and `State.Code` is
    /// nested rather than a top-level string — reading it flat gives every NLB
    /// the same "unknown" state.
    #[test]
    fn parses_a_network_load_balancer_with_no_security_groups() {
        let lbs = parse_load_balancers(LOAD_BALANCERS_JSON);
        let nlb = &lbs[1];
        assert_eq!(nlb.name, "zulu-nlb");
        assert_eq!(nlb.kind, "network");
        assert_eq!(nlb.state, "provisioning");
        assert!(nlb.security_groups.is_empty());
    }

    #[test]
    fn the_kind_label_is_the_short_name_people_use() {
        let lbs = parse_load_balancers(LOAD_BALANCERS_JSON);
        assert_eq!(load_balancer_kind_label(&lbs[0]), "ALB");
        assert_eq!(load_balancer_kind_label(&lbs[1]), "NLB");
        // An unfamiliar type is shown as the API spelled it rather than hidden.
        let odd = LoadBalancer {
            kind: "something-new".to_string(),
            ..Default::default()
        };
        assert_eq!(load_balancer_kind_label(&odd), "something-new");
    }

    /// The table shows the short label, so `application` must still be findable
    /// — and so must the fields the table has no column for.
    #[test]
    fn a_load_balancer_is_searchable_by_what_the_table_hides() {
        let mut lbs = parse_load_balancers(LOAD_BALANCERS_JSON);
        lbs[0].account_id = "111122223333".to_string();
        let text = load_balancer_searchable_text(&lbs[0]);
        for needle in [
            "alpha-alb",
            "application",          // the API's word, though the column says ALB
            "alb",                  // and the label
            "internet-facing",
            "active",
            "vpc-3ac0fb5f",         // no column
            "111122223333",         // no column
            "subnet-8360a9e7",      // no column
            "us-east-1a",
        ] {
            assert!(text.contains(needle), "{needle} missing from {text}");
        }
        assert!(!text.contains("Alpha"), "must be lower-cased");
    }

    #[test]
    fn an_unreadable_load_balancer_payload_yields_none() {
        assert!(parse_load_balancers("not json").is_empty());
        assert!(parse_load_balancers("{}").is_empty());
        assert!(parse_load_balancers(r#"{"LoadBalancers": "nope"}"#).is_empty());
    }

    const LISTENERS_JSON: &str = r#"{
      "Listeners": [
        {
          "ListenerArn": "arn:...:listener/app/alpha-alb/50dc6c49/443",
          "Protocol": "HTTPS",
          "Port": 443,
          "SslPolicy": "ELBSecurityPolicy-2016-08",
          "Certificates": [ { "CertificateArn": "arn:acm:...", "IsDefault": true } ],
          "DefaultActions": [
            { "Type": "forward",
              "TargetGroupArn": "arn:aws:elasticloadbalancing:us-east-1:1111:targetgroup/app-web/73e2d6bc" }
          ]
        },
        {
          "ListenerArn": "arn:...:listener/app/alpha-alb/50dc6c49/80",
          "Protocol": "HTTP",
          "Port": 80,
          "DefaultActions": [
            { "Type": "redirect", "RedirectConfig": { "StatusCode": "HTTP_301", "Protocol": "HTTPS" } }
          ]
        }
      ]
    }"#;

    /// Listeners read in port order, and a forward names its target group by
    /// name — the ARN is a hundred characters of which ten carry the answer.
    #[test]
    fn parses_listeners_in_port_order_and_names_the_target_group() {
        let ls = parse_listeners(LISTENERS_JSON);
        assert_eq!(ls.len(), 2);
        assert_eq!(ls[0].port, Some(80));
        assert_eq!(ls[0].protocol.as_deref(), Some("HTTP"));
        assert_eq!(ls[0].default_action, "redirect HTTP_301");
        assert_eq!(ls[1].port, Some(443));
        assert_eq!(ls[1].default_action, "forward -> app-web");
        assert_eq!(ls[1].certificate_count, 1);
        assert_eq!(ls[1].ssl_policy.as_deref(), Some("ELBSecurityPolicy-2016-08"));
    }

    const RULES_JSON: &str = r#"{
      "Rules": [
        {
          "Priority": "default",
          "Conditions": [],
          "Actions": [ { "Type": "fixed-response",
                         "FixedResponseConfig": { "StatusCode": "404" } } ]
        },
        {
          "Priority": "10",
          "Conditions": [
            { "Field": "path-pattern", "PathPatternConfig": { "Values": [ "/api/*" ] } }
          ],
          "Actions": [
            { "Type": "forward",
              "ForwardConfig": { "TargetGroups": [
                { "TargetGroupArn": "arn:...:targetgroup/api-tg/aaa", "Weight": 1 }
              ] } }
          ]
        },
        {
          "Priority": "2",
          "Conditions": [ { "Field": "host-header", "Values": [ "old.example.com" ] } ],
          "Actions": [ { "Type": "redirect", "RedirectConfig": { "StatusCode": "HTTP_302" } } ]
        }
      ]
    }"#;

    /// Numeric priority order, and the catch-all sorts last because that is
    /// when it fires. Sorting these as strings puts "10" before "2".
    #[test]
    fn rules_sort_by_numeric_priority_with_the_default_last() {
        let rules = parse_listener_rules(RULES_JSON);
        let order: Vec<&str> = rules.iter().map(|r| r.priority.as_str()).collect();
        assert_eq!(order, vec!["2", "10", "default"]);
    }

    /// Both shapes of forward, and both shapes of condition. A rule written in
    /// the console carries the per-field config; one written by Terraform often
    /// carries the old flat `Values`. Reading only one makes half the rules
    /// look like they do nothing.
    #[test]
    fn rules_read_both_the_old_and_the_new_shapes() {
        let rules = parse_listener_rules(RULES_JSON);
        let host = rules.iter().find(|r| r.priority == "2").expect("rule 2");
        assert_eq!(host.conditions, vec!["host-header = old.example.com".to_string()]);
        assert_eq!(host.action, "redirect HTTP_302");

        let path = rules.iter().find(|r| r.priority == "10").expect("rule 10");
        assert_eq!(path.conditions, vec!["path-pattern = /api/*".to_string()]);
        assert_eq!(path.action, "forward -> api-tg");

        let default = rules.iter().find(|r| r.priority == "default").expect("default");
        assert!(default.conditions.is_empty());
        assert_eq!(default.action, "fixed 404");
    }

    #[test]
    fn unreadable_listener_and_rule_payloads_yield_nothing() {
        assert!(parse_listeners("not json").is_empty());
        assert!(parse_listeners("{}").is_empty());
        assert!(parse_listener_rules("not json").is_empty());
        assert!(parse_listener_rules("{}").is_empty());
    }

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
    ///
    /// It says so in **words**. This was an em dash, and in a column whose
    /// other states are a blank cell and an ellipsis, a dash reads as "no
    /// answer came back" — it was reported as the health column failing to
    /// load. The one thing this cell must never look like is an absent
    /// answer, because that is a different diagnosis entirely.
    #[test]
    fn a_group_with_no_targets_says_so_in_words() {
        let empty = parse_target_health(r#"{"TargetHealthDescriptions":[]}"#);
        assert!(empty.is_empty());
        let label = health_label(health_summary(&empty));
        assert_eq!(label, "no targets");
        // Not punctuation that could be mistaken for a missing value.
        assert!(!label.contains('—') && !label.contains('-'));
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

    #[test]
    fn parses_target_group_attributes_as_name_value_pairs() {
        let raw = r#"{"Attributes":[
            {"Key":"deregistration_delay.timeout_seconds","Value":"300"},
            {"Key":"stickiness.enabled","Value":"false"}
        ]}"#;
        let attrs = parse_target_group_attributes(raw);
        assert_eq!(attrs.len(), 2);
        // Sorted, so the panel does not reshuffle between visits.
        assert_eq!(attrs[0].0, "deregistration_delay.timeout_seconds");
        assert_eq!(attrs[0].1, "300");
    }

    #[test]
    fn parses_elb_tags() {
        let raw = r#"{"TagDescriptions":[{
            "ResourceArn":"arn:aws:elasticloadbalancing:us-east-1:1111:targetgroup/app-web/abc",
            "Tags":[{"Key":"Name","Value":"app-web"},{"Key":"Env","Value":"prod"}]
        }]}"#;
        let tags = parse_elb_tags(raw);
        assert_eq!(tags.len(), 2);
        assert_eq!(tags[0], ("Env".to_string(), "prod".to_string()));
    }

    #[test]
    fn unreadable_attribute_and_tag_payloads_yield_nothing() {
        assert!(parse_target_group_attributes("not json").is_empty());
        assert!(parse_elb_tags("{}").is_empty());
    }

    /// A denial must reach the caller as its own variant: the Healthy/Total
    /// column switches itself off for that account on the first one, rather
    /// than issuing a denied call per row for as long as somebody scrolls.
    #[test]
    fn a_denied_call_is_its_own_error() {
        let denied = FetchError::from_stderr(
            "An error occurred (AccessDeniedException) when calling the DescribeTargetHealth operation",
        );
        assert!(matches!(denied, FetchError::Denied));

        let other = FetchError::from_stderr("Could not connect to the endpoint URL");
        match other {
            FetchError::Failed(text) => assert!(text.contains("endpoint")),
            _ => panic!("expected Failed"),
        }
    }

    /// An `Absence::Absent` has no meaning for a list call — nothing here
    /// answers "not configured" — so it is reported as a failure with its own
    /// words rather than silently becoming an empty list, which would render
    /// as "no target groups in this account".
    #[test]
    fn an_absent_code_on_a_list_call_is_still_a_failure() {
        let err = FetchError::from_stderr(
            "An error occurred (NoSuchTagSet) when calling the DescribeTags operation",
        );
        assert!(matches!(err, FetchError::Failed(_)));
    }
}

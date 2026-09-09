//! EC2 Auto Scaling: the groups the ASG sub-tab lists, and their detail view.
//!
//! Parsing is pure and tested against captured payloads; the `fetch_*`
//! functions are thin wrappers over `resources::run_cli`. Same split as
//! `elb.rs`, and for the same reasons — the GUI binary stays free of resource
//! models, and every decision that can be *wrong* is settled by a test rather
//! than by reading a render loop.
//!
//! **`describe-auto-scaling-groups` answers almost everything in one call.**
//! The instances, their lifecycle and health, the target groups, the suspended
//! processes and the tags all arrive with the list — unlike target group
//! health, which is one call per group. So there is no lazy per-row fetch here
//! and no priority list: the Instances column is filled from the list itself,
//! for free.

pub use crate::resources::FetchError;

use crate::resources::run_cli as run;

/// One auto scaling group, as the ASG sub-tab lists it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AutoScalingGroup {
    pub arn: String,
    pub name: String,
    pub min_size: i64,
    pub max_size: i64,
    pub desired_capacity: i64,
    /// `EC2` or `ELB`.
    pub health_check_type: String,
    pub health_check_grace_secs: Option<i64>,
    pub default_cooldown_secs: Option<i64>,
    /// Where instances come from: a launch template (with its version), a
    /// mixed instances policy's template, or a legacy launch configuration.
    pub launch_source: LaunchSource,
    pub availability_zones: Vec<String>,
    /// `VPCZoneIdentifier`, which the API returns as ONE comma-separated
    /// string rather than a list.
    pub subnets: Vec<String>,
    pub target_group_arns: Vec<String>,
    /// Classic ELB names. This site runs none, but an ASG that has them and
    /// renders nothing looks like an ASG attached to nothing.
    pub load_balancer_names: Vec<String>,
    pub instances: Vec<AsgInstance>,
    pub suspended: Vec<SuspendedProcess>,
    pub termination_policies: Vec<String>,
    pub tags: Vec<AsgTag>,
    pub created: Option<String>,
    /// Present **only** while the group is being deleted. Empty is the normal
    /// state, not a missing value.
    pub status: Option<String>,
    pub service_linked_role_arn: Option<String>,
    /// `0` is the API's spelling of "no limit", which is not a lifetime of
    /// zero seconds. Kept raw; `max_instance_lifetime_label` reads it.
    pub max_instance_lifetime_secs: Option<i64>,
    pub capacity_rebalance: Option<bool>,
    pub new_instances_protected: Option<bool>,
    /// Stamped in by the caller from the AWS context, as `TargetGroup`'s and
    /// `LoadBalancer`'s are.
    pub account_id: String,
}

/// Where an auto scaling group gets its instances from.
///
/// Three shapes, and reading only the first is the trap: an ASG built with a
/// **mixed instances policy** carries no top-level `LaunchTemplate` at all, so
/// a parser that reads that key alone shows a blank for exactly the groups
/// most worth looking at. A launch *configuration* is the legacy third.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum LaunchSource {
    Template {
        name: String,
        version: Option<String>,
    },
    /// A mixed instances policy's template. Rendered distinctly from a plain
    /// template: the instance types actually launched come from the policy's
    /// overrides, not from the template, so calling it a plain template would
    /// misdescribe the group.
    MixedTemplate {
        name: String,
        version: Option<String>,
    },
    Configuration(String),
    /// Nothing the API offered was readable. Not the same as "no instances",
    /// and the `Default` for the same reason: a group whose source could not
    /// be read is exactly this, so there is nothing else it could sensibly be.
    #[default]
    Unknown,
}

impl LaunchSource {
    /// One line for the detail grid and the search haystack.
    pub fn label(&self) -> String {
        let with_version = |name: &str, version: &Option<String>| match version {
            Some(v) => format!("{name} (v{v})"),
            None => name.to_string(),
        };
        match self {
            Self::Template { name, version } => with_version(name, version),
            Self::MixedTemplate { name, version } => {
                format!("{} · mixed instances", with_version(name, version))
            }
            Self::Configuration(name) => format!("{name} (launch config)"),
            // Words, not a dash. A dash in a field whose other values are
            // names reads as a parse failure — which is exactly what it would
            // be hiding.
            Self::Unknown => "unknown".to_string(),
        }
    }

    /// The bare name, for the search haystack.
    fn name(&self) -> &str {
        match self {
            Self::Template { name, .. }
            | Self::MixedTemplate { name, .. }
            | Self::Configuration(name) => name,
            Self::Unknown => "",
        }
    }
}

/// One instance in an auto scaling group.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AsgInstance {
    pub instance_id: String,
    pub instance_type: Option<String>,
    pub availability_zone: Option<String>,
    /// `InService`, `Pending`, `Terminating`, `Standby`, …
    pub lifecycle_state: String,
    /// `Healthy` or `Unhealthy`.
    pub health_status: String,
    pub protected_from_scale_in: bool,
}

impl AsgInstance {
    /// Is this instance both in service and healthy?
    ///
    /// **Both halves are required.** An instance can be `InService` and
    /// `Unhealthy` — that is precisely a box about to be replaced — and an
    /// instance can be `Healthy` while still `Pending`, which is one that is
    /// not carrying traffic yet. Counting either as serving overstates the
    /// group in the direction that hides an outage.
    pub fn is_serving(&self) -> bool {
        self.lifecycle_state.eq_ignore_ascii_case("InService")
            && self.health_status.eq_ignore_ascii_case("Healthy")
    }
}

/// A scaling process the group has been told not to run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SuspendedProcess {
    pub name: String,
    pub reason: Option<String>,
}

/// One tag on the group.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AsgTag {
    pub key: String,
    pub value: String,
    pub propagate_at_launch: bool,
}

/// One scaling policy.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScalingPolicy {
    pub name: String,
    /// `TargetTrackingScaling`, `StepScaling`, `SimpleScaling`, …
    pub policy_type: String,
    /// One sentence saying what the policy actually does — see
    /// [`policy_summary`].
    pub summary: String,
    /// Absent on a reply that does not report it, which is not the same as
    /// disabled.
    pub enabled: Option<bool>,
}

/// One scheduled action.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScheduledAction {
    pub name: String,
    /// A cron expression for a recurring action; absent for a one-off.
    pub recurrence: Option<String>,
    pub time_zone: Option<String>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub min_size: Option<i64>,
    pub max_size: Option<i64>,
    pub desired_capacity: Option<i64>,
}

/// One entry from the group's scaling activity history.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScalingActivity {
    pub start_time: Option<String>,
    /// `Successful`, `Failed`, `Cancelled`, `InProgress`, …
    pub status_code: String,
    pub description: String,
    /// Why it happened. This is the field the section exists for — "an
    /// instance was terminated" is not an answer; "because a target tracking
    /// policy changed the desired capacity from 3 to 2" is.
    pub cause: String,
    pub status_message: Option<String>,
}

// ----------------------------------------------------------------- parsing

fn str_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

fn i64_field(value: &serde_json::Value, key: &str) -> Option<i64> {
    value.get(key).and_then(|v| v.as_i64())
}

fn str_list(value: &serde_json::Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Read `autoscaling describe-auto-scaling-groups --output json`.
///
/// An unreadable payload yields no groups rather than an error, for the reason
/// `parse_target_groups` does: this fills one table, and the caller tells an
/// empty account from a failed call by whether the *fetch* returned `Err`,
/// never by an empty list.
pub fn parse_auto_scaling_groups(raw: &str) -> Vec<AutoScalingGroup> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    let Some(entries) = value.get("AutoScalingGroups").and_then(|v| v.as_array()) else {
        return Vec::new();
    };

    let mut out: Vec<AutoScalingGroup> = entries
        .iter()
        .map(|g| AutoScalingGroup {
            arn: str_field(g, "AutoScalingGroupARN").unwrap_or_default(),
            name: str_field(g, "AutoScalingGroupName").unwrap_or_default(),
            min_size: i64_field(g, "MinSize").unwrap_or_default(),
            max_size: i64_field(g, "MaxSize").unwrap_or_default(),
            desired_capacity: i64_field(g, "DesiredCapacity").unwrap_or_default(),
            health_check_type: str_field(g, "HealthCheckType")
                .unwrap_or_else(|| "unknown".to_string()),
            health_check_grace_secs: i64_field(g, "HealthCheckGracePeriod"),
            default_cooldown_secs: i64_field(g, "DefaultCooldown"),
            launch_source: parse_launch_source(g),
            availability_zones: str_list(g, "AvailabilityZones"),
            subnets: parse_subnets(g),
            target_group_arns: str_list(g, "TargetGroupARNs"),
            load_balancer_names: str_list(g, "LoadBalancerNames"),
            instances: parse_asg_instances(g.get("Instances")),
            suspended: parse_suspended(g.get("SuspendedProcesses")),
            termination_policies: str_list(g, "TerminationPolicies"),
            tags: parse_asg_tags(g.get("Tags")),
            created: str_field(g, "CreatedTime"),
            status: str_field(g, "Status"),
            service_linked_role_arn: str_field(g, "ServiceLinkedRoleARN"),
            max_instance_lifetime_secs: i64_field(g, "MaxInstanceLifetime"),
            capacity_rebalance: g.get("CapacityRebalance").and_then(|v| v.as_bool()),
            new_instances_protected: g
                .get("NewInstancesProtectedFromScaleIn")
                .and_then(|v| v.as_bool()),
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

/// The three shapes an ASG names its instance source in, in the order the API
/// prefers them.
fn parse_launch_source(g: &serde_json::Value) -> LaunchSource {
    let read_spec = |spec: &serde_json::Value| {
        // `LaunchTemplateName` is what a human reads; the id is the fallback
        // for a spec carrying only that, which is legal.
        let name =
            str_field(spec, "LaunchTemplateName").or_else(|| str_field(spec, "LaunchTemplateId"))?;
        Some((name, str_field(spec, "Version")))
    };

    if let Some((name, version)) = g.get("LaunchTemplate").and_then(read_spec) {
        return LaunchSource::Template { name, version };
    }
    if let Some((name, version)) = g
        .get("MixedInstancesPolicy")
        .and_then(|p| p.get("LaunchTemplate"))
        .and_then(|t| t.get("LaunchTemplateSpecification"))
        .and_then(read_spec)
    {
        return LaunchSource::MixedTemplate { name, version };
    }
    if let Some(name) = str_field(g, "LaunchConfigurationName") {
        return LaunchSource::Configuration(name);
    }
    LaunchSource::Unknown
}

/// `VPCZoneIdentifier` is ONE comma-separated string, not a list.
///
/// Read as a list it yields nothing; rendered raw it puts a run-on
/// `subnet-a,subnet-b,subnet-c` in a cell. Split, trimmed, blanks dropped —
/// the API tolerates a trailing comma and has been seen emitting one.
fn parse_subnets(g: &serde_json::Value) -> Vec<String> {
    g.get("VPCZoneIdentifier")
        .and_then(|v| v.as_str())
        .map(|s| {
            s.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_asg_instances(value: Option<&serde_json::Value>) -> Vec<AsgInstance> {
    let Some(items) = value.and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out: Vec<AsgInstance> = items
        .iter()
        .map(|i| AsgInstance {
            instance_id: str_field(i, "InstanceId").unwrap_or_default(),
            instance_type: str_field(i, "InstanceType"),
            availability_zone: str_field(i, "AvailabilityZone"),
            lifecycle_state: str_field(i, "LifecycleState")
                .unwrap_or_else(|| "unknown".to_string()),
            health_status: str_field(i, "HealthStatus").unwrap_or_else(|| "unknown".to_string()),
            protected_from_scale_in: i
                .get("ProtectedFromScaleIn")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        })
        .collect();
    // By zone then id, so the same group reads the same way on every visit.
    out.sort_by(|a, b| {
        a.availability_zone
            .cmp(&b.availability_zone)
            .then_with(|| a.instance_id.cmp(&b.instance_id))
    });
    out
}

fn parse_suspended(value: Option<&serde_json::Value>) -> Vec<SuspendedProcess> {
    let Some(items) = value.and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out: Vec<SuspendedProcess> = items
        .iter()
        .filter_map(|p| {
            Some(SuspendedProcess {
                name: str_field(p, "ProcessName")?,
                reason: str_field(p, "SuspensionReason"),
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn parse_asg_tags(value: Option<&serde_json::Value>) -> Vec<AsgTag> {
    let Some(items) = value.and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out: Vec<AsgTag> = items
        .iter()
        .filter_map(|t| {
            Some(AsgTag {
                key: str_field(t, "Key")?,
                value: str_field(t, "Value").unwrap_or_default(),
                propagate_at_launch: t
                    .get("PropagateAtLaunch")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            })
        })
        .collect();
    out.sort_by(|a, b| a.key.to_ascii_lowercase().cmp(&b.key.to_ascii_lowercase()));
    out
}

/// Read `autoscaling describe-policies --output json`.
pub fn parse_scaling_policies(raw: &str) -> Vec<ScalingPolicy> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    let Some(items) = value.get("ScalingPolicies").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out: Vec<ScalingPolicy> = items
        .iter()
        .map(|p| ScalingPolicy {
            name: str_field(p, "PolicyName").unwrap_or_default(),
            policy_type: str_field(p, "PolicyType").unwrap_or_else(|| "unknown".to_string()),
            summary: policy_summary(p),
            enabled: p.get("Enabled").and_then(|v| v.as_bool()),
        })
        .collect();
    out.sort_by(|a, b| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()));
    out
}

/// What a scaling policy actually does, in one sentence.
///
/// The three policy types keep their settings in three unrelated sub-objects,
/// so there is no one field to render. A row showing only the policy type says
/// nothing a reader did not already know from its name.
fn policy_summary(p: &serde_json::Value) -> String {
    if let Some(tt) = p.get("TargetTrackingConfiguration") {
        let metric = tt
            .get("PredefinedMetricSpecification")
            .and_then(|m| str_field(m, "PredefinedMetricType"))
            .or_else(|| {
                tt.get("CustomizedMetricSpecification")
                    .and_then(|m| str_field(m, "MetricName"))
            })
            .unwrap_or_else(|| "custom metric".to_string());
        let target = tt
            .get("TargetValue")
            .and_then(|v| v.as_f64())
            .map(|v| format!("{v}"))
            .unwrap_or_else(|| "?".to_string());
        let mut out = format!("{metric} -> {target}");
        if tt.get("DisableScaleIn").and_then(|v| v.as_bool()) == Some(true) {
            out.push_str(" (scale-in disabled)");
        }
        return out;
    }

    let adjustment = str_field(p, "AdjustmentType");
    if let Some(steps) = p.get("StepAdjustments").and_then(|v| v.as_array()) {
        if !steps.is_empty() {
            return match adjustment {
                Some(kind) => format!("{} step(s), {kind}", steps.len()),
                None => format!("{} step(s)", steps.len()),
            };
        }
    }
    match (adjustment, i64_field(p, "ScalingAdjustment")) {
        // The sign is the whole meaning of a simple policy, so it is always
        // written: `+1` and `-1` are different policies and `1` is neither.
        (Some(kind), Some(n)) => format!("{kind} {n:+}"),
        (Some(kind), None) => kind,
        (None, Some(n)) => format!("{n:+}"),
        (None, None) => "—".to_string(),
    }
}

/// Read `autoscaling describe-scheduled-actions --output json`.
pub fn parse_scheduled_actions(raw: &str) -> Vec<ScheduledAction> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    let Some(items) = value
        .get("ScheduledUpdateGroupActions")
        .and_then(|v| v.as_array())
    else {
        return Vec::new();
    };
    let mut out: Vec<ScheduledAction> = items
        .iter()
        .map(|a| ScheduledAction {
            name: str_field(a, "ScheduledActionName").unwrap_or_default(),
            recurrence: str_field(a, "Recurrence"),
            time_zone: str_field(a, "TimeZone"),
            start_time: str_field(a, "StartTime"),
            end_time: str_field(a, "EndTime"),
            min_size: i64_field(a, "MinSize"),
            max_size: i64_field(a, "MaxSize"),
            desired_capacity: i64_field(a, "DesiredCapacity"),
        })
        .collect();
    out.sort_by(|a, b| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()));
    out
}

/// Read `autoscaling describe-scaling-activities --output json`.
///
/// **Left in the API's own order, which is newest first.** Sorting by the
/// parsed timestamp would be one more thing to get wrong for no gain: the API
/// already answers the question the section asks, which is what happened last.
pub fn parse_scaling_activities(raw: &str) -> Vec<ScalingActivity> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    let Some(items) = value.get("Activities").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    items
        .iter()
        .map(|a| ScalingActivity {
            start_time: str_field(a, "StartTime"),
            status_code: str_field(a, "StatusCode").unwrap_or_else(|| "unknown".to_string()),
            description: str_field(a, "Description").unwrap_or_default(),
            cause: str_field(a, "Cause").unwrap_or_default(),
            status_message: str_field(a, "StatusMessage"),
        })
        .collect()
}

// --------------------------------------------------------------- rendering

/// How many instances are serving, over how many the group holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InstanceSummary {
    pub serving: usize,
    pub total: usize,
}

/// Serving over held.
pub fn instance_summary(instances: &[AsgInstance]) -> InstanceSummary {
    InstanceSummary {
        serving: instances.iter().filter(|i| i.is_serving()).count(),
        total: instances.len(),
    }
}

/// The Instances cell's text.
///
/// A group holding nothing reads **`no instances`, in words** — not `0/0` and
/// not a dash. An ASG scaled deliberately to zero is an ordinary state, and a
/// dash in a column whose other values are numbers reads as "we never got an
/// answer", which is the opposite of what it means. Same lesson as the Target
/// Groups table's `no targets`.
pub fn instance_label(summary: InstanceSummary) -> String {
    if summary.total == 0 {
        return "no instances".to_string();
    }
    format!("{}/{}", summary.serving, summary.total)
}

/// `MaxInstanceLifetime` of `0` means **no limit**, not zero seconds.
///
/// Rendering the raw number puts `0s` next to a field whose other values are
/// days, which reads as instances being replaced instantly.
pub fn max_instance_lifetime_label(secs: Option<i64>) -> String {
    match secs {
        None | Some(0) => "no limit".to_string(),
        Some(s) => {
            let days = s / 86_400;
            if days > 0 {
                format!("{days} day(s)")
            } else {
                format!("{s}s")
            }
        }
    }
}

/// `…:targetgroup/app-web/73e2d6bc` -> `app-web`.
///
/// The ARN is a hundred characters of which about ten carry the answer, and
/// the panel is read rather than parsed. Falls back to the whole string: a
/// shape this does not recognise is better shown than swallowed.
pub fn target_group_name_from_arn(arn: &str) -> String {
    arn.split("targetgroup/")
        .nth(1)
        .and_then(|tail| tail.split('/').next())
        .unwrap_or(arn)
        .to_string()
}

/// The whole haystack the search box filters an ASG row on.
///
/// **Every column the table shows must appear here**, plus the fields the
/// table dropped — the account, the launch template, the zones and subnets,
/// the tags, and every **instance id** the group holds. That last one is the
/// question this haystack exists to answer that no column could: *which group
/// is `i-0abc` in?*
pub fn asg_searchable_text(g: &AutoScalingGroup) -> String {
    let mut out = String::new();
    let mut push = |s: &str| {
        out.push_str(&s.to_ascii_lowercase());
        out.push('\n');
    };
    push(&g.name);
    push(&g.arn);
    push(&g.account_id);
    push(&g.health_check_type);
    push(&g.launch_source.label());
    push(g.launch_source.name());
    push(&instance_label(instance_summary(&g.instances)));
    for n in [g.min_size, g.max_size, g.desired_capacity] {
        push(&n.to_string());
    }
    for zone in &g.availability_zones {
        push(zone);
    }
    for subnet in &g.subnets {
        push(subnet);
    }
    for arn in &g.target_group_arns {
        push(arn);
        push(&target_group_name_from_arn(arn));
    }
    for name in &g.load_balancer_names {
        push(name);
    }
    for instance in &g.instances {
        push(&instance.instance_id);
        push(&instance.lifecycle_state);
        push(&instance.health_status);
        if let Some(t) = &instance.instance_type {
            push(t);
        }
    }
    for process in &g.suspended {
        push(&process.name);
    }
    for tag in &g.tags {
        push(&tag.key);
        push(&tag.value);
    }
    if let Some(status) = &g.status {
        push(status);
    }
    out
}

// ---------------------------------------------------------------- fetching

/// Every auto scaling group in one account and region.
///
/// `account_id` is stamped onto each row: it is how two accounts' identically
/// named groups are told apart, and which credentials the detail calls must
/// use.
pub fn fetch_auto_scaling_groups(
    profile: &str,
    region: &str,
    account_id: &str,
) -> std::result::Result<Vec<AutoScalingGroup>, FetchError> {
    let raw = run(
        profile,
        region,
        &[
            "autoscaling",
            "describe-auto-scaling-groups",
            "--output",
            "json",
        ],
    )?;
    let mut groups = parse_auto_scaling_groups(&raw);
    for group in &mut groups {
        group.account_id = account_id.to_string();
    }
    Ok(groups)
}

/// One group's scaling policies.
pub fn fetch_scaling_policies(
    profile: &str,
    region: &str,
    group_name: &str,
) -> std::result::Result<Vec<ScalingPolicy>, FetchError> {
    let raw = run(
        profile,
        region,
        &[
            "autoscaling",
            "describe-policies",
            "--auto-scaling-group-name",
            group_name,
            "--output",
            "json",
        ],
    )?;
    Ok(parse_scaling_policies(&raw))
}

/// One group's scheduled actions.
pub fn fetch_scheduled_actions(
    profile: &str,
    region: &str,
    group_name: &str,
) -> std::result::Result<Vec<ScheduledAction>, FetchError> {
    let raw = run(
        profile,
        region,
        &[
            "autoscaling",
            "describe-scheduled-actions",
            "--auto-scaling-group-name",
            group_name,
            "--output",
            "json",
        ],
    )?;
    Ok(parse_scheduled_actions(&raw))
}

/// How many scaling activities the detail view reads.
///
/// The history goes back six weeks and a group that flaps has thousands of
/// entries; the section answers "what happened recently", so it is bounded at
/// the call rather than in the renderer — an unbounded read would page through
/// the lot to display twenty.
pub const ACTIVITY_LIMIT: usize = 20;

/// One group's recent scaling activity, newest first.
pub fn fetch_scaling_activities(
    profile: &str,
    region: &str,
    group_name: &str,
) -> std::result::Result<Vec<ScalingActivity>, FetchError> {
    let limit = ACTIVITY_LIMIT.to_string();
    let raw = run(
        profile,
        region,
        &[
            "autoscaling",
            "describe-scaling-activities",
            "--auto-scaling-group-name",
            group_name,
            // `--max-items` is the CLI's own paginator, so it stops the walk
            // rather than trimming a full result after the fact.
            "--max-items",
            &limit,
            "--output",
            "json",
        ],
    )?;
    Ok(parse_scaling_activities(&raw))
}


// ------------------------------------------------------------ capacity edit

/// A proposed min / desired / max for a group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapacityEdit {
    pub min: i64,
    pub desired: i64,
    pub max: i64,
}

/// Why a proposed capacity cannot be sent.
///
/// Every one of these is decided **locally, before the confirmation is even
/// enabled**, rather than by letting AWS reject the call. Two reasons, and the
/// second is the one that matters: a `ValidationError` arrives seconds later
/// from a subprocess, after the user has already agreed to something, and it
/// names the API's field rather than the box they typed in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CapacityProblem {
    /// The field named did not parse as a whole number.
    NotANumber(&'static str),
    Negative(&'static str),
    MinAboveMax,
    DesiredBelowMin,
    DesiredAboveMax,
    /// Nothing would change. Refused rather than sent: an update that changes
    /// nothing is still a write against a live group.
    NoChange,
}

impl CapacityProblem {
    /// The sentence to put under the boxes.
    pub fn message(&self) -> String {
        match self {
            Self::NotANumber(field) => format!("{field} must be a whole number"),
            Self::Negative(field) => format!("{field} cannot be negative"),
            Self::MinAboveMax => "min cannot be above max".to_string(),
            Self::DesiredBelowMin => "desired cannot be below min".to_string(),
            Self::DesiredAboveMax => "desired cannot be above max".to_string(),
            Self::NoChange => "these are already the group's values".to_string(),
        }
    }
}

/// Read the three boxes.
///
/// Blank is refused rather than defaulted: an empty Desired box silently
/// meaning zero is one keystroke away from scaling a group to nothing.
pub fn parse_capacity(
    min: &str,
    desired: &str,
    max: &str,
) -> std::result::Result<CapacityEdit, CapacityProblem> {
    let read = |raw: &str, field: &'static str| -> std::result::Result<i64, CapacityProblem> {
        let value: i64 = raw
            .trim()
            .parse()
            .map_err(|_| CapacityProblem::NotANumber(field))?;
        if value < 0 {
            return Err(CapacityProblem::Negative(field));
        }
        Ok(value)
    };
    Ok(CapacityEdit {
        min: read(min, "min")?,
        desired: read(desired, "desired")?,
        max: read(max, "max")?,
    })
}

/// What is wrong with this edit against that group, if anything.
///
/// The ordering is deliberate: the min/max relationship is reported before
/// desired's place inside it, because "min cannot be above max" is the fault
/// somebody actually made and "desired cannot be below min" would be a
/// confusing way of saying the same thing.
pub fn check_capacity(current: &AutoScalingGroup, edit: CapacityEdit) -> Option<CapacityProblem> {
    if edit.min > edit.max {
        return Some(CapacityProblem::MinAboveMax);
    }
    if edit.desired < edit.min {
        return Some(CapacityProblem::DesiredBelowMin);
    }
    if edit.desired > edit.max {
        return Some(CapacityProblem::DesiredAboveMax);
    }
    if edit.min == current.min_size
        && edit.desired == current.desired_capacity
        && edit.max == current.max_size
    {
        return Some(CapacityProblem::NoChange);
    }
    None
}

/// The fields this edit actually moves, as `desired 3 -> 5`.
///
/// Only the ones that differ. Restating the two that are unchanged buries the
/// one that is, and the confirmation exists to make that one impossible to
/// miss.
pub fn capacity_changes(current: &AutoScalingGroup, edit: CapacityEdit) -> Vec<String> {
    let mut out = Vec::new();
    for (label, from, to) in [
        ("min", current.min_size, edit.min),
        ("desired", current.desired_capacity, edit.desired),
        ("max", current.max_size, edit.max),
    ] {
        if from != to {
            out.push(format!("{label} {from} -> {to}"));
        }
    }
    out
}

/// What this edit does to the running instances.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapacityEffect {
    Launches(i64),
    /// The destructive direction, and the one the dialog must say out loud.
    Terminates(i64),
    /// Desired moved, but the group already holds that many — so nothing
    /// starts or stops.
    AlreadyThere,
}

impl CapacityEffect {
    pub fn message(&self) -> String {
        match self {
            Self::Launches(n) => format!("this launches {n} instance(s)"),
            Self::Terminates(n) => format!("this TERMINATES {n} instance(s)"),
            Self::AlreadyThere => {
                "the group already holds that many instances, so nothing starts or stops"
                    .to_string()
            }
        }
    }
}

/// What an edit will do to the running instances, or `None` when it moves
/// nothing.
///
/// Measured against the instances the group **actually holds**, not against
/// its current desired capacity: those two disagree exactly when a group is
/// mid-scale, and it is the real count AWS reconciles to.
///
/// `None` when desired is unchanged. A group that is already short of its
/// desired capacity is already launching instances, and reporting that as the
/// consequence of a min/max edit would blame this change for something it did
/// not cause.
pub fn capacity_effect(current: &AutoScalingGroup, edit: CapacityEdit) -> Option<CapacityEffect> {
    if edit.desired == current.desired_capacity {
        return None;
    }
    let held = current.instances.len() as i64;
    Some(match edit.desired.cmp(&held) {
        std::cmp::Ordering::Greater => CapacityEffect::Launches(edit.desired - held),
        std::cmp::Ordering::Less => CapacityEffect::Terminates(held - edit.desired),
        std::cmp::Ordering::Equal => CapacityEffect::AlreadyThere,
    })
}

/// Set a group's min, desired and max.
///
/// **The one write in this module**, and the only call here that changes
/// anything in AWS. Everything else is a describe.
///
/// All three values are always sent, even the ones that did not change.
/// Sending a subset hands the outcome to AWS's own coupling rules — raising
/// `MinSize` without naming a desired capacity silently raises the desired
/// capacity to match, and lowering `MaxSize` silently lowers it — so what
/// landed would not be what the confirmation showed. With all three named,
/// an inconsistent set is a hard `ValidationError` rather than a silent
/// change, and `check_capacity` has already refused it anyway.
pub fn set_capacity(
    profile: &str,
    region: &str,
    group_name: &str,
    edit: CapacityEdit,
) -> std::result::Result<(), FetchError> {
    let (min, desired, max) = (
        edit.min.to_string(),
        edit.desired.to_string(),
        edit.max.to_string(),
    );
    run(
        profile,
        region,
        &[
            "autoscaling",
            "update-auto-scaling-group",
            "--auto-scaling-group-name",
            group_name,
            "--min-size",
            &min,
            "--desired-capacity",
            &desired,
            "--max-size",
            &max,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;


    fn sized(min: i64, desired: i64, max: i64, held: usize) -> AutoScalingGroup {
        AutoScalingGroup {
            name: "app".to_string(),
            min_size: min,
            desired_capacity: desired,
            max_size: max,
            instances: (0..held)
                .map(|i| AsgInstance {
                    instance_id: format!("i-{i}"),
                    lifecycle_state: "InService".to_string(),
                    health_status: "Healthy".to_string(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    /// Blank is refused rather than defaulted. An empty Desired box silently
    /// meaning zero is one keystroke away from scaling a group to nothing.
    #[test]
    fn a_blank_or_junk_capacity_box_is_refused_by_name() {
        assert_eq!(
            parse_capacity("", "3", "8"),
            Err(CapacityProblem::NotANumber("min"))
        );
        assert_eq!(
            parse_capacity("2", "  ", "8"),
            Err(CapacityProblem::NotANumber("desired"))
        );
        assert_eq!(
            parse_capacity("2", "3", "eight"),
            Err(CapacityProblem::NotANumber("max"))
        );
        // The message names the box the user typed in, not the API's field.
        assert!(CapacityProblem::NotANumber("desired")
            .message()
            .contains("desired"));
    }

    /// Surrounding whitespace is ordinary in a typed box.
    #[test]
    fn a_capacity_box_tolerates_surrounding_space() {
        assert_eq!(
            parse_capacity(" 2 ", "3", "8 "),
            Ok(CapacityEdit {
                min: 2,
                desired: 3,
                max: 8
            })
        );
    }

    #[test]
    fn a_negative_capacity_is_refused_by_name() {
        assert_eq!(
            parse_capacity("-1", "3", "8"),
            Err(CapacityProblem::Negative("min"))
        );
    }

    /// Every inconsistent combination is refused locally, so the dialog says
    /// which box is wrong instead of a subprocess `ValidationError` arriving
    /// seconds after the user already agreed to it.
    #[test]
    fn an_inconsistent_capacity_is_refused_before_it_is_sent() {
        let g = sized(2, 3, 8, 3);
        assert_eq!(
            check_capacity(
                &g,
                CapacityEdit {
                    min: 9,
                    desired: 3,
                    max: 8
                }
            ),
            Some(CapacityProblem::MinAboveMax)
        );
        assert_eq!(
            check_capacity(
                &g,
                CapacityEdit {
                    min: 4,
                    desired: 3,
                    max: 8
                }
            ),
            Some(CapacityProblem::DesiredBelowMin)
        );
        assert_eq!(
            check_capacity(
                &g,
                CapacityEdit {
                    min: 2,
                    desired: 9,
                    max: 8
                }
            ),
            Some(CapacityProblem::DesiredAboveMax)
        );
    }

    /// "min cannot be above max" is the fault somebody actually made;
    /// reporting the same input as "desired cannot be below min" would be a
    /// confusing way of saying it. The ordering is what decides that.
    #[test]
    fn min_above_max_is_reported_ahead_of_desireds_place_inside_it() {
        let g = sized(2, 3, 8, 3);
        // Both are true of this input: min > max, AND desired > max.
        assert_eq!(
            check_capacity(
                &g,
                CapacityEdit {
                    min: 9,
                    desired: 9,
                    max: 1
                }
            ),
            Some(CapacityProblem::MinAboveMax)
        );
    }

    /// An update that changes nothing is still a write against a live group.
    #[test]
    fn an_edit_that_changes_nothing_is_refused() {
        let g = sized(2, 3, 8, 3);
        assert_eq!(
            check_capacity(
                &g,
                CapacityEdit {
                    min: 2,
                    desired: 3,
                    max: 8
                }
            ),
            Some(CapacityProblem::NoChange)
        );
    }

    #[test]
    fn a_legal_edit_passes() {
        let g = sized(2, 3, 8, 3);
        assert_eq!(
            check_capacity(
                &g,
                CapacityEdit {
                    min: 0,
                    desired: 5,
                    max: 10
                }
            ),
            None
        );
    }

    /// Only the fields that move are listed. Restating the unchanged two
    /// buries the one that changed, which is the whole point of the line.
    #[test]
    fn the_change_list_names_only_what_moves() {
        let g = sized(2, 3, 8, 3);
        assert_eq!(
            capacity_changes(
                &g,
                CapacityEdit {
                    min: 2,
                    desired: 5,
                    max: 10
                }
            ),
            vec!["desired 3 -> 5".to_string(), "max 8 -> 10".to_string()]
        );
    }

    /// The destructive direction has to be said out loud: "desired 5 -> 2"
    /// without "this terminates 3 instances" understates what is about to
    /// happen.
    #[test]
    fn scaling_down_says_it_terminates_instances() {
        let g = sized(0, 5, 10, 5);
        assert_eq!(
            capacity_effect(
                &g,
                CapacityEdit {
                    min: 0,
                    desired: 2,
                    max: 10
                }
            ),
            Some(CapacityEffect::Terminates(3))
        );
        assert!(CapacityEffect::Terminates(3)
            .message()
            .contains("TERMINATES"));
    }

    #[test]
    fn scaling_up_says_it_launches_instances() {
        let g = sized(0, 2, 10, 2);
        assert_eq!(
            capacity_effect(
                &g,
                CapacityEdit {
                    min: 0,
                    desired: 5,
                    max: 10
                }
            ),
            Some(CapacityEffect::Launches(3))
        );
    }

    /// Measured against what the group ACTUALLY holds, not against its
    /// current desired capacity. The two disagree exactly when a group is
    /// mid-scale, and the real count is what AWS reconciles to: a group
    /// desiring 5 but holding 2 needs three more, not none.
    #[test]
    fn the_effect_counts_the_instances_the_group_really_holds() {
        let mid_scale = sized(0, 5, 10, 2);
        assert_eq!(
            capacity_effect(
                &mid_scale,
                CapacityEdit {
                    min: 0,
                    desired: 6,
                    max: 10
                }
            ),
            Some(CapacityEffect::Launches(4))
        );
    }

    /// A min/max-only edit starts and stops nothing, and must not claim
    /// otherwise. A group already short of desired is already launching
    /// instances; blaming this edit for that would be a lie the confirmation
    /// tells.
    #[test]
    fn an_edit_that_leaves_desired_alone_reports_no_instance_effect() {
        let mid_scale = sized(0, 5, 10, 2);
        assert_eq!(
            capacity_effect(
                &mid_scale,
                CapacityEdit {
                    min: 1,
                    desired: 5,
                    max: 20
                }
            ),
            None
        );
    }

    /// Desired moved onto the count the group already holds: honest about
    /// there being nothing to do, rather than reporting a launch of zero.
    #[test]
    fn moving_desired_onto_the_held_count_starts_and_stops_nothing() {
        let g = sized(0, 5, 10, 3);
        assert_eq!(
            capacity_effect(
                &g,
                CapacityEdit {
                    min: 0,
                    desired: 3,
                    max: 10
                }
            ),
            Some(CapacityEffect::AlreadyThere)
        );
    }

    /// A real `describe-auto-scaling-groups` payload: a launch-template group
    /// attached to a target group with a mix of instance states, and a
    /// mixed-instances-policy group scaled to zero.
    const GROUPS_JSON: &str = r#"{
      "AutoScalingGroups": [
        {
          "AutoScalingGroupName": "zulu-workers",
          "AutoScalingGroupARN": "arn:aws:autoscaling:us-east-1:111122223333:autoScalingGroup:aaaa:autoScalingGroupName/zulu-workers",
          "MixedInstancesPolicy": {
            "LaunchTemplate": {
              "LaunchTemplateSpecification": {
                "LaunchTemplateId": "lt-0999",
                "LaunchTemplateName": "zulu-lt",
                "Version": "$Latest"
              },
              "Overrides": [ { "InstanceType": "m5.large" }, { "InstanceType": "m5a.large" } ]
            }
          },
          "MinSize": 0,
          "MaxSize": 6,
          "DesiredCapacity": 0,
          "DefaultCooldown": 300,
          "AvailabilityZones": [ "us-east-1a" ],
          "LoadBalancerNames": [],
          "TargetGroupARNs": [],
          "HealthCheckType": "EC2",
          "HealthCheckGracePeriod": 0,
          "Instances": [],
          "CreatedTime": "2026-03-02T08:00:00.000Z",
          "SuspendedProcesses": [
            { "ProcessName": "AZRebalance", "SuspensionReason": "User suspended at 2026-03-02" }
          ],
          "VPCZoneIdentifier": "subnet-aaa,subnet-bbb,",
          "TerminationPolicies": [ "OldestInstance" ],
          "NewInstancesProtectedFromScaleIn": false,
          "MaxInstanceLifetime": 0,
          "Tags": []
        },
        {
          "AutoScalingGroupName": "alpha-web",
          "AutoScalingGroupARN": "arn:aws:autoscaling:us-east-1:111122223333:autoScalingGroup:bbbb:autoScalingGroupName/alpha-web",
          "LaunchTemplate": { "LaunchTemplateId": "lt-0abc", "LaunchTemplateName": "alpha-lt", "Version": "3" },
          "MinSize": 2,
          "MaxSize": 8,
          "DesiredCapacity": 3,
          "DefaultCooldown": 120,
          "AvailabilityZones": [ "us-east-1a", "us-east-1b" ],
          "LoadBalancerNames": [],
          "TargetGroupARNs": [
            "arn:aws:elasticloadbalancing:us-east-1:111122223333:targetgroup/alpha-web-tg/73e2d6bc"
          ],
          "HealthCheckType": "ELB",
          "HealthCheckGracePeriod": 300,
          "Instances": [
            { "InstanceId": "i-0bbb", "InstanceType": "m5.large", "AvailabilityZone": "us-east-1b",
              "LifecycleState": "InService", "HealthStatus": "Unhealthy", "ProtectedFromScaleIn": false },
            { "InstanceId": "i-0aaa", "InstanceType": "m5.large", "AvailabilityZone": "us-east-1a",
              "LifecycleState": "InService", "HealthStatus": "Healthy", "ProtectedFromScaleIn": true },
            { "InstanceId": "i-0ccc", "InstanceType": "m5.large", "AvailabilityZone": "us-east-1a",
              "LifecycleState": "Pending", "HealthStatus": "Healthy", "ProtectedFromScaleIn": false }
          ],
          "CreatedTime": "2026-01-15T10:30:00.000Z",
          "SuspendedProcesses": [],
          "VPCZoneIdentifier": "subnet-111",
          "TerminationPolicies": [ "Default" ],
          "NewInstancesProtectedFromScaleIn": false,
          "ServiceLinkedRoleARN": "arn:aws:iam::111122223333:role/aws-service-role/autoscaling.amazonaws.com/AWSServiceRoleForAutoScaling",
          "MaxInstanceLifetime": 604800,
          "CapacityRebalance": true,
          "Tags": [
            { "Key": "MMODAL_ENV", "Value": "DEV1", "PropagateAtLaunch": true },
            { "Key": "Name", "Value": "alpha-web", "PropagateAtLaunch": true }
          ]
        }
      ]
    }"#;

    fn groups() -> Vec<AutoScalingGroup> {
        parse_auto_scaling_groups(GROUPS_JSON)
    }

    /// The API's order is not stable, so the list is sorted by name — which is
    /// also why `alpha-web` comes back first from a payload that listed it
    /// second.
    #[test]
    fn groups_are_sorted_by_name() {
        let gs = groups();
        assert_eq!(gs.len(), 2);
        assert_eq!(gs[0].name, "alpha-web");
        assert_eq!(gs[1].name, "zulu-workers");
    }

    #[test]
    fn a_launch_template_group_reads_its_whole_shape() {
        let g = &groups()[0];
        assert_eq!(
            g.launch_source,
            LaunchSource::Template {
                name: "alpha-lt".to_string(),
                version: Some("3".to_string()),
            }
        );
        assert_eq!(g.launch_source.label(), "alpha-lt (v3)");
        assert_eq!((g.min_size, g.desired_capacity, g.max_size), (2, 3, 8));
        assert_eq!(g.health_check_type, "ELB");
        assert_eq!(g.health_check_grace_secs, Some(300));
        assert_eq!(g.availability_zones, vec!["us-east-1a", "us-east-1b"]);
        assert_eq!(g.subnets, vec!["subnet-111"]);
        assert_eq!(g.capacity_rebalance, Some(true));
        assert_eq!(
            g.tags.iter().map(|t| t.key.as_str()).collect::<Vec<_>>(),
            vec!["MMODAL_ENV", "Name"]
        );
    }

    /// An ASG built with a mixed instances policy carries **no** top-level
    /// `LaunchTemplate`. A parser reading that key alone leaves this blank —
    /// on exactly the groups most worth looking at — so it must reach into the
    /// policy, and must say that is where it came from.
    #[test]
    fn a_mixed_instances_policy_still_names_its_template() {
        let g = &groups()[1];
        assert_eq!(
            g.launch_source,
            LaunchSource::MixedTemplate {
                name: "zulu-lt".to_string(),
                version: Some("$Latest".to_string()),
            }
        );
        assert!(
            g.launch_source.label().contains("mixed instances"),
            "the label must not pass a mixed policy off as a plain template: {}",
            g.launch_source.label()
        );
    }

    /// A spec carrying only an id is legal, and an id is better than a blank.
    #[test]
    fn a_template_with_no_name_falls_back_to_its_id() {
        let raw = r#"{"AutoScalingGroups":[{"AutoScalingGroupName":"x",
          "LaunchTemplate":{"LaunchTemplateId":"lt-042"}}]}"#;
        assert_eq!(
            parse_auto_scaling_groups(raw)[0].launch_source,
            LaunchSource::Template {
                name: "lt-042".to_string(),
                version: None,
            }
        );
    }

    /// The legacy third shape.
    #[test]
    fn a_launch_configuration_is_labelled_as_one() {
        let raw = r#"{"AutoScalingGroups":[{"AutoScalingGroupName":"x",
          "LaunchConfigurationName":"legacy-lc"}]}"#;
        let g = &parse_auto_scaling_groups(raw)[0];
        assert_eq!(
            g.launch_source,
            LaunchSource::Configuration("legacy-lc".to_string())
        );
        assert_eq!(g.launch_source.label(), "legacy-lc (launch config)");
    }

    /// Nothing readable is `Unknown` and says so in words — a dash in a field
    /// whose other values are names reads as a parse failure, which is exactly
    /// what it would be hiding.
    #[test]
    fn a_group_naming_no_source_says_unknown() {
        let raw = r#"{"AutoScalingGroups":[{"AutoScalingGroupName":"x"}]}"#;
        let g = &parse_auto_scaling_groups(raw)[0];
        assert_eq!(g.launch_source, LaunchSource::Unknown);
        assert_eq!(g.launch_source.label(), "unknown");
    }

    /// `VPCZoneIdentifier` is one comma-separated string, not a list, and the
    /// API has been seen emitting a trailing comma.
    #[test]
    fn the_subnet_field_is_split_not_rendered_raw() {
        assert_eq!(groups()[1].subnets, vec!["subnet-aaa", "subnet-bbb"]);
    }

    /// Serving requires **both** halves. `i-0bbb` is InService and Unhealthy —
    /// a box about to be replaced — and `i-0ccc` is Healthy but still Pending,
    /// so carrying no traffic. Counting either overstates the group in the
    /// direction that hides an outage.
    #[test]
    fn only_an_in_service_and_healthy_instance_counts_as_serving() {
        let g = &groups()[0];
        let summary = instance_summary(&g.instances);
        assert_eq!(
            summary,
            InstanceSummary {
                serving: 1,
                total: 3
            }
        );
        assert_eq!(instance_label(summary), "1/3");
    }

    /// A group scaled deliberately to zero is an ordinary state, and must not
    /// read like a column that failed to load.
    #[test]
    fn an_empty_group_says_no_instances_rather_than_zero_over_zero() {
        let label = instance_label(instance_summary(&groups()[1].instances));
        assert_eq!(label, "no instances");
        assert!(!label.contains('—') && !label.contains("0/0"));
    }

    /// Instances sort by zone then id, so a group reads the same on each visit.
    #[test]
    fn instances_are_ordered_stably() {
        let gs = groups();
        let ids: Vec<&str> = gs[0]
            .instances
            .iter()
            .map(|i| i.instance_id.as_str())
            .collect();
        assert_eq!(ids, vec!["i-0aaa", "i-0ccc", "i-0bbb"]);
    }

    /// `MaxInstanceLifetime: 0` is the API's "no limit". Rendered raw it says
    /// `0s`, which reads as instances being replaced instantly.
    #[test]
    fn a_zero_max_lifetime_means_no_limit() {
        assert_eq!(max_instance_lifetime_label(Some(0)), "no limit");
        assert_eq!(max_instance_lifetime_label(None), "no limit");
        assert_eq!(max_instance_lifetime_label(Some(604_800)), "7 day(s)");
        assert_eq!(max_instance_lifetime_label(Some(90)), "90s");
    }

    /// `Status` is set only while a group is being deleted, so an ordinary
    /// group must report `None` rather than an empty string that renders as a
    /// blank status field.
    #[test]
    fn an_ordinary_group_has_no_status() {
        assert_eq!(groups()[0].status, None);
    }

    /// Unreadable input yields no groups rather than an error: the caller
    /// tells an empty account from a failed call by whether the *fetch*
    /// returned `Err`.
    #[test]
    fn unreadable_input_yields_no_groups() {
        assert!(parse_auto_scaling_groups("not json").is_empty());
        assert!(parse_auto_scaling_groups(r#"{"Something":[]}"#).is_empty());
    }

    /// Every column the table shows must be searchable, and so must the
    /// instance ids — "which group is this box in?" is the question no column
    /// can answer.
    #[test]
    fn the_haystack_carries_every_column_and_every_instance_id() {
        let g = &groups()[0];
        let hay = asg_searchable_text(g);
        for needle in [
            "alpha-web",
            "elb",
            "alpha-lt",
            "1/3",
            "i-0aaa",
            "i-0bbb",
            "unhealthy",
            "m5.large",
            "us-east-1b",
            "subnet-111",
            "alpha-web-tg",
            "mmodal_env",
            "dev1",
            "111122223333",
        ] {
            assert!(hay.contains(needle), "haystack is missing {needle}: {hay}");
        }
    }

    /// The haystack is lower-cased once so the matcher does not have to be
    /// case-aware per field — `DEV1` in a tag has to be findable by typing
    /// `dev1`.
    #[test]
    fn the_haystack_is_case_folded() {
        assert_eq!(
            asg_searchable_text(&groups()[0]),
            asg_searchable_text(&groups()[0]).to_ascii_lowercase()
        );
    }

    /// A forward's target group is named, not ARN'd: the ARN is a hundred
    /// characters of which about ten carry the answer.
    #[test]
    fn a_target_group_arn_renders_as_its_name() {
        assert_eq!(
            target_group_name_from_arn(
                "arn:aws:elasticloadbalancing:us-east-1:111122223333:targetgroup/alpha-web-tg/73e2d6bc"
            ),
            "alpha-web-tg"
        );
        // An unrecognised shape is shown rather than swallowed.
        assert_eq!(target_group_name_from_arn("something-else"), "something-else");
    }

    const POLICIES_JSON: &str = r#"{
      "ScalingPolicies": [
        {
          "PolicyName": "target-cpu",
          "PolicyType": "TargetTrackingScaling",
          "Enabled": true,
          "TargetTrackingConfiguration": {
            "PredefinedMetricSpecification": { "PredefinedMetricType": "ASGAverageCPUUtilization" },
            "TargetValue": 60.0,
            "DisableScaleIn": true
          }
        },
        {
          "PolicyName": "add-one",
          "PolicyType": "SimpleScaling",
          "AdjustmentType": "ChangeInCapacity",
          "ScalingAdjustment": 1
        },
        {
          "PolicyName": "remove-one",
          "PolicyType": "SimpleScaling",
          "AdjustmentType": "ChangeInCapacity",
          "ScalingAdjustment": -1
        },
        {
          "PolicyName": "steps",
          "PolicyType": "StepScaling",
          "AdjustmentType": "PercentChangeInCapacity",
          "StepAdjustments": [
            { "MetricIntervalLowerBound": 0, "ScalingAdjustment": 10 },
            { "MetricIntervalLowerBound": 20, "ScalingAdjustment": 30 }
          ]
        }
      ]
    }"#;

    /// The three policy types keep their settings in three unrelated
    /// sub-objects, so a row showing only the type says nothing.
    #[test]
    fn each_policy_type_summarises_what_it_actually_does() {
        let ps = parse_scaling_policies(POLICIES_JSON);
        let by = |name: &str| {
            ps.iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("no policy {name}"))
                .summary
                .clone()
        };
        assert_eq!(
            by("target-cpu"),
            "ASGAverageCPUUtilization -> 60 (scale-in disabled)"
        );
        assert_eq!(by("steps"), "2 step(s), PercentChangeInCapacity");
        // The sign is the whole meaning: `+1` and `-1` are different policies
        // and a bare `1` is neither.
        assert_eq!(by("add-one"), "ChangeInCapacity +1");
        assert_eq!(by("remove-one"), "ChangeInCapacity -1");
    }

    #[test]
    fn policies_sort_by_name_and_keep_their_enabled_flag() {
        let ps = parse_scaling_policies(POLICIES_JSON);
        assert_eq!(
            ps.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            vec!["add-one", "remove-one", "steps", "target-cpu"]
        );
        // Absent is not the same as disabled.
        assert_eq!(ps[3].enabled, Some(true));
        assert_eq!(ps[0].enabled, None);
    }

    #[test]
    fn scheduled_actions_read_their_recurrence_and_sizes() {
        let raw = r#"{"ScheduledUpdateGroupActions":[
          {"ScheduledActionName":"scale-down-overnight","Recurrence":"0 22 * * *",
           "TimeZone":"America/Chicago","MinSize":0,"MaxSize":6,"DesiredCapacity":0},
          {"ScheduledActionName":"one-off","StartTime":"2026-09-20T06:00:00Z","DesiredCapacity":4}
        ]}"#;
        let actions = parse_scheduled_actions(raw);
        assert_eq!(actions[0].name, "one-off");
        assert_eq!(actions[0].recurrence, None);
        assert_eq!(actions[0].desired_capacity, Some(4));
        assert_eq!(actions[1].recurrence.as_deref(), Some("0 22 * * *"));
        assert_eq!(actions[1].time_zone.as_deref(), Some("America/Chicago"));
        // Zero is a real size, not a missing one.
        assert_eq!(actions[1].min_size, Some(0));
    }

    /// Activities keep the API's own newest-first order — that is already the
    /// answer the section asks for.
    #[test]
    fn activities_keep_the_apis_own_order_and_carry_the_cause() {
        let raw = r#"{"Activities":[
          {"StartTime":"2026-09-08T12:00:00Z","StatusCode":"Successful",
           "Description":"Terminating EC2 instance: i-0aaa",
           "Cause":"At 2026-09-08T12:00:00Z a monitor alarm TargetTracking triggered a policy changing the desired capacity from 3 to 2."},
          {"StartTime":"2026-09-07T09:00:00Z","StatusCode":"Failed",
           "Description":"Launching a new EC2 instance",
           "Cause":"At 2026-09-07T09:00:00Z an instance was started",
           "StatusMessage":"Insufficient capacity in us-east-1a"}
        ]}"#;
        let acts = parse_scaling_activities(raw);
        assert_eq!(acts.len(), 2);
        assert_eq!(acts[0].start_time.as_deref(), Some("2026-09-08T12:00:00Z"));
        assert!(acts[0].cause.contains("from 3 to 2"));
        assert_eq!(acts[1].status_code, "Failed");
        assert_eq!(
            acts[1].status_message.as_deref(),
            Some("Insufficient capacity in us-east-1a")
        );
    }

    /// Every one of these parsers fills one section of one panel, so
    /// unreadable input is an empty section rather than a panic.
    #[test]
    fn every_detail_parser_survives_rubbish() {
        assert!(parse_scaling_policies("{").is_empty());
        assert!(parse_scheduled_actions(r#"{"Nope":[]}"#).is_empty());
        assert!(parse_scaling_activities("").is_empty());
    }
}

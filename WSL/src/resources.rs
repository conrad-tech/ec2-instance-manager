//! Scaffolding shared by every resource type in the Inventory sub-tabs.
//!
//! No egui, and one AWS call: [`run_cli`], the single place every resource
//! fetch reaches the CLI. Each resource module owns its own typed cache and
//! its own parsing; what lives here is what all of them must agree on — how a
//! cache key is shaped, how long a result is good for, how an error is
//! classified, and which rows are worth fetching first.
//!
//! `FetchError` and `run_cli` started out in `elb.rs`. They moved here the
//! moment a second resource module needed them: a per-module copy of "what a
//! denial is" would let one sub-tab treat an `AccessDenied` as an ordinary
//! failure while its neighbour switched a column off, and nothing on screen
//! would say why the two behaved differently.

use crate::aws_cli::run_aws_cli;
use crate::error::AppError;
use std::time::Duration;

/// How long a fetched resource list is good for.
///
/// Five minutes rather than the inventory's 45 seconds: that one is short
/// because the EC2 State column has to be current, and buckets, hosted zones
/// and target groups do not change on that timescale. Each sub-tab has a
/// Refresh button for when they do.
pub const RESOURCE_TTL: Duration = Duration::from_secs(300);

/// The resource types the Inventory sub-tabs can list.
///
/// EC2 instances are deliberately absent: they have their own inventory, their
/// own cache and their own 45-second TTL, and folding them in here would mean
/// changing all three.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    TargetGroup,
    LoadBalancer,
    Asg,
    Bucket,
    HostedZone,
}

impl ResourceKind {
    /// The cache-key fragment. Stable: it is part of a key, not a label.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TargetGroup => "targetgroup",
            Self::LoadBalancer => "loadbalancer",
            Self::Asg => "asg",
            Self::Bucket => "bucket",
            Self::HostedZone => "hostedzone",
        }
    }

    /// The sub-tab's label.
    pub fn label(self) -> &'static str {
        match self {
            Self::TargetGroup => "Target Groups",
            Self::LoadBalancer => "Load Balancers",
            Self::Asg => "ASG",
            Self::Bucket => "S3",
            Self::HostedZone => "Route 53",
        }
    }

    /// True for a service that has no region at all.
    ///
    /// S3 and Route 53 are global: a bucket belongs to an account, not to a
    /// region, and `list-buckets` returns the same answer whichever region the
    /// CLI is pointed at. Keying either on region lists every one of them once
    /// per region the user has visited.
    pub fn is_global(self) -> bool {
        matches!(self, Self::Bucket | Self::HostedZone)
    }
}

/// The cache key for one kind's list in one account.
///
/// A global kind's key omits the region entirely rather than substituting a
/// placeholder, so the omission is visible in a logged key.
pub fn cache_key(kind: ResourceKind, mode: &str, account: &str, region: &str) -> String {
    if kind.is_global() {
        format!("{mode}:{account}:{}", kind.as_str())
    } else {
        format!("{mode}:{account}:{region}:{}", kind.as_str())
    }
}

/// The most rows a configured priority list may pull ahead of the visible
/// range.
///
/// A pattern like `prod` or `-` can match every target group in the account,
/// which would turn the bounded visible-row fill into one call per group. Past
/// the cap the rest are left to the ordinary path.
pub const PRIORITY_HEALTH_MAX: usize = 50;

/// Which of `names` a configured priority list claims, in list order, capped.
///
/// Matching is a **case-insensitive substring of the name**, never of the ARN:
/// an ARN carries an account id and a random suffix, so a short pattern would
/// match by accident.
///
/// A blank pattern is skipped rather than treated as a match-all. An empty
/// string is a substring of everything, so one stray entry would otherwise
/// prioritise the whole account — the same trap `forwards.rs` records for a
/// blank section marker.
pub fn priority_indexes(patterns: &[String], names: &[String], cap: usize) -> Vec<usize> {
    let needles = priority_needles(patterns);
    if needles.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    for (idx, name) in names.iter().enumerate() {
        if out.len() >= cap {
            break;
        }
        if matches_a_needle(name, &needles) {
            out.push(idx);
        }
    }
    out
}

/// How many of `names` the priority list claims, **ignoring the cap**.
///
/// [`priority_indexes`] truncates silently, and the silence is the failure:
/// the star marker on a row and a warning are meant to be the pair that makes
/// a too-broad pattern visible, so without this a pattern like `prod` matching
/// a whole account reads exactly like one matching three groups. The caller
/// compares this against [`PRIORITY_HEALTH_MAX`] and says so once.
///
/// Deliberately a separate function rather than a second return value:
/// `priority_indexes` is pure, in a module with no logger, and must stay that
/// way. The two share `priority_needles`, so the blank-pattern rule and the
/// case folding cannot drift between what is counted and what is prioritised.
pub fn priority_match_count(patterns: &[String], names: &[String]) -> usize {
    let needles = priority_needles(patterns);
    if needles.is_empty() {
        return 0;
    }
    names
        .iter()
        .filter(|name| matches_a_needle(name, &needles))
        .count()
}

/// The configured patterns, trimmed and case-folded, with the blanks dropped.
fn priority_needles(patterns: &[String]) -> Vec<String> {
    patterns
        .iter()
        .map(|p| p.trim().to_ascii_lowercase())
        .filter(|p| !p.is_empty())
        .collect()
}

fn matches_a_needle(name: &str, needles: &[String]) -> bool {
    let hay = name.to_ascii_lowercase();
    needles.iter().any(|needle| hay.contains(needle))
}

/// What a non-zero exit from the AWS CLI actually meant.
///
/// These are three different facts and they must never render alike. The S3
/// API reports "this bucket has no lifecycle policy" by *failing*, so treating
/// every failure as an error makes an ordinary bucket look broken — while
/// treating every failure as "None" hides a permissions hole, and a bucket
/// with no public-access block reads identically to one whose block you are
/// not allowed to read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Absence {
    /// The API's way of saying "nothing is configured". Render as `None`.
    Absent,
    /// The role lacks the permission. Render as `not permitted`.
    Denied,
    /// A real failure. Render the text.
    Failed(String),
}

/// AWS error codes that mean "nothing is configured", not "something went
/// wrong". Matched as substrings of stderr, because the CLI wraps the code in
/// a sentence: `An error occurred (NoSuchBucketPolicy) when calling …`.
const ABSENT_CODES: &[&str] = &[
    "NoSuchBucketPolicy",
    "NoSuchLifecycleConfiguration",
    "ServerSideEncryptionConfigurationNotFoundError",
    "NoSuchTagSet",
    "NoSuchPublicAccessBlockConfiguration",
    "NoSuchCORSConfiguration",
    "ReplicationConfigurationNotFoundError",
];

/// Error codes that mean the role lacks the permission. `UnauthorizedOperation`
/// is EC2's spelling, `AccessDeniedException` the one most other services use,
/// and the bare "is not authorized to perform" sentence turns up where an SCP
/// or a permission boundary is what refused.
const DENIED_CODES: &[&str] = &[
    "AccessDenied",
    "AccessDeniedException",
    "UnauthorizedOperation",
    "is not authorized to perform",
];

/// Read a failed AWS CLI invocation's stderr as one of three answers.
///
/// Denial is checked **before** the absent codes. None of today's codes
/// contain both, but the ordering is what keeps a future code that does from
/// being filed as "nothing configured" — the direction that hides a
/// permissions hole.
pub fn classify_absent(stderr: &str) -> Absence {
    let trimmed = stderr.trim();
    if DENIED_CODES.iter().any(|code| trimmed.contains(code)) {
        return Absence::Denied;
    }
    if ABSENT_CODES.iter().any(|code| trimmed.contains(code)) {
        return Absence::Absent;
    }
    Absence::Failed(trimmed.to_string())
}

/// Why a fetch did not produce data.
///
/// `Denied` is separate from `Failed` because the caller acts on it: the
/// Target Groups table's Healthy/Total column switches itself off for an
/// account on the first denial, rather than issuing one refused call per row
/// for as long as somebody keeps scrolling.
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
        match classify_absent(stderr) {
            Absence::Denied => Self::Denied,
            Absence::Absent | Absence::Failed(_) => Self::Failed(stderr.trim().to_string()),
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

/// The one place a resource fetch reaches the AWS CLI.
///
/// Every `fetch_*` in every resource module goes through here, so the profile,
/// the region and the error classification are decided once. A module that
/// called `run_aws_cli` directly would be the module whose denials render
/// differently from everybody else's.
pub fn run_cli(
    profile: &str,
    region: &str,
    args: &[&str],
) -> std::result::Result<String, FetchError> {
    run_aws_cli(Some(profile), Some(region), args).map_err(fetch_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A denial has to survive the trip through `FetchError`, or the
    /// Healthy/Total column never switches itself off and every scrolled row
    /// spends a refused call.
    #[test]
    fn a_fetch_error_keeps_the_denial_distinct_from_a_failure() {
        assert_eq!(
            FetchError::from_stderr(
                "An error occurred (AccessDenied) when calling DescribeTargetHealth"
            ),
            FetchError::Denied
        );
        assert_eq!(FetchError::Denied.message(), "not permitted");
        match FetchError::from_stderr("  Rate exceeded  ") {
            FetchError::Failed(text) => assert_eq!(text, "Rate exceeded"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// "Nothing configured" is not an answer a *list* call can give, so an
    /// absent code stays a failure here rather than becoming an empty list —
    /// which would read as "this account has no target groups".
    #[test]
    fn an_absent_code_is_still_a_failure_for_a_list_call() {
        assert!(matches!(
            FetchError::from_stderr(
                "An error occurred (NoSuchBucketPolicy) when calling GetBucketPolicy"
            ),
            FetchError::Failed(_)
        ));
    }

    /// Every one of these is the API saying "nothing is configured", not a
    /// failure. Rendering them as errors makes an ordinary bucket look broken.
    #[test]
    fn the_apis_spellings_of_nothing_configured_are_absent() {
        for stderr in [
            "An error occurred (NoSuchBucketPolicy) when calling the GetBucketPolicy operation: The bucket policy does not exist",
            "An error occurred (NoSuchLifecycleConfiguration) when calling the GetBucketLifecycleConfiguration operation",
            "An error occurred (ServerSideEncryptionConfigurationNotFoundError) when calling the GetBucketEncryption operation",
            "An error occurred (NoSuchTagSet) when calling the GetBucketTagging operation",
            "An error occurred (NoSuchPublicAccessBlockConfiguration) when calling the GetPublicAccessBlock operation",
        ] {
            assert_eq!(classify_absent(stderr), Absence::Absent, "for {stderr}");
        }
    }

    /// A permissions hole must never be reported as "nothing configured".
    #[test]
    fn a_denial_is_its_own_answer() {
        for stderr in [
            "An error occurred (AccessDenied) when calling the GetBucketPolicy operation: Access Denied",
            "An error occurred (AccessDeniedException) when calling the DescribeTargetHealth operation",
            "An error occurred (UnauthorizedOperation) when calling the DescribeInstances operation",
            "is not authorized to perform: elasticloadbalancing:DescribeTargetHealth",
        ] {
            assert_eq!(classify_absent(stderr), Absence::Denied, "for {stderr}");
        }
    }

    /// Anything unrecognised keeps its text — a throttle or a network error is
    /// neither absent nor denied, and swallowing it loses the diagnosis.
    #[test]
    fn anything_else_keeps_its_own_words() {
        let stderr = "An error occurred (Throttling) when calling the DescribeTargetGroups operation: Rate exceeded";
        match classify_absent(stderr) {
            Absence::Failed(text) => assert!(text.contains("Rate exceeded"), "got {text}"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// Blank stderr is a failure with nothing to say, never "nothing
    /// configured" — an empty answer must not be read as a fact about AWS.
    #[test]
    fn empty_stderr_is_a_failure_not_an_absence() {
        assert!(matches!(classify_absent("   "), Absence::Failed(_)));
    }

    /// The ordering is load-bearing, so it is pinned by construction: a
    /// message carrying both markers must be read as a denial. AWS has no
    /// such code today, which is exactly why nothing else would catch the
    /// two blocks being swapped — and misfiling a denial as "nothing
    /// configured" is the direction that hides a permissions hole.
    #[test]
    fn a_denial_wins_over_an_absent_code_in_the_same_message() {
        let both = "An error occurred (AccessDenied) when calling GetBucketPolicy: \
                    NoSuchBucketPolicy";
        assert_eq!(classify_absent(both), Absence::Denied);
    }

    /// S3 and Route 53 are global. A region in their cache key lists every
    /// bucket once per region the user has visited.
    #[test]
    fn a_global_kinds_key_carries_no_region() {
        for kind in [ResourceKind::Bucket, ResourceKind::HostedZone] {
            let east = cache_key(kind, "live", "111122223333", "us-east-1");
            let west = cache_key(kind, "live", "111122223333", "eu-west-2");
            assert_eq!(east, west, "{kind:?} must not key on region");
            assert!(!east.contains("us-east-1"), "{kind:?}: {east}");
        }
    }

    /// A regional kind must key on region, or two regions' target groups
    /// collapse into one list.
    #[test]
    fn a_regional_kinds_key_carries_its_region() {
        let east = cache_key(ResourceKind::TargetGroup, "live", "111122223333", "us-east-1");
        let west = cache_key(ResourceKind::TargetGroup, "live", "111122223333", "eu-west-2");
        assert_ne!(east, west);
    }

    /// Two accounts, and sim versus live, are never one cache entry.
    #[test]
    fn account_and_mode_separate_entries() {
        let a = cache_key(ResourceKind::TargetGroup, "live", "111122223333", "us-east-1");
        let b = cache_key(ResourceKind::TargetGroup, "live", "444455556666", "us-east-1");
        let sim = cache_key(ResourceKind::TargetGroup, "sim", "111122223333", "us-east-1");
        assert_ne!(a, b);
        assert_ne!(a, sim);
    }

    /// Two kinds in one account and region are never one entry.
    #[test]
    fn kinds_do_not_collide() {
        let tg = cache_key(ResourceKind::TargetGroup, "live", "1111", "us-east-1");
        let lb = cache_key(ResourceKind::LoadBalancer, "live", "1111", "us-east-1");
        assert_ne!(tg, lb);
    }

    /// Names drift in case the same way MMODAL_ENV does, and a pattern is
    /// typed by a human into a config file.
    #[test]
    fn priority_matching_ignores_case() {
        let names = vec!["APP-Web-Prod".to_string(), "other".to_string()];
        let patterns = vec!["app-web".to_string()];
        assert_eq!(priority_indexes(&patterns, &names, 50), vec![0]);
    }

    /// Substring, not exact match: "app-web" must find "app-web-prod-tg".
    #[test]
    fn priority_matching_is_a_substring() {
        let names = vec!["app-web-prod-tg".to_string()];
        assert_eq!(
            priority_indexes(&["app-web".to_string()], &names, 50),
            vec![0]
        );
    }

    /// One pattern claims EVERY name it matches, not just the first.
    ///
    /// This is the behaviour a configured list is actually relied on for: an
    /// operator writes one app name and expects every target group carrying it
    /// to be answered. Stopping at the first match would look like it worked —
    /// one row fills promptly — and quietly leave the rest to the scroll
    /// position.
    #[test]
    fn one_pattern_claims_every_name_it_matches() {
        let names = vec![
            "cassandra-prod-tg".to_string(),
            "kafka-prod-tg".to_string(),
            "cassandra-reaper-8080".to_string(),
            "CASSANDRA-Backup".to_string(),
            "postgres-tg".to_string(),
        ];
        // Indices, in the list's own order: all three cassandra rows, and
        // neither of the others.
        assert_eq!(
            priority_indexes(&["cassandra".to_string()], &names, 50),
            vec![0, 2, 3]
        );
    }

    /// The shipped state. Prioritising nothing is not prioritising everything.
    #[test]
    fn no_patterns_prioritises_nothing() {
        let names = vec!["a".to_string(), "b".to_string()];
        assert!(priority_indexes(&[], &names, 50).is_empty());
    }

    /// A blank entry is skipped, never treated as a match-all. This is the
    /// same bug forwards.rs records for a blank section marker: an empty
    /// string is a substring of everything, so one stray entry in the config
    /// would prioritise the entire account.
    #[test]
    fn a_blank_pattern_matches_nothing() {
        let names = vec!["a".to_string(), "b".to_string()];
        let patterns = vec!["".to_string(), "   ".to_string()];
        assert!(priority_indexes(&patterns, &names, 50).is_empty());
    }

    /// A broad pattern like "prod" could match the whole account and turn a
    /// bounded fill into an unbounded one.
    #[test]
    fn the_cap_bounds_a_pattern_that_matches_everything() {
        let names: Vec<String> = (0..200).map(|i| format!("prod-{i}")).collect();
        let hits = priority_indexes(&["prod".to_string()], &names, 50);
        assert_eq!(hits.len(), 50);
        // The cap keeps the first matches, so the order is the list's own.
        assert_eq!(hits[0], 0);
        assert_eq!(hits[49], 49);
    }

    /// The cap truncates in silence, so the count has to come from somewhere
    /// else: a pattern matching a whole account must not read like one
    /// matching three groups.
    #[test]
    fn the_match_count_ignores_the_cap() {
        let names: Vec<String> = (0..200).map(|i| format!("prod-{i}")).collect();
        let patterns = vec!["prod".to_string()];
        assert_eq!(
            priority_indexes(&patterns, &names, PRIORITY_HEALTH_MAX).len(),
            PRIORITY_HEALTH_MAX
        );
        assert_eq!(priority_match_count(&patterns, &names), 200);
    }

    /// Counting and prioritising share their needles, so the two can never
    /// disagree about a blank entry, about case, or about a name two patterns
    /// both claim.
    #[test]
    fn the_match_count_follows_the_same_rules_as_the_prioritising() {
        let names = vec![
            "APP-Web-Prod".to_string(),
            "app-web-dev".to_string(),
            "other".to_string(),
        ];
        // Blank entries are skipped by both, not treated as a match-all.
        assert_eq!(priority_match_count(&["".to_string()], &names), 0);
        assert_eq!(priority_match_count(&[], &names), 0);
        // Case-insensitive, and a name matched twice is still counted once.
        let patterns = vec!["app".to_string(), "web".to_string()];
        assert_eq!(priority_match_count(&patterns, &names), 2);
        assert_eq!(
            priority_indexes(&patterns, &names, 50).len(),
            priority_match_count(&patterns, &names),
            "under the cap the two must agree exactly"
        );
    }

    /// One name matched by two patterns is still one fetch.
    #[test]
    fn a_name_matched_twice_is_listed_once() {
        let names = vec!["app-web-prod".to_string()];
        let patterns = vec!["app".to_string(), "web".to_string()];
        assert_eq!(priority_indexes(&patterns, &names, 50), vec![0]);
    }
}

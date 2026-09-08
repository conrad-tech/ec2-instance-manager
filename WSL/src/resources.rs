//! Scaffolding shared by every resource type in the Inventory sub-tabs.
//!
//! Pure: no AWS calls and no egui. Each resource module owns its own typed
//! cache and its own parsing; what lives here is what all of them must agree
//! on — how a cache key is shaped, how long a result is good for, how an
//! error is classified, and which rows are worth fetching first.

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
            Self::Asg => "ASGs",
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

#[cfg(test)]
mod tests {
    use super::*;

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
}

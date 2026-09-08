//! Scaffolding shared by every resource type in the Inventory sub-tabs.
//!
//! Pure: no AWS calls and no egui. Each resource module owns its own typed
//! cache and its own parsing; what lives here is what all of them must agree
//! on — how a cache key is shaped, how long a result is good for, how an
//! error is classified, and which rows are worth fetching first.

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
}

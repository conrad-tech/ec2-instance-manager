//! S3 buckets: the list the S3 sub-tab shows, and each bucket's configuration.
//!
//! Parsing is pure and tested against captured payloads; the `fetch_*`
//! functions are thin wrappers over `resources::run_cli`. Same split as
//! `elb.rs` and `asg.rs`.
//!
//! **Object contents are deliberately out of scope.** This browses
//! configuration — versioning, encryption, who can reach it — and never lists
//! or reads objects. A bucket can hold millions of them and nothing here is
//! shaped to page through that.
//!
//! Two things about S3 make this module unlike the other two:
//!
//! - **It is a global service.** `list-buckets` returns every bucket in the
//!   account whatever region the CLI is pointed at, so the cache key carries
//!   no region (`ResourceKind::is_global`). Keying on region would list every
//!   bucket once per region the user has visited.
//! - **Half the detail calls report "nothing is configured" by FAILING.**
//!   `get-bucket-policy` on a bucket with no policy exits non-zero with
//!   `NoSuchBucketPolicy`; so do lifecycle, tagging, encryption and the
//!   public access block. `resources::classify_absent` is what tells that
//!   apart from a permissions failure, and it exists for exactly this module:
//!   reading every failure as an error makes an ordinary bucket look broken,
//!   and reading every failure as "None" hides a permissions hole.

use crate::resources::{classify_absent, run_cli as run, Absence};

pub use crate::resources::FetchError;

/// One bucket, as the S3 sub-tab lists it.
///
/// This is **everything `list-buckets` returns** — a name, a creation date and
/// (on a recent CLI) a region. Every other fact about a bucket is its own API
/// call, which is why the table has three columns and the detail view has
/// eight sections.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Bucket {
    pub name: String,
    pub created: Option<String>,
    /// From the list's own `BucketRegion`, which a recent AWS CLI returns and
    /// an older one omits. Empty when it did not — see [`parse_buckets`].
    pub region: String,
    /// Stamped in by the caller from the AWS context, as every other resource
    /// type's is.
    pub account_id: String,
}

impl Bucket {
    /// The region to address this bucket's own detail calls with.
    ///
    /// **A bucket is reached in the region it lives in.** Several `s3api`
    /// calls answer a bucket in another region with `PermanentRedirect`
    /// rather than following it, so pointing every call at the account's
    /// configured region fails on exactly the buckets that are not in it —
    /// and a redirect reads like a permissions problem. Falls back to the
    /// account's region when the CLI did not report one, which is what the
    /// call would have used anyway.
    pub fn call_region<'a>(&'a self, account_region: &'a str) -> &'a str {
        if self.region.is_empty() {
            account_region
        } else {
            &self.region
        }
    }
}

/// Read `s3api list-buckets --output json`.
///
/// An unreadable payload yields no buckets rather than an error, for the
/// reason every other list parser here does: this fills one table, and the
/// caller tells an empty account from a failed call by whether the *fetch*
/// returned `Err`, never by an empty list.
///
/// **`BucketRegion` is read where the CLI provides it and left blank where it
/// does not.** It arrived in `ListBuckets` relatively recently; an older CLI
/// simply omits it. The alternative — one `get-bucket-location` per bucket —
/// would turn opening the tab into a call per row on an account with hundreds
/// of buckets, which is the trap the Target Groups health column already
/// records. A blank region column is worth more than that.
pub fn parse_buckets(raw: &str) -> Vec<Bucket> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    let Some(entries) = value.get("Buckets").and_then(|v| v.as_array()) else {
        return Vec::new();
    };

    let mut out: Vec<Bucket> = entries
        .iter()
        .filter_map(|b| {
            Some(Bucket {
                name: str_field(b, "Name")?,
                created: str_field(b, "CreationDate"),
                region: str_field(b, "BucketRegion").unwrap_or_default(),
                account_id: String::new(),
            })
        })
        .collect();

    // The API's order is not stable between calls.
    out.sort_by(|a, b| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()));
    out
}

/// What the Region cell says when the CLI did not report one.
///
/// Words, not a dash. A dash in a column whose other values are region names
/// reads as "this bucket has no region", which is not a thing a bucket can
/// be; the truth is that this build of the CLI did not say.
pub fn region_label(bucket: &Bucket) -> String {
    if bucket.region.is_empty() {
        "not reported".to_string()
    } else {
        bucket.region.clone()
    }
}

/// The whole haystack the search box filters a bucket row on.
///
/// **Every column the table shows must appear here**, plus the account, which
/// the table dropped. There is nothing else to search on: the list call
/// returns three fields, and everything richer is behind a per-bucket call
/// that the search box cannot afford to make.
pub fn bucket_searchable_text(b: &Bucket) -> String {
    let mut out = String::new();
    for field in [
        b.name.as_str(),
        b.region.as_str(),
        b.account_id.as_str(),
        b.created.as_deref().unwrap_or(""),
    ] {
        out.push_str(&field.to_ascii_lowercase());
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------- sections

/// One section of a bucket's detail view — and one API call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BucketSection {
    PublicAccess,
    Ownership,
    Versioning,
    Encryption,
    Logging,
    Lifecycle,
    Policy,
    Tags,
}

impl BucketSection {
    /// The order the detail panel renders them in.
    ///
    /// Public access first, deliberately. It is the question somebody opens a
    /// bucket to answer, and burying it under versioning and tags is how a
    /// world-readable bucket goes unnoticed.
    pub fn all() -> [BucketSection; 8] {
        [
            Self::PublicAccess,
            Self::Ownership,
            Self::Versioning,
            Self::Encryption,
            Self::Logging,
            Self::Lifecycle,
            Self::Policy,
            Self::Tags,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::PublicAccess => "Public access",
            Self::Ownership => "Object ownership",
            Self::Versioning => "Versioning",
            Self::Encryption => "Encryption",
            Self::Logging => "Server access logging",
            Self::Lifecycle => "Lifecycle rules",
            Self::Policy => "Bucket policy",
            Self::Tags => "Tags",
        }
    }

    /// The `s3api` subcommand behind it.
    fn command(self) -> &'static str {
        match self {
            Self::PublicAccess => "get-public-access-block",
            Self::Ownership => "get-bucket-ownership-controls",
            Self::Versioning => "get-bucket-versioning",
            Self::Encryption => "get-bucket-encryption",
            Self::Logging => "get-bucket-logging",
            Self::Lifecycle => "get-bucket-lifecycle-configuration",
            Self::Policy => "get-bucket-policy",
            Self::Tags => "get-bucket-tagging",
        }
    }

    /// What "nothing is configured" means here, and whether it is worrying.
    ///
    /// For seven of the eight it is benign — a bucket with no lifecycle rules
    /// or no tags is an ordinary bucket, and rendering that as an error makes
    /// every ordinary bucket look broken. **The public access block is the
    /// exception**: absent there means nothing at the bucket level is stopping
    /// it being made public, and a reader must not have to notice the absence
    /// of a section to learn that.
    pub fn absent_note(self) -> (String, bool) {
        match self {
            Self::PublicAccess => (
                "No public access block — nothing here stops this bucket being made public"
                    .to_string(),
                true,
            ),
            Self::Ownership => ("No ownership controls set".to_string(), false),
            other => (format!("No {} configured", other.label().to_lowercase()), false),
        }
    }
}

/// The rows (or document) one section came back with.
#[derive(Clone, Debug, PartialEq)]
pub enum BucketFacts {
    /// Name/value rows, which is what most of these are.
    Pairs(Vec<(String, String)>),
    /// A document, rendered monospace with a copy button. Only the bucket
    /// policy is one.
    Document(String),
    /// Rows plus a headline the reader must not have to derive for
    /// themselves.
    ///
    /// Only the public access block uses this, and it earns its own variant:
    /// four booleans is not an answer to "can this bucket be made public",
    /// and that question is the one thing on this panel somebody is actually
    /// checking. Every other section genuinely is just rows.
    Verdict {
        note: String,
        worrying: bool,
        pairs: Vec<(String, String)>,
    },
}

/// What one section's call came back with.
///
/// Four outcomes, and **they must never render alike**. This is the enum
/// `resources::classify_absent` exists to fill: the S3 API answers "this
/// bucket has no lifecycle policy" by exiting non-zero, so reading every
/// failure as an error makes an ordinary bucket look broken — while reading
/// every failure as "None" hides a permissions hole, and a bucket with no
/// public-access block reads identically to one whose block you are not
/// allowed to read.
#[derive(Clone, Debug, PartialEq)]
pub enum SectionResult {
    Facts(BucketFacts),
    /// The API's way of saying nothing is configured.
    Absent,
    /// The role lacks the permission.
    Denied,
    Failed(String),
}

// ----------------------------------------------------------------- parsing

fn str_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

fn bool_field(value: &serde_json::Value, key: &str) -> bool {
    value.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

fn yes_no(v: bool) -> String {
    if v { "yes" } else { "no" }.to_string()
}

/// The four `PublicAccessBlockConfiguration` flags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PublicAccessBlock {
    pub block_public_acls: bool,
    pub ignore_public_acls: bool,
    pub block_public_policy: bool,
    pub restrict_public_buckets: bool,
}

impl PublicAccessBlock {
    /// The four, in the order the console lists them.
    pub fn flags(&self) -> [(&'static str, bool); 4] {
        [
            ("Block public ACLs", self.block_public_acls),
            ("Ignore public ACLs", self.ignore_public_acls),
            ("Block public bucket policies", self.block_public_policy),
            ("Restrict public buckets", self.restrict_public_buckets),
        ]
    }

    pub fn all_blocked(&self) -> bool {
        self.flags().iter().all(|(_, on)| *on)
    }
}

/// Read `get-public-access-block --output json`.
///
/// **A missing flag is read as `false`, never as absent.** The API omits a
/// flag it has never been given a value for, and defaulting that to "blocked"
/// would report a bucket as protected by a setting nobody ever set.
pub fn parse_public_access_block(raw: &str) -> Option<PublicAccessBlock> {
    let value = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    let cfg = value.get("PublicAccessBlockConfiguration")?;
    Some(PublicAccessBlock {
        block_public_acls: bool_field(cfg, "BlockPublicAcls"),
        ignore_public_acls: bool_field(cfg, "IgnorePublicAcls"),
        block_public_policy: bool_field(cfg, "BlockPublicPolicy"),
        restrict_public_buckets: bool_field(cfg, "RestrictPublicBuckets"),
    })
}

/// The headline for a public access block, and whether it is worrying.
///
/// **Anything short of all four is worrying.** They are not independent
/// safeguards that partly cover for each other — a bucket with public
/// policies blocked but public ACLs allowed is reachable, and "3 of 4" reads
/// as mostly fine, which is why the note names the ones that are OFF rather
/// than counting the ones that are on.
pub fn public_access_note(block: &PublicAccessBlock) -> (String, bool) {
    if block.all_blocked() {
        return ("All four settings block public access".to_string(), false);
    }
    let off: Vec<&str> = block
        .flags()
        .iter()
        .filter(|(_, on)| !*on)
        .map(|(label, _)| *label)
        .collect();
    (format!("NOT set: {}", off.join(", ")), true)
}

fn parse_public_access(raw: &str) -> Option<BucketFacts> {
    let block = parse_public_access_block(raw)?;
    let (note, worrying) = public_access_note(&block);
    Some(BucketFacts::Verdict {
        note,
        worrying,
        pairs: block
            .flags()
            .iter()
            .map(|(label, on)| (label.to_string(), yes_no(*on)))
            .collect(),
    })
}

fn parse_ownership(raw: &str) -> Option<BucketFacts> {
    let value = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    let rules = value
        .get("OwnershipControls")
        .and_then(|c| c.get("Rules"))
        .and_then(|r| r.as_array())?;
    let pairs: Vec<(String, String)> = rules
        .iter()
        .filter_map(|r| {
            Some((
                "Object ownership".to_string(),
                str_field(r, "ObjectOwnership")?,
            ))
        })
        .collect();
    if pairs.is_empty() {
        return None;
    }
    Some(BucketFacts::Pairs(pairs))
}

/// Read `get-bucket-versioning --output json`.
///
/// **An empty reply is "Never enabled", not absent.** Versioning is the one
/// section where "nothing configured" is a real, reportable state of the
/// bucket rather than the absence of a feature — and a bucket that has never
/// had versioning is a different thing from one where it was turned on and
/// then suspended, which is what `Suspended` means.
fn parse_versioning(raw: &str) -> Option<BucketFacts> {
    let value = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    let mut pairs = vec![(
        "Status".to_string(),
        str_field(&value, "Status").unwrap_or_else(|| "Never enabled".to_string()),
    )];
    if let Some(mfa) = str_field(&value, "MFADelete") {
        pairs.push(("MFA delete".to_string(), mfa));
    }
    Some(BucketFacts::Pairs(pairs))
}

fn parse_encryption(raw: &str) -> Option<BucketFacts> {
    let value = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    let rules = value
        .get("ServerSideEncryptionConfiguration")
        .and_then(|c| c.get("Rules"))
        .and_then(|r| r.as_array())?;
    let mut pairs = Vec::new();
    for rule in rules {
        if let Some(d) = rule.get("ApplyServerSideEncryptionByDefault") {
            if let Some(algo) = str_field(d, "SSEAlgorithm") {
                pairs.push(("Algorithm".to_string(), algo));
            }
            // Only on SSE-KMS, and worth naming: which key encrypts a bucket
            // decides who can read it just as much as the bucket policy does.
            if let Some(key) = str_field(d, "KMSMasterKeyID") {
                pairs.push(("KMS key".to_string(), key));
            }
        }
        if let Some(bk) = rule.get("BucketKeyEnabled").and_then(|v| v.as_bool()) {
            pairs.push(("S3 Bucket Key".to_string(), yes_no(bk)));
        }
    }
    if pairs.is_empty() {
        return None;
    }
    Some(BucketFacts::Pairs(pairs))
}

/// Read `get-bucket-logging --output json`.
///
/// **An empty reply means logging is off**, and this call reports that with a
/// success rather than a failure — unlike its neighbours. So the `None` here
/// is doing the same job `classify_absent` does for the others.
fn parse_logging(raw: &str) -> Option<BucketFacts> {
    let value = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    let enabled = value.get("LoggingEnabled")?;
    let mut pairs = Vec::new();
    if let Some(bucket) = str_field(enabled, "TargetBucket") {
        pairs.push(("Target bucket".to_string(), bucket));
    }
    if let Some(prefix) = str_field(enabled, "TargetPrefix") {
        pairs.push(("Target prefix".to_string(), prefix));
    }
    if pairs.is_empty() {
        return None;
    }
    Some(BucketFacts::Pairs(pairs))
}

/// Read `get-bucket-lifecycle-configuration --output json`.
///
/// One row per rule, keyed on the rule's own id. A rule with no id renders as
/// `(unnamed)` rather than an empty key — an empty key column reads as a
/// rendering fault, and an unnamed lifecycle rule is legal and common.
fn parse_lifecycle(raw: &str) -> Option<BucketFacts> {
    let value = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    let rules = value.get("Rules").and_then(|r| r.as_array())?;
    let pairs: Vec<(String, String)> = rules
        .iter()
        .map(|rule| {
            let id = str_field(rule, "ID").unwrap_or_else(|| "(unnamed)".to_string());
            (id, lifecycle_rule_summary(rule))
        })
        .collect();
    if pairs.is_empty() {
        return None;
    }
    Some(BucketFacts::Pairs(pairs))
}

/// One lifecycle rule as a sentence.
///
/// **The scope is always stated, including when it is the whole bucket.** A
/// rule that expires objects after 30 days and a rule that expires
/// `logs/`-prefixed objects after 30 days are very different rules, and
/// omitting the scope when there is none renders the dangerous one as the
/// safe one.
fn lifecycle_rule_summary(rule: &serde_json::Value) -> String {
    let mut parts = Vec::new();
    parts.push(str_field(rule, "Status").unwrap_or_else(|| "unknown status".to_string()));

    // The filter is `Filter` on a v2 rule and a bare `Prefix` on a v1 one,
    // and a real account has both. Reading only one shows half the rules as
    // applying to the whole bucket when they do not.
    let prefix = str_field(rule, "Prefix").or_else(|| {
        rule.get("Filter").and_then(|f| {
            str_field(f, "Prefix").or_else(|| f.get("And").and_then(|a| str_field(a, "Prefix")))
        })
    });
    parts.push(match prefix {
        Some(p) => format!("prefix \"{p}\""),
        None => "whole bucket".to_string(),
    });

    for (key, verb) in [
        ("Expiration", "expire"),
        ("NoncurrentVersionExpiration", "expire noncurrent"),
    ] {
        if let Some(node) = rule.get(key) {
            if let Some(days) = node
                .get("Days")
                .or_else(|| node.get("NoncurrentDays"))
                .and_then(|v| v.as_i64())
            {
                parts.push(format!("{verb} after {days}d"));
            } else if let Some(date) = str_field(node, "Date") {
                parts.push(format!("{verb} on {date}"));
            } else if node
                .get("ExpiredObjectDeleteMarker")
                .and_then(|v| v.as_bool())
                == Some(true)
            {
                parts.push("remove expired delete markers".to_string());
            }
        }
    }
    for key in ["Transitions", "NoncurrentVersionTransitions"] {
        if let Some(list) = rule.get(key).and_then(|v| v.as_array()) {
            for t in list {
                if let Some(class) = str_field(t, "StorageClass") {
                    match t
                        .get("Days")
                        .or_else(|| t.get("NoncurrentDays"))
                        .and_then(|v| v.as_i64())
                    {
                        Some(days) => parts.push(format!("-> {class} after {days}d")),
                        None => parts.push(format!("-> {class}")),
                    }
                }
            }
        }
    }
    if let Some(days) = rule
        .get("AbortIncompleteMultipartUpload")
        .and_then(|n| n.get("DaysAfterInitiation"))
        .and_then(|v| v.as_i64())
    {
        parts.push(format!("abort incomplete uploads after {days}d"));
    }
    parts.join(" · ")
}

/// Read `get-bucket-policy --output json`.
///
/// **The policy arrives as a JSON string INSIDE a JSON object**, so the raw
/// reply is one escaped line. Rendering that is technically the policy and
/// practically unreadable; it is re-parsed and pretty-printed. A policy that
/// will not re-parse is shown verbatim rather than dropped — this is the
/// document somebody opened the panel to read.
fn parse_policy(raw: &str) -> Option<BucketFacts> {
    let value = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    let policy = str_field(&value, "Policy")?;
    let pretty = serde_json::from_str::<serde_json::Value>(&policy)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or(policy);
    Some(BucketFacts::Document(pretty))
}

fn parse_tags(raw: &str) -> Option<BucketFacts> {
    let value = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    let set = value.get("TagSet").and_then(|t| t.as_array())?;
    let mut pairs: Vec<(String, String)> = set
        .iter()
        .filter_map(|t| Some((str_field(t, "Key")?, str_field(t, "Value").unwrap_or_default())))
        .collect();
    if pairs.is_empty() {
        return None;
    }
    pairs.sort_by(|a, b| a.0.to_ascii_lowercase().cmp(&b.0.to_ascii_lowercase()));
    Some(BucketFacts::Pairs(pairs))
}

/// Read one section's reply.
///
/// Split out from the fetch so every parser is testable against a captured
/// payload without AWS, and so the mapping from section to parser is one
/// readable table rather than eight branches inside a thread.
pub fn parse_section(section: BucketSection, raw: &str) -> SectionResult {
    let parsed = match section {
        BucketSection::PublicAccess => parse_public_access(raw),
        BucketSection::Ownership => parse_ownership(raw),
        BucketSection::Versioning => parse_versioning(raw),
        BucketSection::Encryption => parse_encryption(raw),
        BucketSection::Logging => parse_logging(raw),
        BucketSection::Lifecycle => parse_lifecycle(raw),
        BucketSection::Policy => parse_policy(raw),
        BucketSection::Tags => parse_tags(raw),
    };
    match parsed {
        Some(facts) => SectionResult::Facts(facts),
        // The call succeeded and said nothing is configured — which for
        // `get-bucket-logging` is how the API reports it, and for the rest is
        // a reply we could not read. Either way "None" is the honest render:
        // an error would be a claim about AWS we cannot support.
        None => SectionResult::Absent,
    }
}

// ---------------------------------------------------------------- fetching

/// Every bucket in one account.
///
/// **No region in the answer and none in the key.** S3 is global; this call
/// returns the same list whichever region the CLI is pointed at, and the
/// region argument only decides which endpoint the request goes to.
pub fn fetch_buckets(
    profile: &str,
    region: &str,
    account_id: &str,
) -> std::result::Result<Vec<Bucket>, FetchError> {
    let raw = run(profile, region, &["s3api", "list-buckets", "--output", "json"])?;
    let mut buckets = parse_buckets(&raw);
    for bucket in &mut buckets {
        bucket.account_id = account_id.to_string();
    }
    Ok(buckets)
}

/// Read one section of one bucket's configuration.
///
/// Returns a [`SectionResult`] rather than a `Result`, because a non-zero
/// exit here has **three** meanings and only one of them is an error. That is
/// the whole reason `classify_absent` exists; see this module's header.
pub fn fetch_section(
    profile: &str,
    region: &str,
    bucket: &str,
    section: BucketSection,
) -> SectionResult {
    match run(
        profile,
        region,
        &[
            "s3api",
            section.command(),
            "--bucket",
            bucket,
            "--output",
            "json",
        ],
    ) {
        Ok(raw) => parse_section(section, &raw),
        // `run_cli` already folded a denial into `FetchError::Denied`, but the
        // absent codes reach here as `Failed` — a list call has no "nothing
        // configured" answer, so `FetchError::from_stderr` deliberately keeps
        // them as failures. This is the one caller for which they ARE an
        // answer, so the text is re-classified.
        Err(FetchError::Denied) => SectionResult::Denied,
        Err(FetchError::Failed(text)) => match classify_absent(&text) {
            Absence::Absent => SectionResult::Absent,
            Absence::Denied => SectionResult::Denied,
            Absence::Failed(msg) => SectionResult::Failed(msg),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUCKETS_JSON: &str = r#"{
      "Buckets": [
        { "Name": "zulu-logs", "CreationDate": "2021-06-02T09:00:00+00:00", "BucketRegion": "eu-west-2" },
        { "Name": "Alpha-Assets", "CreationDate": "2020-01-15T10:30:00+00:00", "BucketRegion": "us-east-1" }
      ],
      "Owner": { "DisplayName": "acct", "ID": "abc" }
    }"#;

    /// The API's order is not stable, and case must not split the sort — a
    /// capitalised bucket sorting above every lowercase one is how a list
    /// stops reading alphabetically.
    #[test]
    fn buckets_sort_by_name_ignoring_case() {
        let bs = parse_buckets(BUCKETS_JSON);
        assert_eq!(
            bs.iter().map(|b| b.name.as_str()).collect::<Vec<_>>(),
            vec!["Alpha-Assets", "zulu-logs"]
        );
        assert_eq!(bs[0].region, "us-east-1");
        assert_eq!(bs[0].created.as_deref(), Some("2020-01-15T10:30:00+00:00"));
    }

    /// `BucketRegion` is recent. An older CLI omits it, and the answer is a
    /// blank region — never a `get-bucket-location` per row, which is the
    /// call-per-row trap the Target Groups health column already records.
    #[test]
    fn an_older_cli_leaves_the_region_blank_rather_than_costing_a_call() {
        let raw = r#"{"Buckets":[{"Name":"old","CreationDate":"2019-01-01T00:00:00+00:00"}]}"#;
        let b = &parse_buckets(raw)[0];
        assert!(b.region.is_empty());
        // And the cell says which it is, rather than showing a dash that
        // reads as "this bucket has no region".
        assert_eq!(region_label(b), "not reported");
    }

    /// A bucket is reached in the region it lives in: several `s3api` calls
    /// answer a bucket in another region with `PermanentRedirect`, which
    /// reads exactly like a permissions problem.
    #[test]
    fn a_bucket_is_addressed_in_its_own_region_when_one_is_known() {
        let known = Bucket {
            region: "ap-south-1".to_string(),
            ..Default::default()
        };
        assert_eq!(known.call_region("us-east-1"), "ap-south-1");
        // With none reported, the account's own region is what the call
        // would have used anyway.
        assert_eq!(Bucket::default().call_region("us-east-1"), "us-east-1");
    }

    #[test]
    fn unreadable_input_yields_no_buckets() {
        assert!(parse_buckets("not json").is_empty());
        assert!(parse_buckets(r#"{"Something":[]}"#).is_empty());
    }

    /// Every column the table shows has to be searchable, or the column reads
    /// as "search is broken".
    #[test]
    fn the_haystack_carries_every_column() {
        let mut b = parse_buckets(BUCKETS_JSON)[0].clone();
        b.account_id = "111122223333".to_string();
        let hay = bucket_searchable_text(&b);
        for needle in ["alpha-assets", "us-east-1", "111122223333", "2020-01-15"] {
            assert!(hay.contains(needle), "missing {needle}: {hay}");
        }
        // Folded once, so `Alpha-Assets` is found by typing `alpha`.
        assert_eq!(hay, hay.to_ascii_lowercase());
    }

    // ------------------------------------------------------ public access

    const BLOCK_ALL: &str = r#"{"PublicAccessBlockConfiguration":{
      "BlockPublicAcls":true,"IgnorePublicAcls":true,
      "BlockPublicPolicy":true,"RestrictPublicBuckets":true}}"#;

    #[test]
    fn a_fully_blocked_bucket_says_so_and_is_not_worrying() {
        let block = parse_public_access_block(BLOCK_ALL).expect("parsed");
        assert!(block.all_blocked());
        let (note, worrying) = public_access_note(&block);
        assert!(!worrying);
        assert!(note.contains("All four"), "{note}");
    }

    /// Anything short of all four is worrying, and the note names the ones
    /// that are OFF. They are not independent safeguards partly covering for
    /// each other — a bucket with public policies blocked and public ACLs
    /// allowed is reachable — and "3 of 4" reads as mostly fine.
    #[test]
    fn a_partly_blocked_bucket_names_the_settings_that_are_off() {
        let raw = r#"{"PublicAccessBlockConfiguration":{
          "BlockPublicAcls":false,"IgnorePublicAcls":true,
          "BlockPublicPolicy":true,"RestrictPublicBuckets":true}}"#;
        let block = parse_public_access_block(raw).expect("parsed");
        let (note, worrying) = public_access_note(&block);
        assert!(worrying, "3 of 4 is not fine");
        assert!(note.contains("Block public ACLs"), "{note}");
        assert!(!note.contains("Ignore public ACLs"), "only the OFF ones: {note}");
    }

    /// The API omits a flag it has never been given a value for. Defaulting
    /// that to "blocked" would report a bucket as protected by a setting
    /// nobody ever set.
    #[test]
    fn a_missing_flag_is_read_as_not_blocked() {
        let raw = r#"{"PublicAccessBlockConfiguration":{"BlockPublicAcls":true}}"#;
        let block = parse_public_access_block(raw).expect("parsed");
        assert!(block.block_public_acls);
        assert!(!block.restrict_public_buckets);
        assert!(!block.all_blocked());
    }

    /// The one Absent in this panel that is not benign. A reader must not
    /// have to notice a missing section to learn a bucket can be made public.
    #[test]
    fn an_absent_public_access_block_is_the_one_worrying_absence() {
        let (note, worrying) = BucketSection::PublicAccess.absent_note();
        assert!(worrying);
        assert!(note.contains("made public"), "{note}");

        // Every other section's absence is an ordinary bucket, and must not
        // be dressed up as a problem.
        for section in BucketSection::all() {
            if section == BucketSection::PublicAccess {
                continue;
            }
            let (_, worrying) = section.absent_note();
            assert!(!worrying, "{section:?} must not be worrying when absent");
        }
    }

    #[test]
    fn the_public_access_section_carries_its_verdict_and_its_rows() {
        match parse_section(BucketSection::PublicAccess, BLOCK_ALL) {
            SectionResult::Facts(BucketFacts::Verdict {
                worrying, pairs, ..
            }) => {
                assert!(!worrying);
                assert_eq!(pairs.len(), 4);
            }
            other => panic!("expected a verdict, got {other:?}"),
        }
    }

    // ---------------------------------------------------------- the rest

    /// An empty reply is "Never enabled", a real state of the bucket — and a
    /// different thing from `Suspended`, which means it was on and was turned
    /// off.
    #[test]
    fn versioning_tells_never_enabled_from_suspended() {
        let never = parse_section(BucketSection::Versioning, "{}");
        assert_eq!(
            never,
            SectionResult::Facts(BucketFacts::Pairs(vec![(
                "Status".to_string(),
                "Never enabled".to_string()
            )]))
        );
        let suspended = parse_section(
            BucketSection::Versioning,
            r#"{"Status":"Suspended","MFADelete":"Disabled"}"#,
        );
        match suspended {
            SectionResult::Facts(BucketFacts::Pairs(pairs)) => {
                assert_eq!(pairs[0].1, "Suspended");
                assert_eq!(pairs[1], ("MFA delete".to_string(), "Disabled".to_string()));
            }
            other => panic!("got {other:?}"),
        }
    }

    /// Which key encrypts a bucket decides who can read it just as much as
    /// the bucket policy does, so SSE-KMS must name it.
    #[test]
    fn encryption_names_the_kms_key() {
        let raw = r#"{"ServerSideEncryptionConfiguration":{"Rules":[
          {"ApplyServerSideEncryptionByDefault":{"SSEAlgorithm":"aws:kms",
           "KMSMasterKeyID":"arn:aws:kms:us-east-1:111122223333:key/abc"},
           "BucketKeyEnabled":true}]}}"#;
        match parse_section(BucketSection::Encryption, raw) {
            SectionResult::Facts(BucketFacts::Pairs(pairs)) => {
                assert_eq!(pairs[0], ("Algorithm".to_string(), "aws:kms".to_string()));
                assert!(pairs[1].1.contains("key/abc"));
                assert_eq!(pairs[2], ("S3 Bucket Key".to_string(), "yes".to_string()));
            }
            other => panic!("got {other:?}"),
        }
    }

    /// Logging reports "off" with a SUCCESS and an empty object, unlike its
    /// neighbours which report it by failing. The parser is what turns that
    /// into the same `Absent` everything else produces.
    #[test]
    fn logging_off_is_absent_even_though_the_call_succeeded() {
        assert_eq!(parse_section(BucketSection::Logging, "{}"), SectionResult::Absent);
        match parse_section(
            BucketSection::Logging,
            r#"{"LoggingEnabled":{"TargetBucket":"logs","TargetPrefix":"app/"}}"#,
        ) {
            SectionResult::Facts(BucketFacts::Pairs(pairs)) => {
                assert_eq!(pairs[0].1, "logs");
                assert_eq!(pairs[1].1, "app/");
            }
            other => panic!("got {other:?}"),
        }
    }

    /// A rule that expires everything after 30 days and one that expires
    /// `logs/` after 30 days are very different rules. Omitting the scope
    /// when there is none renders the dangerous one as the safe one.
    #[test]
    fn a_lifecycle_rule_always_states_its_scope() {
        let raw = r#"{"Rules":[
          {"ID":"whole","Status":"Enabled","Expiration":{"Days":30}},
          {"ID":"scoped","Status":"Enabled","Filter":{"Prefix":"logs/"},
           "Expiration":{"Days":30}}
        ]}"#;
        match parse_section(BucketSection::Lifecycle, raw) {
            SectionResult::Facts(BucketFacts::Pairs(pairs)) => {
                assert!(pairs[0].1.contains("whole bucket"), "{:?}", pairs[0]);
                assert!(pairs[1].1.contains("prefix \"logs/\""), "{:?}", pairs[1]);
                assert!(pairs[0].1.contains("expire after 30d"));
            }
            other => panic!("got {other:?}"),
        }
    }

    /// The filter is `Filter` on a v2 rule and a bare `Prefix` on a v1 one,
    /// and a real account has both. Reading only one shows half the rules as
    /// applying to the whole bucket when they do not.
    #[test]
    fn a_legacy_lifecycle_rule_keeps_its_prefix() {
        let raw = r#"{"Rules":[{"ID":"v1","Status":"Enabled","Prefix":"tmp/",
          "Transitions":[{"Days":60,"StorageClass":"GLACIER"}]}]}"#;
        match parse_section(BucketSection::Lifecycle, raw) {
            SectionResult::Facts(BucketFacts::Pairs(pairs)) => {
                assert!(pairs[0].1.contains("prefix \"tmp/\""), "{:?}", pairs[0]);
                assert!(pairs[0].1.contains("GLACIER after 60d"), "{:?}", pairs[0]);
            }
            other => panic!("got {other:?}"),
        }
    }

    /// An unnamed lifecycle rule is legal and common; an empty key column
    /// reads as a rendering fault.
    #[test]
    fn an_unnamed_lifecycle_rule_is_labelled_not_blank() {
        let raw = r#"{"Rules":[{"Status":"Enabled","Expiration":{"Days":1}}]}"#;
        match parse_section(BucketSection::Lifecycle, raw) {
            SectionResult::Facts(BucketFacts::Pairs(pairs)) => {
                assert_eq!(pairs[0].0, "(unnamed)");
            }
            other => panic!("got {other:?}"),
        }
    }

    /// The policy arrives as a JSON string inside a JSON object, so the raw
    /// reply is one escaped line. Rendering that is technically the policy
    /// and practically unreadable.
    #[test]
    fn a_bucket_policy_is_unwrapped_and_pretty_printed() {
        let raw = r#"{"Policy":"{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\"}]}"}"#;
        match parse_section(BucketSection::Policy, raw) {
            SectionResult::Facts(BucketFacts::Document(doc)) => {
                assert!(doc.contains('\n'), "must be pretty-printed: {doc}");
                assert!(!doc.contains("\\\""), "must not still be escaped: {doc}");
                assert!(doc.contains("2012-10-17"));
            }
            other => panic!("got {other:?}"),
        }
    }

    /// A policy that will not re-parse is shown verbatim rather than dropped.
    /// This is the document somebody opened the panel to read.
    #[test]
    fn an_unparseable_policy_is_shown_rather_than_swallowed() {
        let raw = r#"{"Policy":"not actually json"}"#;
        assert_eq!(
            parse_section(BucketSection::Policy, raw),
            SectionResult::Facts(BucketFacts::Document("not actually json".to_string()))
        );
    }

    #[test]
    fn tags_are_pairs_sorted_by_key() {
        let raw = r#"{"TagSet":[{"Key":"owner","Value":"team"},{"Key":"App","Value":"web"}]}"#;
        match parse_section(BucketSection::Tags, raw) {
            SectionResult::Facts(BucketFacts::Pairs(pairs)) => {
                assert_eq!(pairs[0].0, "App");
                assert_eq!(pairs[1].0, "owner");
            }
            other => panic!("got {other:?}"),
        }
    }

    /// Every section fills one part of one panel, so unreadable input is an
    /// empty section rather than a panic — and `Absent`, never `Failed`: a
    /// reply we could not read is not evidence of an error at AWS.
    #[test]
    fn every_section_parser_survives_rubbish() {
        for section in BucketSection::all() {
            assert_eq!(
                parse_section(section, "{"),
                SectionResult::Absent,
                "{section:?} must degrade to Absent"
            );
        }
    }

    /// Public access is rendered first. It is the question somebody opens a
    /// bucket to answer, and burying it under versioning and tags is how a
    /// world-readable bucket goes unnoticed.
    #[test]
    fn public_access_is_the_first_section() {
        assert_eq!(BucketSection::all()[0], BucketSection::PublicAccess);
    }

    /// Each section is its own `s3api` subcommand, and no two share one.
    #[test]
    fn every_section_has_its_own_command() {
        let mut seen = Vec::new();
        for section in BucketSection::all() {
            let cmd = section.command();
            assert!(cmd.starts_with("get-"), "{section:?} -> {cmd}");
            assert!(!seen.contains(&cmd), "{cmd} is claimed twice");
            seen.push(cmd);
        }
        assert_eq!(seen.len(), 8);
    }
}

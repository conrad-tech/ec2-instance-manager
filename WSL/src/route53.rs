//! Route 53: the hosted zones the sub-tab lists, and each zone's records.
//!
//! Parsing is pure and tested against captured payloads; the `fetch_*`
//! functions are thin wrappers over `resources::run_cli`. Same split as
//! `elb.rs`, `asg.rs` and `s3.rs`.
//!
//! Three things about this API shape everything below:
//!
//! - **It is a global service**, like S3, so the cache key carries no region
//!   (`ResourceKind::is_global`) — and the same consequence follows: one
//!   account checked in two regions produces one key, not two.
//! - **Every name comes back with a trailing dot and octal escapes.**
//!   `example.com.` and `\052.example.com.` are the API's spellings of
//!   `example.com` and `*.example.com`. A wildcard record rendered raw reads
//!   as a parse failure, and it is the record people go looking for.
//! - **An alias record has no TTL and no `ResourceRecords` at all.** It
//!   carries an `AliasTarget` instead. Reading only `ResourceRecords` renders
//!   every alias as an empty row — and an alias is how a load balancer gets
//!   pointed at, so that is most of the interesting records in a real zone.

use crate::resources::run_cli as run;

pub use crate::resources::FetchError;

/// One hosted zone, as the Route 53 sub-tab lists it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HostedZone {
    /// The bare id — `Z1234567890ABC`, never `/hostedzone/Z1234567890ABC`.
    /// See [`strip_zone_prefix`].
    pub id: String,
    /// The zone name with its trailing dot removed and its escapes decoded.
    pub name: String,
    pub private: bool,
    pub record_count: i64,
    pub comment: Option<String>,
    /// Set when the zone is managed by another AWS service (a Service
    /// Discovery namespace, say). Such a zone is not one a human edits, and
    /// saying so is cheaper than someone working that out from its records.
    pub linked_service: Option<String>,
    /// Stamped in by the caller from the AWS context, as every other resource
    /// type's is.
    pub account_id: String,
}

/// `/hostedzone/Z1234567890ABC` -> `Z1234567890ABC`.
///
/// The API returns the prefixed form in `list-hosted-zones` and accepts
/// either form as an argument, so this is about what a human reads and
/// copies: the bare id is what appears in the console, in a Terraform state
/// and in every other tool, and a search for `Z1234567890ABC` has to find it.
pub fn strip_zone_prefix(raw: &str) -> String {
    raw.rsplit('/').next().unwrap_or(raw).to_string()
}

/// Decode Route 53's octal escapes.
///
/// The API returns any character outside printable ASCII — **and `*` and `@`,
/// which are printable** — as a backslash followed by three octal digits. So
/// a wildcard record arrives as `\052.example.com.`, and rendered raw it
/// reads as a parse failure. It is also exactly the record somebody is
/// looking for when they open a zone.
///
/// A backslash not followed by three octal digits is kept verbatim rather
/// than swallowed: this is a name, and dropping a character from it would be
/// worse than showing an odd one.
pub fn decode_dns_name(raw: &str) -> String {
    let mut out = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let mut digits = String::new();
        while digits.len() < 3 {
            match chars.peek() {
                Some(d) if d.is_digit(8) => {
                    digits.push(*d);
                    chars.next();
                }
                _ => break,
            }
        }
        match u8::from_str_radix(&digits, 8) {
            Ok(byte) if digits.len() == 3 => out.push(byte as char),
            _ => {
                out.push('\\');
                out.push_str(&digits);
            }
        }
    }
    out
}

/// Decode the escapes and drop the trailing dot.
///
/// Every name in this API is fully qualified, so every one of them ends in a
/// dot. Keeping it puts a stray `.` on the end of every row for no
/// information at all — nobody types or says it — and it is still matched by
/// a search, because the haystack carries the raw form too.
///
/// The root itself (`.`) keeps its dot: stripping it leaves an empty string,
/// which renders as a missing name rather than as the root.
pub fn display_name(raw: &str) -> String {
    let decoded = decode_dns_name(raw);
    match decoded.strip_suffix('.') {
        Some(trimmed) if !trimmed.is_empty() => trimmed.to_string(),
        _ => decoded,
    }
}

/// Read `route53 list-hosted-zones --output json`.
///
/// An unreadable payload yields no zones rather than an error, for the reason
/// every other list parser here does: this fills one table, and the caller
/// tells an empty account from a failed call by whether the *fetch* returned
/// `Err`, never by an empty list.
pub fn parse_hosted_zones(raw: &str) -> Vec<HostedZone> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    let Some(entries) = value.get("HostedZones").and_then(|v| v.as_array()) else {
        return Vec::new();
    };

    let mut out: Vec<HostedZone> = entries
        .iter()
        .filter_map(|z| {
            Some(HostedZone {
                id: strip_zone_prefix(&str_field(z, "Id")?),
                name: display_name(&str_field(z, "Name")?),
                private: z
                    .get("Config")
                    .and_then(|c| c.get("PrivateZone"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                record_count: z
                    .get("ResourceRecordSetCount")
                    .and_then(|v| v.as_i64())
                    .unwrap_or_default(),
                comment: z.get("Config").and_then(|c| str_field(c, "Comment")),
                linked_service: z
                    .get("LinkedService")
                    .and_then(|s| str_field(s, "ServicePrincipal")),
                account_id: String::new(),
            })
        })
        .collect();

    // The API's order is not stable between calls. Sorted by name, with the
    // id as the tiebreak: two zones can share a name — one public and one
    // private for the same domain is a normal split-horizon setup — and
    // without a tiebreak those two swap places between visits.
    out.sort_by(|a, b| {
        a.name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    out
}

/// `Private` or `Public`.
pub fn zone_kind_label(zone: &HostedZone) -> String {
    if zone.private { "Private" } else { "Public" }.to_string()
}

/// The whole haystack the search box filters a zone row on.
///
/// **Every column the table shows must appear here**, plus the comment and
/// the account, which the table dropped. The name appears **twice** — once as
/// displayed and once with its trailing dot — so a search for either
/// `example.com` or `example.com.` finds the row.
pub fn hosted_zone_searchable_text(z: &HostedZone) -> String {
    let mut out = String::new();
    let mut push = |s: &str| {
        out.push_str(&s.to_ascii_lowercase());
        out.push('\n');
    };
    push(&z.name);
    push(&format!("{}.", z.name));
    push(&z.id);
    push(&zone_kind_label(z));
    push(&z.record_count.to_string());
    push(&z.account_id);
    if let Some(comment) = &z.comment {
        push(comment);
    }
    if let Some(service) = &z.linked_service {
        push(service);
    }
    out
}

// ----------------------------------------------------------- record sets

/// One record set in a zone.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecordSet {
    /// Decoded and with the trailing dot dropped.
    pub name: String,
    pub kind: String,
    /// `None` on an alias record, which has no TTL at all.
    pub ttl: Option<i64>,
    /// The plain record values. Empty on an alias.
    pub values: Vec<String>,
    /// Where an alias points, decoded like every other name.
    pub alias_target: Option<String>,
    /// Whether an alias evaluates the target's health.
    pub alias_health: bool,
    /// Present only on a record that is part of a routing policy — and it is
    /// what tells two records with the same name and type apart.
    pub set_identifier: Option<String>,
    /// A sentence for whichever routing policy this record uses.
    pub routing: Option<String>,
    pub health_check_id: Option<String>,
}

/// One page of a zone's records, and whether there were more.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecordPage {
    pub records: Vec<RecordSet>,
    /// True when the read was cut short by [`RECORD_LIMIT`]. Surfaced rather
    /// than swallowed: a panel silently showing the first 500 of 4000 records
    /// is a panel that answers "is this name in the zone?" wrongly.
    pub truncated: bool,
}

/// Read `route53 list-resource-record-sets --output json`.
pub fn parse_record_sets(raw: &str) -> RecordPage {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return RecordPage::default();
    };
    // The raw API says `IsTruncated`; the CLI's own paginator says
    // `NextToken` when `--max-items` cut it short. Both mean the same thing
    // here and a build could see either.
    let truncated = value
        .get("IsTruncated")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        || value.get("NextToken").is_some();
    let Some(entries) = value.get("ResourceRecordSets").and_then(|v| v.as_array()) else {
        return RecordPage {
            records: Vec::new(),
            truncated,
        };
    };

    let records = entries
        .iter()
        .filter_map(|r| {
            let alias = r.get("AliasTarget");
            Some(RecordSet {
                name: display_name(&str_field(r, "Name")?),
                kind: str_field(r, "Type")?,
                ttl: r.get("TTL").and_then(|v| v.as_i64()),
                values: r
                    .get("ResourceRecords")
                    .and_then(|v| v.as_array())
                    .map(|rows| rows.iter().filter_map(|v| str_field(v, "Value")).collect())
                    .unwrap_or_default(),
                alias_target: alias
                    .and_then(|a| str_field(a, "DNSName"))
                    .map(|n| display_name(&n)),
                alias_health: alias
                    .and_then(|a| a.get("EvaluateTargetHealth"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                set_identifier: str_field(r, "SetIdentifier"),
                routing: routing_label(r),
                health_check_id: str_field(r, "HealthCheckId"),
            })
        })
        .collect();

    RecordPage { records, truncated }
}

/// Which routing policy a record uses, as a sentence.
///
/// The five policies live in five unrelated fields, so there is no one field
/// to render — and a record showing only its `SetIdentifier` says nothing
/// about *why* there are three records with the same name.
fn routing_label(r: &serde_json::Value) -> Option<String> {
    if let Some(weight) = r.get("Weight").and_then(|v| v.as_i64()) {
        return Some(format!("weighted {weight}"));
    }
    if let Some(failover) = str_field(r, "Failover") {
        return Some(format!("failover {failover}"));
    }
    if let Some(region) = str_field(r, "Region") {
        return Some(format!("latency {region}"));
    }
    if let Some(geo) = r.get("GeoLocation") {
        let parts: Vec<String> = ["ContinentCode", "CountryCode", "SubdivisionCode"]
            .iter()
            .filter_map(|k| str_field(geo, k))
            .collect();
        return Some(if parts.is_empty() {
            "geolocation".to_string()
        } else {
            format!("geolocation {}", parts.join("-"))
        });
    }
    if r.get("MultiValueAnswer").and_then(|v| v.as_bool()) == Some(true) {
        return Some("multivalue".to_string());
    }
    None
}

/// The TTL cell's text.
///
/// **An alias record has no TTL**, and says so in words rather than showing a
/// dash. A dash in a column whose other values are numbers reads as "we could
/// not read it"; `alias` explains why there is nothing there, and is the same
/// answer the console gives.
pub fn ttl_label(r: &RecordSet) -> String {
    match r.ttl {
        Some(ttl) => format!("{ttl}"),
        None if r.alias_target.is_some() => "alias".to_string(),
        None => "—".to_string(),
    }
}

/// What a record actually points at.
///
/// An alias is rendered as `ALIAS -> <target>` rather than as a bare name,
/// because the difference matters: an alias is resolved by Route 53 at query
/// time and is free, and a CNAME to the same target is neither.
pub fn record_value(r: &RecordSet) -> String {
    if let Some(target) = &r.alias_target {
        let mut out = format!("ALIAS -> {target}");
        if r.alias_health {
            out.push_str(" (evaluates target health)");
        }
        return out;
    }
    if r.values.is_empty() {
        // Neither an alias nor any values: a shape this does not understand.
        // Said plainly rather than left blank, since a blank value cell reads
        // as a record pointing at nothing.
        return "no value".to_string();
    }
    r.values.join("\n")
}

/// The whole haystack the detail view's own filter matches a record on.
///
/// The zone list's search box filters *zones*; a zone can hold thousands of
/// records, so the records table has its own filter — the same arrangement
/// the target group detail view uses for its targets.
pub fn record_searchable_text(r: &RecordSet) -> String {
    let mut out = String::new();
    let mut push = |s: &str| {
        out.push_str(&s.to_ascii_lowercase());
        out.push('\n');
    };
    push(&r.name);
    push(&r.kind);
    push(&record_value(r));
    push(&ttl_label(r));
    for v in &r.values {
        push(v);
    }
    if let Some(target) = &r.alias_target {
        push(target);
    }
    if let Some(id) = &r.set_identifier {
        push(id);
    }
    if let Some(routing) = &r.routing {
        push(routing);
    }
    if let Some(hc) = &r.health_check_id {
        push(hc);
    }
    out
}

// ------------------------------------------------------------ zone detail

/// What `get-hosted-zone` adds to what the list already said.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ZoneDetail {
    /// The zone's authoritative name servers. Public zones only — these are
    /// what a registrar is given.
    pub name_servers: Vec<String>,
    /// `(region, vpc id)` for a private zone, which has no name servers.
    pub vpcs: Vec<(String, String)>,
}

/// Read `route53 get-hosted-zone --output json`.
pub fn parse_zone_detail(raw: &str) -> ZoneDetail {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return ZoneDetail::default();
    };
    ZoneDetail {
        name_servers: value
            .get("DelegationSet")
            .and_then(|d| d.get("NameServers"))
            .and_then(|n| n.as_array())
            .map(|ns| {
                ns.iter()
                    .filter_map(|v| v.as_str())
                    .map(display_name)
                    .collect()
            })
            .unwrap_or_default(),
        vpcs: value
            .get("VPCs")
            .and_then(|v| v.as_array())
            .map(|vpcs| {
                vpcs.iter()
                    .filter_map(|v| {
                        Some((
                            str_field(v, "VPCRegion").unwrap_or_default(),
                            str_field(v, "VPCId")?,
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// Read `route53 list-tags-for-resource --output json`.
///
/// The tags are nested under `ResourceTagSet`, not at the top level — this
/// is not the shape `elb`'s `describe-tags` uses, and reading it flat yields
/// no tags on every zone.
pub fn parse_zone_tags(raw: &str) -> Vec<(String, String)> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    let Some(tags) = value
        .get("ResourceTagSet")
        .and_then(|s| s.get("Tags"))
        .and_then(|t| t.as_array())
    else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = tags
        .iter()
        .filter_map(|t| Some((str_field(t, "Key")?, str_field(t, "Value").unwrap_or_default())))
        .collect();
    out.sort_by(|a, b| a.0.to_ascii_lowercase().cmp(&b.0.to_ascii_lowercase()));
    out
}

// ---------------------------------------------------------------- fetching

fn str_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

/// Every hosted zone in one account.
///
/// **Route 53 is global**, like S3: this returns the same list whichever
/// region the CLI is pointed at, and the region argument only decides which
/// endpoint the request goes to.
pub fn fetch_hosted_zones(
    profile: &str,
    region: &str,
    account_id: &str,
) -> std::result::Result<Vec<HostedZone>, FetchError> {
    let raw = run(
        profile,
        region,
        &["route53", "list-hosted-zones", "--output", "json"],
    )?;
    let mut zones = parse_hosted_zones(&raw);
    for zone in &mut zones {
        zone.account_id = account_id.to_string();
    }
    Ok(zones)
}

/// How many records the detail view reads.
///
/// A busy zone holds thousands, and every one of them would be a row in an
/// egui grid inside one scroll area. Bounded at the call rather than in the
/// renderer, so the reply is bounded too — and the panel says when it was cut
/// short, because a table silently showing the first 500 of 4000 answers
/// "is this name in the zone?" wrongly.
pub const RECORD_LIMIT: usize = 500;

/// One zone's records, up to [`RECORD_LIMIT`].
pub fn fetch_record_sets(
    profile: &str,
    region: &str,
    zone_id: &str,
) -> std::result::Result<RecordPage, FetchError> {
    let limit = RECORD_LIMIT.to_string();
    let raw = run(
        profile,
        region,
        &[
            "route53",
            "list-resource-record-sets",
            "--hosted-zone-id",
            zone_id,
            "--max-items",
            &limit,
            "--output",
            "json",
        ],
    )?;
    Ok(parse_record_sets(&raw))
}

/// One zone's name servers, or its VPCs when it is private.
pub fn fetch_zone_detail(
    profile: &str,
    region: &str,
    zone_id: &str,
) -> std::result::Result<ZoneDetail, FetchError> {
    let raw = run(
        profile,
        region,
        &["route53", "get-hosted-zone", "--id", zone_id, "--output", "json"],
    )?;
    Ok(parse_zone_detail(&raw))
}

/// One zone's tags.
pub fn fetch_zone_tags(
    profile: &str,
    region: &str,
    zone_id: &str,
) -> std::result::Result<Vec<(String, String)>, FetchError> {
    let raw = run(
        profile,
        region,
        &[
            "route53",
            "list-tags-for-resource",
            "--resource-type",
            "hostedzone",
            "--resource-id",
            zone_id,
            "--output",
            "json",
        ],
    )?;
    Ok(parse_zone_tags(&raw))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZONES_JSON: &str = r#"{
      "HostedZones": [
        {
          "Id": "/hostedzone/Z0987654321XYZ",
          "Name": "internal.example.com.",
          "CallerReference": "x",
          "Config": { "Comment": "split horizon", "PrivateZone": true },
          "ResourceRecordSetCount": 42
        },
        {
          "Id": "/hostedzone/Z1234567890ABC",
          "Name": "example.com.",
          "CallerReference": "y",
          "Config": { "PrivateZone": false },
          "ResourceRecordSetCount": 12
        }
      ],
      "IsTruncated": false,
      "MaxItems": "100"
    }"#;

    fn zones() -> Vec<HostedZone> {
        parse_hosted_zones(ZONES_JSON)
    }

    /// The bare id is what the console shows, what Terraform state holds, and
    /// what somebody pastes into a search box. The API's `/hostedzone/`
    /// prefix is addressing, not identity.
    #[test]
    fn the_zone_id_loses_its_api_prefix() {
        assert_eq!(strip_zone_prefix("/hostedzone/Z1234567890ABC"), "Z1234567890ABC");
        // Already bare, which is how it comes back from other calls.
        assert_eq!(strip_zone_prefix("Z1234567890ABC"), "Z1234567890ABC");
        assert_eq!(zones()[0].id, "Z1234567890ABC");
    }

    /// Every name in this API is fully qualified, so keeping the dot puts a
    /// stray `.` on every row for no information at all.
    #[test]
    fn a_zone_name_drops_its_trailing_dot() {
        assert_eq!(zones()[0].name, "example.com");
        assert_eq!(zones()[1].name, "internal.example.com");
    }

    /// The root keeps its dot: stripping it leaves an empty string, which
    /// renders as a missing name rather than as the root.
    #[test]
    fn the_root_keeps_its_dot() {
        assert_eq!(display_name("."), ".");
    }

    /// **The wildcard trap.** Route 53 returns `*` as `\052`, so a wildcard
    /// record rendered raw reads as a parse failure — and it is exactly the
    /// record somebody opens a zone to find.
    #[test]
    fn a_wildcard_record_is_decoded_rather_than_shown_as_an_escape() {
        assert_eq!(display_name("\\052.example.com."), "*.example.com");
        assert_eq!(decode_dns_name("\\100"), "@");
    }

    /// A backslash that is not an escape is kept verbatim. This is a name;
    /// dropping a character from it is worse than showing an odd one.
    #[test]
    fn a_backslash_that_is_not_an_escape_survives() {
        assert_eq!(decode_dns_name("a\\9b"), "a\\9b");
        assert_eq!(decode_dns_name("trailing\\"), "trailing\\");
        // Only three digits are taken, so the fourth stays part of the name.
        assert_eq!(decode_dns_name("\\0525"), "*5");
    }

    #[test]
    fn a_zone_reads_its_kind_comment_and_count() {
        let zs = zones();
        assert!(!zs[0].private);
        assert_eq!(zone_kind_label(&zs[0]), "Public");
        assert_eq!(zs[0].record_count, 12);
        assert!(zs[1].private);
        assert_eq!(zone_kind_label(&zs[1]), "Private");
        assert_eq!(zs[1].comment.as_deref(), Some("split horizon"));
    }

    /// Sorted by name, and the id breaks the tie. Two zones CAN share a name
    /// — one public and one private for the same domain is an ordinary
    /// split-horizon setup — and without the tiebreak those two swap places
    /// between visits.
    #[test]
    fn zones_sort_by_name_then_id() {
        let raw = r#"{"HostedZones":[
          {"Id":"/hostedzone/ZBBB","Name":"example.com.","Config":{"PrivateZone":true}},
          {"Id":"/hostedzone/ZAAA","Name":"example.com.","Config":{"PrivateZone":false}}
        ]}"#;
        let zs = parse_hosted_zones(raw);
        assert_eq!(zs[0].id, "ZAAA");
        assert_eq!(zs[1].id, "ZBBB");
    }

    #[test]
    fn unreadable_input_yields_no_zones() {
        assert!(parse_hosted_zones("not json").is_empty());
        assert!(parse_hosted_zones(r#"{"Something":[]}"#).is_empty());
    }

    /// Every column the table shows has to be searchable, and the name has to
    /// be findable with OR without its trailing dot.
    #[test]
    fn the_zone_haystack_carries_every_column_and_both_spellings_of_the_name() {
        let mut z = zones()[0].clone();
        z.account_id = "111122223333".to_string();
        let hay = hosted_zone_searchable_text(&z);
        for needle in [
            "example.com",
            "example.com.",
            "z1234567890abc",
            "public",
            "12",
            "111122223333",
        ] {
            assert!(hay.contains(needle), "missing {needle}: {hay}");
        }
        assert_eq!(hay, hay.to_ascii_lowercase());
    }

    // ------------------------------------------------------------ records

    const RECORDS_JSON: &str = r#"{
      "ResourceRecordSets": [
        { "Name": "example.com.", "Type": "A",
          "AliasTarget": { "HostedZoneId": "Z35SXDOTRQ7X7K",
            "DNSName": "alpha-alb-50dc6c49.us-east-1.elb.amazonaws.com.",
            "EvaluateTargetHealth": true } },
        { "Name": "\\052.example.com.", "Type": "CNAME", "TTL": 300,
          "ResourceRecords": [ { "Value": "example.com" } ] },
        { "Name": "api.example.com.", "Type": "A", "SetIdentifier": "east",
          "Weight": 90, "TTL": 60, "HealthCheckId": "hc-123",
          "ResourceRecords": [ { "Value": "1.2.3.4" } ] },
        { "Name": "api.example.com.", "Type": "A", "SetIdentifier": "west",
          "Weight": 10, "TTL": 60,
          "ResourceRecords": [ { "Value": "5.6.7.8" } ] },
        { "Name": "db.example.com.", "Type": "A", "SetIdentifier": "dr",
          "Failover": "SECONDARY", "TTL": 60,
          "ResourceRecords": [ { "Value": "9.9.9.9" } ] }
      ],
      "IsTruncated": false
    }"#;

    fn records() -> Vec<RecordSet> {
        parse_record_sets(RECORDS_JSON).records
    }

    /// **An alias has no TTL and no `ResourceRecords`.** Reading only
    /// `ResourceRecords` renders it as an empty row — and an alias is how a
    /// load balancer gets pointed at, so that is most of the interesting
    /// records in a real zone.
    #[test]
    fn an_alias_record_renders_its_target_rather_than_an_empty_row() {
        let alias = &records()[0];
        assert!(alias.values.is_empty());
        assert_eq!(alias.ttl, None);
        let value = record_value(alias);
        assert!(value.starts_with("ALIAS -> alpha-alb"), "{value}");
        assert!(value.contains("evaluates target health"), "{value}");
        // And the TTL cell explains why there is nothing in it, rather than
        // showing a dash that reads as "we could not read it".
        assert_eq!(ttl_label(alias), "alias");
    }

    #[test]
    fn a_plain_record_renders_its_values_and_ttl() {
        let cname = &records()[1];
        assert_eq!(cname.name, "*.example.com");
        assert_eq!(record_value(cname), "example.com");
        assert_eq!(ttl_label(cname), "300");
    }

    /// A record with neither an alias nor values is a shape this does not
    /// understand. Said plainly, because a blank value cell reads as a record
    /// pointing at nothing.
    #[test]
    fn a_record_with_nothing_in_it_says_so() {
        assert_eq!(record_value(&RecordSet::default()), "no value");
        assert_eq!(ttl_label(&RecordSet::default()), "—");
    }

    /// The five routing policies live in five unrelated fields, so a record
    /// showing only its `SetIdentifier` says nothing about *why* there are
    /// three records with one name.
    #[test]
    fn each_routing_policy_is_named() {
        let rs = records();
        assert_eq!(rs[2].routing.as_deref(), Some("weighted 90"));
        assert_eq!(rs[2].set_identifier.as_deref(), Some("east"));
        assert_eq!(rs[2].health_check_id.as_deref(), Some("hc-123"));
        assert_eq!(rs[3].routing.as_deref(), Some("weighted 10"));
        assert_eq!(rs[4].routing.as_deref(), Some("failover SECONDARY"));
        // A simple record has no policy, and must not be given one.
        assert_eq!(rs[1].routing, None);
    }

    #[test]
    fn latency_geolocation_and_multivalue_are_read_too() {
        let raw = r#"{"ResourceRecordSets":[
          {"Name":"a.example.com.","Type":"A","SetIdentifier":"lat","Region":"eu-west-2"},
          {"Name":"b.example.com.","Type":"A","SetIdentifier":"geo",
           "GeoLocation":{"CountryCode":"US","SubdivisionCode":"CA"}},
          {"Name":"c.example.com.","Type":"A","SetIdentifier":"mv","MultiValueAnswer":true},
          {"Name":"d.example.com.","Type":"A","SetIdentifier":"any","GeoLocation":{}}
        ]}"#;
        let rs = parse_record_sets(raw).records;
        assert_eq!(rs[0].routing.as_deref(), Some("latency eu-west-2"));
        assert_eq!(rs[1].routing.as_deref(), Some("geolocation US-CA"));
        assert_eq!(rs[2].routing.as_deref(), Some("multivalue"));
        assert_eq!(rs[3].routing.as_deref(), Some("geolocation"));
    }

    /// A zone bigger than the cap must SAY it was cut short. A table silently
    /// showing the first 500 of 4000 answers "is this name in the zone?"
    /// wrongly.
    #[test]
    fn a_truncated_record_read_reports_itself() {
        assert!(!parse_record_sets(RECORDS_JSON).truncated);
        // The raw API's spelling.
        assert!(parse_record_sets(r#"{"ResourceRecordSets":[],"IsTruncated":true}"#).truncated);
        // The CLI paginator's spelling, which is what `--max-items` produces.
        assert!(parse_record_sets(r#"{"ResourceRecordSets":[],"NextToken":"abc"}"#).truncated);
    }

    /// A zone can hold thousands of records, so the detail view has its own
    /// filter — the arrangement the target group detail view already uses for
    /// its targets. Everything the records table shows has to be in it.
    #[test]
    fn the_record_haystack_carries_everything_the_row_shows() {
        let rs = records();
        let alias_hay = record_searchable_text(&rs[0]);
        assert!(alias_hay.contains("alpha-alb"), "{alias_hay}");
        assert!(alias_hay.contains("alias"), "{alias_hay}");

        let weighted = record_searchable_text(&rs[2]);
        for needle in ["api.example.com", "1.2.3.4", "east", "weighted 90", "hc-123"] {
            assert!(weighted.contains(needle), "missing {needle}: {weighted}");
        }
        assert_eq!(weighted, weighted.to_ascii_lowercase());
    }

    /// The escapes are decoded once, at parse time, so the filter matches
    /// what the row actually shows. Searching `*.example.com` has to find the
    /// wildcard row.
    #[test]
    fn the_record_filter_matches_the_decoded_name() {
        let hay = record_searchable_text(&records()[1]);
        assert!(hay.contains("*.example.com"), "{hay}");
        assert!(!hay.contains("\\052"), "{hay}");
    }

    // ------------------------------------------------------- zone detail

    #[test]
    fn a_public_zone_reads_its_name_servers() {
        let raw = r#"{"HostedZone":{"Id":"/hostedzone/Z1","Name":"example.com."},
          "DelegationSet":{"NameServers":["ns-1.awsdns-01.com","ns-2.awsdns-02.net"]}}"#;
        let d = parse_zone_detail(raw);
        assert_eq!(d.name_servers, vec!["ns-1.awsdns-01.com", "ns-2.awsdns-02.net"]);
        assert!(d.vpcs.is_empty());
    }

    /// A private zone has VPCs and no name servers. Both halves matter: an
    /// empty name-server list on a private zone is correct, not a failure.
    #[test]
    fn a_private_zone_reads_its_vpcs_instead() {
        let raw = r#"{"HostedZone":{"Id":"/hostedzone/Z2"},
          "VPCs":[{"VPCRegion":"us-east-1","VPCId":"vpc-3ac0fb5f"}]}"#;
        let d = parse_zone_detail(raw);
        assert!(d.name_servers.is_empty());
        assert_eq!(
            d.vpcs,
            vec![("us-east-1".to_string(), "vpc-3ac0fb5f".to_string())]
        );
    }

    /// Zone tags are nested under `ResourceTagSet`, unlike ELB's flat
    /// `describe-tags`. Reading it flat yields no tags on every single zone.
    #[test]
    fn zone_tags_are_read_from_their_nested_home() {
        let raw = r#"{"ResourceTagSet":{"ResourceType":"hostedzone","ResourceId":"Z1",
          "Tags":[{"Key":"owner","Value":"team"},{"Key":"App","Value":"web"}]}}"#;
        let tags = parse_zone_tags(raw);
        assert_eq!(tags[0], ("App".to_string(), "web".to_string()));
        assert_eq!(tags[1], ("owner".to_string(), "team".to_string()));
        // A flat read must not accidentally work, or the nesting is untested.
        assert!(parse_zone_tags(r#"{"Tags":[{"Key":"k","Value":"v"}]}"#).is_empty());
    }

    /// Every parser fills one part of one panel, so unreadable input is an
    /// empty section rather than a panic.
    #[test]
    fn every_detail_parser_survives_rubbish() {
        assert_eq!(parse_record_sets("{"), RecordPage::default());
        assert_eq!(parse_zone_detail("nope"), ZoneDetail::default());
        assert!(parse_zone_tags("").is_empty());
    }
}

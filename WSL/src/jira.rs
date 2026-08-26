//! The Jira issue API — the tickets behind the **Jira Tickets** button.
//!
//! This is a *different API* from the JSM Operations alert feed in
//! `src/alerts.rs`, reached with the *same* credentials: the Atlassian email,
//! API token and cloud id already resolved once at startup for Alerts
//! (`jsm_auth::load_auth`). `assets/scripts/oncall_probe.sh` has read
//! `https://api.atlassian.com/ex/jira/<cloud_id>/rest/api/3/myself` with that
//! token since before this module existed, so nothing new needs configuring
//! and no new Credential Manager entry is involved. That is also why
//! [`AlertsAuth`] is reused rather than a near-identical `JiraAuth` being
//! declared: one resolution, one value, no second thing to keep in step.
//!
//! Things here that are load-bearing:
//!
//! - **The site is configurable, and it is not necessarily the alerts one.**
//!   The JSM Ops feed is addressed by cloud id through
//!   `api.atlassian.com/ex/jira/<cloud_id>`; a site can equally be addressed
//!   by its own domain, and an org may run Jira somewhere other than the
//!   tenant the alert feed lives in. [`resolve_base_url`] layers
//!   `JIRA_BASE_URL` → `features.json` `jira.base_url` → the cloud-id form,
//!   so an unset build behaves exactly as before.
//! - **API v3, and search is `POST /search/jql`.** Atlassian removed the old
//!   unsuffixed `/search`, and the v2 spelling of the replacement is not
//!   dependable — a real tenant served v3 and only v3. POST rather than GET
//!   because that is the form proven against that tenant, and it keeps a long
//!   JQL out of a URL.
//! - **v3 means the description arrives as ADF**, a JSON node tree rather
//!   than text, so [`description_text`] flattens it. It also accepts a plain
//!   string, because that is what v2 returns and a site may still answer that
//!   way — one function, either payload, rather than a version to keep track
//!   of.
//! - **The issue key and the transition id are whitelisted, not escaped.**
//!   The key is interpolated into a URL path and the transition id into a
//!   JSON body, so both are checked against a strict shape and refused
//!   otherwise — the same stance `alerts::validate_alert_id` and
//!   `vault_iam` take.
//! - **Every parser is a pure function over `&str`.** The fetches are thin
//!   wrappers around them, so the whole of the parsing — including the
//!   fields Jira routinely returns as `null` — is tested without a network.

use chrono::{DateTime, Local, TimeZone};
use serde_json::Value;

use crate::alerts::AlertsAuth;
use crate::atlassian_http;
use crate::error::{AppError, Result};

/// Issues per search. The list window is a working set, not an inbox to
/// page through — if this ever truncates, the answer is a narrower JQL.
const MAX_RESULTS: u32 = 50;

/// What "my open tickets" means. `currentUser()` resolves server-side from
/// the token, so no account id has to be configured for this to work — which
/// is the whole reason it is written this way rather than with an
/// `accountId = …` clause.
pub const DEFAULT_JQL: &str =
    "assignee = currentUser() AND statusCategory != Done ORDER BY updated DESC";

/// Environment override for the Jira site. Layered above the `features.json`
/// value so a domain need not be committed to git to be used.
pub const JIRA_BASE_URL_ENV: &str = "JIRA_BASE_URL";

/// Fields the list needs. Deliberately not every field: the list renders six
/// columns and a search that drags every custom field on every ticket is
/// slower for nothing.
const LIST_FIELDS: &[&str] =
    &["summary", "status", "issuetype", "priority", "updated", "project"];

/// Fields the ticket view needs. `description` is here and absent from
/// [`LIST_FIELDS`] on purpose — it is the largest field on most tickets and
/// the list never shows it.
const ISSUE_FIELDS: &str =
    "summary,description,status,issuetype,priority,assignee,reporter,created,updated,labels";

/// Where the Jira issue API is, and what to authenticate to it with.
///
/// Resolved **once** at startup and passed around, for the same reason
/// `App::alerts_auth` is: the credentials behind it are a `CredReadW` per
/// field on Windows.
#[derive(Clone, Debug, Default)]
pub struct JiraSite {
    auth: AlertsAuth,
    /// Fully-qualified `…/rest/api/3`, no trailing slash.
    api_base: String,
}

impl JiraSite {
    /// `configured` is `features.json`'s `jira.base_url`; `env` is
    /// [`JIRA_BASE_URL_ENV`]. Either may be blank.
    pub fn new(auth: AlertsAuth, configured: &str, env: Option<String>) -> Self {
        let api_base = resolve_base_url(configured, env, &auth.cloud_id);
        Self { auth, api_base }
    }

    /// True when a request could be made — credentials present *and* a site
    /// to send them to.
    pub fn is_complete(&self) -> bool {
        self.auth.is_complete() && !self.api_base.is_empty()
    }

    /// The resolved `…/rest/api/3` base, for the startup log line.
    pub fn api_base(&self) -> &str {
        &self.api_base
    }
}

/// Work out the `…/rest/api/3` base for the issue API.
///
/// `JIRA_BASE_URL` wins, then `features.json`'s `jira.base_url`, then the
/// cloud-id form the alert feed uses — so a build that configures nothing
/// behaves exactly as it did before the field existed.
///
/// Input is forgiving on purpose: this is pasted by a human from a browser
/// bar or a curl command, so a bare host, a trailing slash, and a URL that
/// already carries `/rest/api/3` (or `/rest/api/2`) all resolve to the same
/// thing. A blank cloud id with no configured site yields an empty string,
/// which `is_complete` reports rather than building a request against
/// nowhere.
pub fn resolve_base_url(configured: &str, env: Option<String>, cloud_id: &str) -> String {
    let picked = [env.as_deref().unwrap_or_default(), configured]
        .into_iter()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .unwrap_or_default()
        .to_string();

    if picked.is_empty() {
        let cloud_id = cloud_id.trim();
        if cloud_id.is_empty() {
            return String::new();
        }
        return format!("https://api.atlassian.com/ex/jira/{cloud_id}/rest/api/3");
    }

    let mut base = picked;
    // A scheme-less host is what someone types when copying a domain out of
    // a browser bar. https only: this carries a bearer-equivalent token.
    if !base.starts_with("http://") && !base.starts_with("https://") {
        base = format!("https://{base}");
    }
    base = base.trim_end_matches('/').to_string();
    // Someone pasting a working curl URL brings the path with it.
    for suffix in ["/rest/api/3/search/jql", "/rest/api/2/search/jql", "/rest/api/3", "/rest/api/2"]
    {
        if let Some(stripped) = base.strip_suffix(suffix) {
            base = stripped.trim_end_matches('/').to_string();
            break;
        }
    }
    format!("{base}/rest/api/3")
}

/// Flatten a description field to plain text.
///
/// v3 returns ADF — `{"type":"doc","content":[…]}` — where the text lives in
/// leaf nodes and the structure carries the line breaks. v2 returns a plain
/// string. Both arrive here, because which one a site sends is not worth
/// tracking in the caller, and `null` (a ticket with no description) is
/// neither.
///
/// This is a *reader*, not a renderer: it recovers the words and the shape of
/// the paragraphs, and deliberately drops formatting marks (bold, colour,
/// links' display styling) that egui is not being asked to reproduce. A link
/// keeps its URL, a mention keeps the name, since both are content rather
/// than decoration.
pub fn description_text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        // v2, or a site still answering the old way.
        Value::String(s) => s.trim().to_string(),
        _ => {
            let mut out = String::new();
            adf_node(v, &mut out);
            // Block nodes each add their own break, so nesting them stacks
            // blank lines that were never in the document.
            let mut collapsed = String::new();
            let mut blanks = 0usize;
            for line in out.lines() {
                if line.trim().is_empty() {
                    blanks += 1;
                    if blanks > 1 {
                        continue;
                    }
                } else {
                    blanks = 0;
                }
                collapsed.push_str(line.trim_end());
                collapsed.push('\n');
            }
            collapsed.trim().to_string()
        }
    }
}

/// One ADF node into `out`. Unknown node types recurse into their children
/// rather than being dropped: ADF gains node types over time, and losing a
/// paragraph because it sat inside a panel nobody had heard of is worse than
/// rendering it without its box.
fn adf_node(node: &Value, out: &mut String) {
    let node_type = node.get("type").and_then(Value::as_str).unwrap_or("");
    let attr = |k: &str| node.get("attrs").and_then(|a| a.get(k)).and_then(Value::as_str);

    match node_type {
        "text" => out.push_str(node.get("text").and_then(Value::as_str).unwrap_or("")),
        "hardBreak" => out.push('\n'),
        "rule" => out.push_str("\n---\n"),
        // Content, not decoration: dropping these loses the who and the where.
        "mention" => out.push_str(attr("text").unwrap_or("@unknown")),
        "emoji" => out.push_str(attr("text").or(attr("shortName")).unwrap_or("")),
        "inlineCard" | "blockCard" | "embedCard" => {
            out.push_str(attr("url").unwrap_or("[link]"))
        }
        "media" | "mediaSingle" | "mediaGroup" | "mediaInline" => {
            out.push_str(attr("alt").unwrap_or("[attachment]"));
            out.push('\n');
        }
        "listItem" => {
            out.push_str("• ");
            adf_children(node, out);
            if !out.ends_with('\n') {
                out.push('\n');
            }
        }
        "paragraph" | "heading" | "blockquote" | "codeBlock" => {
            adf_children(node, out);
            out.push_str("\n\n");
        }
        // "doc", "bulletList", "orderedList", "panel", "table", and whatever
        // ADF adds next.
        _ => adf_children(node, out),
    }
}

fn adf_children(node: &Value, out: &mut String) {
    if let Some(kids) = node.get("content").and_then(Value::as_array) {
        for kid in kids {
            adf_node(kid, out);
        }
    }
}

/// One row of the ticket list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IssueRow {
    pub key: String,
    pub summary: String,
    pub status: String,
    /// `statusCategory.key` — `new`, `indeterminate` or `done`. Drives the
    /// status colour, because status *names* are per-workflow free text and
    /// cannot be matched against.
    pub status_category: String,
    pub issue_type: String,
    pub priority: String,
    pub project: String,
    /// `updated` exactly as the API returned it. Rendered in local time.
    pub updated: String,
}

/// One ticket, as the detail window shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Issue {
    pub key: String,
    pub summary: String,
    /// Plain text — see the module note on API v2. Empty when the ticket has
    /// no description, which is common and is not an error.
    pub description: String,
    pub status: String,
    pub status_category: String,
    pub issue_type: String,
    pub priority: String,
    /// Display name, or empty for an unassigned ticket.
    pub assignee: String,
    pub reporter: String,
    pub created: String,
    pub updated: String,
    pub labels: Vec<String>,
}

/// One move this ticket can legally make right now, as Jira names it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Transition {
    pub id: String,
    /// The workflow's own label — "Start Progress", "Done", "Resolve
    /// Issue". Rendered verbatim on the button, because inventing our own
    /// wording for someone else's workflow is how a button ends up lying.
    pub name: String,
    /// The status the ticket lands in. Shown as hover text so a button
    /// named "Done" that actually moves to "Closed" says so.
    pub to_status: String,
}

fn require_complete(site: &JiraSite) -> Result<()> {
    if site.is_complete() {
        Ok(())
    } else {
        Err(AppError::InvalidArgument(
            "jira: needs an email, an API token, and a site (jira.base_url, \
             JIRA_BASE_URL, or a cloud id)"
                .to_string(),
        ))
    }
}

/// True for something shaped like `OPS-123`. Used by the search box to tell
/// a key from a typo before anything is sent.
pub fn looks_like_issue_key(s: &str) -> bool {
    validate_issue_key(s).is_ok()
}

/// The key is interpolated into a URL path, so it is checked against a
/// strict shape rather than escaped — the same stance as
/// `alerts::validate_alert_id`. Jira project keys are uppercase letters and
/// digits starting with a letter; the number after the hyphen is the issue
/// number.
pub fn validate_issue_key(key: &str) -> Result<()> {
    let key = key.trim();
    let Some((project, number)) = key.split_once('-') else {
        return Err(AppError::InvalidArgument(format!(
            "jira: '{key}' is not a ticket key (expected something like CATDO-123)"
        )));
    };
    let project_ok = !project.is_empty()
        && project.starts_with(|c: char| c.is_ascii_uppercase())
        && project.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
    let number_ok = !number.is_empty() && number.chars().all(|c| c.is_ascii_digit());
    if project_ok && number_ok {
        Ok(())
    } else {
        Err(AppError::InvalidArgument(format!(
            "jira: '{key}' is not a ticket key (expected something like CATDO-123)"
        )))
    }
}

/// The transition id goes into a JSON request body, so it is whitelisted for
/// the same reason the key is. Jira's transition ids are numeric strings.
fn validate_transition_id(id: &str) -> Result<()> {
    let id = id.trim();
    if !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) {
        Ok(())
    } else {
        Err(AppError::InvalidArgument(format!(
            "jira: refusing malformed transition id '{id}'"
        )))
    }
}

/// String at a dotted path, trimmed. Missing, `null` and non-string all
/// collapse to empty: an unassigned ticket and a project with no priority
/// field are ordinary states, not parse failures.
fn field_str(v: &Value, path: &[&str]) -> String {
    let mut cur = v;
    for step in path {
        match cur.get(step) {
            Some(next) => cur = next,
            None => return String::new(),
        }
    }
    cur.as_str().unwrap_or_default().trim().to_string()
}

/// Parse a `/search/jql` response.
///
/// A missing `issues` key is an error, but an **empty** one is not: no open
/// tickets is a real and common answer, and reporting it as a failure would
/// be a lie. The distinction matters — silently showing an empty list for a
/// response we could not read is precisely the failure mode the forwards.json
/// build check exists to prevent elsewhere in this repo.
pub fn parse_issue_list(body: &str) -> Result<Vec<IssueRow>> {
    let v: Value = serde_json::from_str(body)
        .map_err(|e| AppError::InvalidArgument(format!("jira: could not parse search: {e}")))?;
    let Some(issues) = v.get("issues").and_then(Value::as_array) else {
        return Err(AppError::InvalidArgument(
            "jira: search response carried no 'issues' list".to_string(),
        ));
    };
    Ok(issues
        .iter()
        .map(|i| IssueRow {
            key: field_str(i, &["key"]),
            summary: field_str(i, &["fields", "summary"]),
            status: field_str(i, &["fields", "status", "name"]),
            status_category: field_str(i, &["fields", "status", "statusCategory", "key"]),
            issue_type: field_str(i, &["fields", "issuetype", "name"]),
            priority: field_str(i, &["fields", "priority", "name"]),
            project: field_str(i, &["fields", "project", "key"]),
            updated: field_str(i, &["fields", "updated"]),
        })
        .collect())
}

/// Parse a single-issue response.
pub fn parse_issue(body: &str) -> Result<Issue> {
    let v: Value = serde_json::from_str(body)
        .map_err(|e| AppError::InvalidArgument(format!("jira: could not parse issue: {e}")))?;
    let key = field_str(&v, &["key"]);
    if key.is_empty() {
        return Err(AppError::InvalidArgument(
            "jira: issue response carried no key".to_string(),
        ));
    }
    let labels = v
        .get("fields")
        .and_then(|f| f.get("labels"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    Ok(Issue {
        key,
        summary: field_str(&v, &["fields", "summary"]),
        description: v
            .get("fields")
            .and_then(|f| f.get("description"))
            .map(description_text)
            .unwrap_or_default(),
        status: field_str(&v, &["fields", "status", "name"]),
        status_category: field_str(&v, &["fields", "status", "statusCategory", "key"]),
        issue_type: field_str(&v, &["fields", "issuetype", "name"]),
        priority: field_str(&v, &["fields", "priority", "name"]),
        assignee: field_str(&v, &["fields", "assignee", "displayName"]),
        reporter: field_str(&v, &["fields", "reporter", "displayName"]),
        created: field_str(&v, &["fields", "created"]),
        updated: field_str(&v, &["fields", "updated"]),
        labels,
    })
}

/// Parse a `/transitions` response.
///
/// A transition with no id is dropped rather than failing the lot: it could
/// not be actioned anyway, and one odd entry must not cost the ticket every
/// other button.
pub fn parse_transitions(body: &str) -> Result<Vec<Transition>> {
    let v: Value = serde_json::from_str(body).map_err(|e| {
        AppError::InvalidArgument(format!("jira: could not parse transitions: {e}"))
    })?;
    let Some(list) = v.get("transitions").and_then(Value::as_array) else {
        return Err(AppError::InvalidArgument(
            "jira: response carried no 'transitions' list".to_string(),
        ));
    };
    Ok(list
        .iter()
        .map(|t| Transition {
            id: field_str(t, &["id"]),
            name: field_str(t, &["name"]),
            to_status: field_str(t, &["to", "name"]),
        })
        .filter(|t| !t.id.is_empty())
        .collect())
}

/// Jira timestamps in local time, e.g. `2026-08-26 10:12 AM`.
///
/// Jira writes the offset without a colon (`+0100`), which
/// `DateTime::parse_from_rfc3339` rejects outright — so the RFC 3339 parse is
/// tried first (it is what the alert feed returns) and the Jira spelling
/// second. An unparseable stamp is returned verbatim: showing the raw string
/// is honest, showing a blank or an epoch date is not.
pub fn local_time(ts: &str) -> String {
    let ts = ts.trim();
    if ts.is_empty() {
        return String::new();
    }
    let parsed = DateTime::parse_from_rfc3339(ts)
        .or_else(|_| DateTime::parse_from_str(ts, "%Y-%m-%dT%H:%M:%S%.3f%z"))
        .or_else(|_| DateTime::parse_from_str(ts, "%Y-%m-%dT%H:%M:%S%z"));
    match parsed {
        Ok(dt) => Local
            .from_utc_datetime(&dt.naive_utc())
            .format("%Y-%m-%d %-I:%M %p")
            .to_string(),
        Err(_) => ts.to_string(),
    }
}

/// The tickets assigned to whoever this token belongs to and not yet done.
///
/// `POST`, not `GET`: it is the form proven against a real tenant, and it
/// keeps a JQL clause out of a URL that gets logged and rendered.
pub fn search_my_issues(site: &JiraSite) -> Result<Vec<IssueRow>> {
    require_complete(site)?;
    let url = format!("{}/search/jql", site.api_base);
    let payload = serde_json::json!({
        "jql": DEFAULT_JQL,
        "fields": LIST_FIELDS,
        "maxResults": MAX_RESULTS,
    })
    .to_string();
    let body = atlassian_http::request(
        &site.auth.email,
        &site.auth.token,
        &url,
        &[],
        Some(&payload),
    )?;
    parse_issue_list(&body)
}

/// One ticket in full.
pub fn fetch_issue(site: &JiraSite, key: &str) -> Result<Issue> {
    require_complete(site)?;
    validate_issue_key(key)?;
    let url = format!("{}/issue/{}", site.api_base, key.trim());
    let body = atlassian_http::request(
        &site.auth.email,
        &site.auth.token,
        &url,
        &[("fields", ISSUE_FIELDS.to_string())],
        None,
    )?;
    parse_issue(&body)
}

/// What this ticket can legally do right now, per its own workflow.
///
/// Asked of Jira rather than assumed, because workflows differ per project:
/// "Start Progress" and "Close" exist in some and not others, and a hardcoded
/// pair of buttons would be dead in any project that names them differently.
pub fn fetch_transitions(site: &JiraSite, key: &str) -> Result<Vec<Transition>> {
    require_complete(site)?;
    validate_issue_key(key)?;
    let url = format!("{}/issue/{}/transitions", site.api_base, key.trim());
    let body = atlassian_http::request(&site.auth.email, &site.auth.token, &url, &[], None)?;
    parse_transitions(&body)
}

/// Move a ticket. The only call in this module that changes anything.
pub fn do_transition(site: &JiraSite, key: &str, transition_id: &str) -> Result<()> {
    require_complete(site)?;
    validate_issue_key(key)?;
    validate_transition_id(transition_id)?;
    let url = format!("{}/issue/{}/transitions", site.api_base, key.trim());
    let payload = format!(r#"{{"transition":{{"id":"{}"}}}}"#, transition_id.trim());
    atlassian_http::request(
        &site.auth.email,
        &site.auth.token,
        &url,
        &[],
        Some(&payload),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth() -> AlertsAuth {
        AlertsAuth {
            email: "me@example.com".to_string(),
            token: "tok".to_string(),
            cloud_id: "cloud-1".to_string(),
        }
    }

    #[test]
    fn an_unconfigured_site_still_addresses_the_alerts_cloud_id() {
        // A build that sets nothing must behave exactly as it did before the
        // field existed.
        let site = JiraSite::new(auth(), "", None);
        assert_eq!(
            site.api_base(),
            "https://api.atlassian.com/ex/jira/cloud-1/rest/api/3"
        );
        assert!(site.is_complete());
    }

    #[test]
    fn a_configured_domain_wins_over_the_cloud_id_and_the_env_wins_over_both() {
        // The alert feed's tenant and the Jira site are not necessarily the
        // same place -- which is the bug this whole field exists for.
        assert_eq!(
            resolve_base_url("jira.example.com", None, "cloud-1"),
            "https://jira.example.com/rest/api/3"
        );
        assert_eq!(
            resolve_base_url("jira.example.com", Some("other.example.com".into()), "cloud-1"),
            "https://other.example.com/rest/api/3"
        );
        // Blank-but-set must not shadow the configured value.
        assert_eq!(
            resolve_base_url("jira.example.com", Some("   ".into()), "cloud-1"),
            "https://jira.example.com/rest/api/3"
        );
    }

    #[test]
    fn the_site_is_forgiving_about_how_it_was_pasted() {
        // Typed from a browser bar, or copied out of a working curl command.
        for input in [
            "jira.example.com",
            "https://jira.example.com",
            "https://jira.example.com/",
            "https://jira.example.com/rest/api/3",
            "https://jira.example.com/rest/api/2",
            "https://jira.example.com/rest/api/3/search/jql",
        ] {
            assert_eq!(
                resolve_base_url(input, None, "cloud-1"),
                "https://jira.example.com/rest/api/3",
                "{input} should normalise"
            );
        }
        // A token travels on this, so a scheme-less host is upgraded, never
        // downgraded.
        assert!(resolve_base_url("jira.example.com", None, "").starts_with("https://"));
    }

    #[test]
    fn nothing_configured_and_no_cloud_id_is_no_site_at_all() {
        // Better an empty base that `is_complete` refuses than a request
        // built against nowhere.
        assert_eq!(resolve_base_url("", None, ""), "");
        let site = JiraSite::new(AlertsAuth::default(), "", None);
        assert!(!site.is_complete());
        // Credentials without a site is equally unusable.
        assert!(!JiraSite::new(auth(), "", None).api_base().is_empty());
    }

    #[test]
    fn a_key_is_whitelisted_not_escaped() {
        for good in ["OPS-1", "OPS-1234", "AB2-9", "A-1"] {
            assert!(validate_issue_key(good).is_ok(), "{good} should be a key");
            assert!(looks_like_issue_key(good));
        }
        // It is interpolated into a URL path, so anything path-shaped, blank
        // or lowercase is refused rather than sanitised.
        for bad in [
            "ops-1",
            "OPS",
            "OPS-",
            "-1",
            "OPS-1/../secret",
            "OPS 1",
            "OPS-1x",
            "",
            "../OPS-1",
            "OPS-1?expand=x",
        ] {
            assert!(validate_issue_key(bad).is_err(), "{bad:?} must be refused");
            assert!(!looks_like_issue_key(bad));
        }
    }

    #[test]
    fn a_transition_id_is_whitelisted_too() {
        // It lands in a JSON body, so a quote in it would break out of the
        // string it is written into.
        assert!(validate_transition_id("31").is_ok());
        for bad in ["", "abc", "3 1", "31\"}", "-1"] {
            assert!(validate_transition_id(bad).is_err(), "{bad:?} must be refused");
        }
    }

    /// Trimmed-down copy of a real `/search/jql` response, including the two
    /// fields Jira routinely returns as `null`.
    const SEARCH_SAMPLE: &str = r#"{
      "issues": [
        {
          "key": "OPS-1421",
          "fields": {
            "summary": "Bastion secondary out of sync",
            "status": { "name": "In Progress",
                        "statusCategory": { "key": "indeterminate" } },
            "issuetype": { "name": "Task" },
            "priority": { "name": "High" },
            "project": { "key": "OPS" },
            "updated": "2026-08-26T09:14:22.512+0100"
          }
        },
        {
          "key": "INFRA-77",
          "fields": {
            "summary": "Rotate the bastion key",
            "status": { "name": "To Do",
                        "statusCategory": { "key": "new" } },
            "issuetype": { "name": "Story" },
            "priority": null,
            "project": { "key": "INFRA" },
            "updated": "2026-08-25T16:02:00.000+0100"
          }
        }
      ]
    }"#;

    #[test]
    fn the_search_response_parses_including_its_null_fields() {
        let rows = parse_issue_list(SEARCH_SAMPLE).expect("should parse");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].key, "OPS-1421");
        assert_eq!(rows[0].summary, "Bastion secondary out of sync");
        assert_eq!(rows[0].status, "In Progress");
        assert_eq!(rows[0].status_category, "indeterminate");
        assert_eq!(rows[0].issue_type, "Task");
        assert_eq!(rows[0].priority, "High");
        assert_eq!(rows[0].project, "OPS");
        // A project with no priority field is an ordinary state, not a
        // parse failure.
        assert_eq!(rows[1].priority, "");
        assert_eq!(rows[1].status_category, "new");
    }

    #[test]
    fn no_open_tickets_is_an_empty_list_and_not_an_error() {
        let rows = parse_issue_list(r#"{"issues":[]}"#).expect("empty is valid");
        assert!(rows.is_empty());
    }

    #[test]
    fn a_response_with_no_issues_key_is_an_error_not_an_empty_list() {
        // The two must never look alike: "you have no open tickets" and "we
        // could not read the reply" are different answers, and rendering the
        // second as the first is the silent-empty failure this repo has been
        // bitten by before.
        assert!(parse_issue_list(r#"{"warningMessages":["bad jql"]}"#).is_err());
        assert!(parse_issue_list("not json at all").is_err());
    }

    const ISSUE_SAMPLE: &str = r#"{
      "key": "OPS-1421",
      "fields": {
        "summary": "Bastion secondary out of sync",
        "description": "The secondary bastion is missing three accounts.\n\nSteps:\n1. run the sync",
        "status": { "name": "In Progress",
                    "statusCategory": { "key": "indeterminate" } },
        "issuetype": { "name": "Task" },
        "priority": { "name": "High" },
        "assignee": null,
        "reporter": { "displayName": "A Reporter" },
        "created": "2026-08-20T11:00:00.000+0100",
        "updated": "2026-08-26T09:14:22.512+0100",
        "labels": ["bastion", "efs"]
      }
    }"#;

    #[test]
    fn an_issue_parses_and_an_unassigned_ticket_is_not_a_failure() {
        let issue = parse_issue(ISSUE_SAMPLE).expect("should parse");
        assert_eq!(issue.key, "OPS-1421");
        assert_eq!(issue.status, "In Progress");
        assert_eq!(issue.reporter, "A Reporter");
        assert_eq!(issue.labels, vec!["bastion", "efs"]);
        // Flattened by `description_text`, whichever way the site sent it.
        assert!(issue.description.starts_with("The secondary bastion"));
        assert!(issue.description.contains('\n'));
        // `assignee: null` is an unassigned ticket, not a broken response.
        assert_eq!(issue.assignee, "");
    }

    #[test]
    fn an_issue_with_no_description_or_labels_still_parses() {
        let body = r#"{"key":"OPS-2","fields":{"summary":"s","status":{"name":"To Do"}}}"#;
        let issue = parse_issue(body).expect("should parse");
        assert_eq!(issue.key, "OPS-2");
        assert_eq!(issue.description, "");
        assert!(issue.labels.is_empty());
        assert_eq!(issue.priority, "");
    }

    #[test]
    fn an_issue_response_with_no_key_is_an_error() {
        assert!(parse_issue(r#"{"errorMessages":["Issue does not exist"]}"#).is_err());
    }

    const TRANSITIONS_SAMPLE: &str = r#"{
      "transitions": [
        { "id": "11", "name": "Start Progress", "to": { "name": "In Progress" } },
        { "id": "31", "name": "Done", "to": { "name": "Closed" } },
        { "name": "Broken entry with no id", "to": { "name": "Nowhere" } }
      ]
    }"#;

    #[test]
    fn transitions_parse_and_an_entry_with_no_id_is_dropped() {
        let ts = parse_transitions(TRANSITIONS_SAMPLE).expect("should parse");
        // The third entry could not be actioned, and dropping it must not
        // cost the ticket its other two buttons.
        assert_eq!(ts.len(), 2);
        assert_eq!(ts[0].id, "11");
        assert_eq!(ts[0].name, "Start Progress");
        // The landing status is kept so a button named "Done" that moves the
        // ticket to "Closed" can say so.
        assert_eq!(ts[1].to_status, "Closed");
    }

    #[test]
    fn a_ticket_with_no_legal_moves_is_an_empty_list() {
        assert!(parse_transitions(r#"{"transitions":[]}"#)
            .expect("empty is valid")
            .is_empty());
        assert!(parse_transitions(r#"{"nope":1}"#).is_err());
    }

    #[test]
    fn jira_timestamps_parse_despite_the_colonless_offset() {
        // Jira writes `+0100`, which parse_from_rfc3339 rejects outright.
        // Getting this wrong shows every ticket's dates as raw API strings.
        assert_ne!(local_time("2026-08-26T09:14:22.512+0100"), "2026-08-26T09:14:22.512+0100");
        // The alert feed's spelling (`Z`, with a colon offset) still works.
        assert_ne!(local_time("2026-08-26T09:14:22.512Z"), "2026-08-26T09:14:22.512Z");
        assert_ne!(local_time("2026-08-26T09:14:22+01:00"), "2026-08-26T09:14:22+01:00");
        // Blank stays blank rather than becoming an epoch date.
        assert_eq!(local_time(""), "");
        assert_eq!(local_time("  "), "");
        // Anything unparseable is shown verbatim — honest, unlike a blank.
        assert_eq!(local_time("who knows"), "who knows");
    }

    #[test]
    fn an_incomplete_site_is_refused_before_any_request_is_made() {
        let blank = JiraSite::new(AlertsAuth::default(), "", None);
        assert!(search_my_issues(&blank).is_err());
        assert!(fetch_issue(&blank, "OPS-1").is_err());
        assert!(fetch_transitions(&blank, "OPS-1").is_err());
        assert!(do_transition(&blank, "OPS-1", "11").is_err());
    }

    /// The ADF a real v3 description arrives as. Getting this wrong renders
    /// every ticket's description empty, which reads as "this ticket has no
    /// description" rather than as a bug.
    const ADF: &str = r#"{
      "type": "doc",
      "version": 1,
      "content": [
        { "type": "paragraph", "content": [
            { "type": "text", "text": "The secondary bastion is missing " },
            { "type": "text", "text": "three accounts", "marks": [{"type":"strong"}] },
            { "type": "text", "text": "." } ] },
        { "type": "paragraph", "content": [
            { "type": "text", "text": "Raised by " },
            { "type": "mention", "attrs": { "id": "abc", "text": "@A Person" } } ] },
        { "type": "bulletList", "content": [
            { "type": "listItem", "content": [
                { "type": "paragraph", "content": [{"type":"text","text":"run the sync"}] } ] },
            { "type": "listItem", "content": [
                { "type": "paragraph", "content": [{"type":"text","text":"re-check uids"}] } ] } ] },
        { "type": "paragraph", "content": [
            { "type": "inlineCard", "attrs": { "url": "https://example.com/runbook" } } ] }
      ]
    }"#;

    #[test]
    fn an_adf_description_flattens_to_readable_text() {
        let v: Value = serde_json::from_str(ADF).expect("adf parses");
        let out = description_text(&v);
        // Text survives, and so does the sentence it was split across --
        // ADF breaks a styled run into its own node.
        assert!(out.contains("The secondary bastion is missing three accounts."), "{out}");
        // Content, not decoration: the who and the where are kept.
        assert!(out.contains("@A Person"), "{out}");
        assert!(out.contains("https://example.com/runbook"), "{out}");
        // List structure survives as something readable.
        assert!(out.contains("• run the sync"), "{out}");
        assert!(out.contains("• re-check uids"), "{out}");
        // Nested block nodes each add a break; stacking them would leave the
        // description full of blank lines that were never in the document.
        assert!(!out.contains("\n\n\n"), "runs of blank lines: {out:?}");
        assert!(!out.starts_with('\n') && !out.ends_with('\n'));
    }

    #[test]
    fn a_plain_string_description_is_taken_as_is() {
        // v2's spelling, and what a site may still answer with. One function,
        // either payload.
        let v = Value::String("just text\nover two lines".to_string());
        assert_eq!(description_text(&v), "just text\nover two lines");
        // A ticket with no description is neither.
        assert_eq!(description_text(&Value::Null), "");
    }

    #[test]
    fn an_unknown_adf_node_keeps_the_text_inside_it() {
        // ADF gains node types over time. Losing a paragraph because it sat
        // inside a panel nobody had heard of is worse than losing the panel.
        let v: Value = serde_json::from_str(
            r#"{"type":"doc","content":[
                 {"type":"panelOfTheFuture","content":[
                   {"type":"paragraph","content":[{"type":"text","text":"still here"}]}]}]}"#,
        )
        .expect("parses");
        assert_eq!(description_text(&v), "still here");
    }

    /// The default JQL must not need an account id: `currentUser()` is what
    /// lets this work on a machine that has never been told who you are.
    #[test]
    fn the_default_jql_resolves_the_user_server_side() {
        assert!(DEFAULT_JQL.contains("currentUser()"));
        assert!(DEFAULT_JQL.contains("statusCategory != Done"));
        assert!(!DEFAULT_JQL.contains("accountId"));
    }
}

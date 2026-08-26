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

/// Comments fetched per ticket. A thread longer than this is rare, and the
/// window scrolls.
const MAX_COMMENTS: u32 = 100;

/// Environment override for the Jira site. Layered above the `features.json`
/// value so a domain need not be committed to git to be used.
pub const JIRA_BASE_URL_ENV: &str = "JIRA_BASE_URL";

/// Fields the list needs. Deliberately not every field: the list renders six
/// columns and a search that drags every custom field on every ticket is
/// slower for nothing.
const LIST_FIELDS: &[&str] =
    &["summary", "status", "issuetype", "priority", "updated", "project", "duedate"];

/// Fields the ticket view needs. `description` is here and absent from
/// [`LIST_FIELDS`] on purpose — it is the largest field on most tickets and
/// the list never shows it.
const ISSUE_FIELDS: &str = "summary,description,status,issuetype,priority,assignee,\
     reporter,created,updated,labels,duedate";

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
    /// `duedate` — a **date**, `2026-09-01`, with no time and no timezone.
    /// Empty when the ticket has none. See [`due_label`] for why it must
    /// never go through [`local_time`].
    pub due: String,
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
    pub assignee_id: String,
    pub reporter: String,
    pub reporter_id: String,
    pub created: String,
    pub updated: String,
    /// See [`IssueRow::due`].
    pub due: String,
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
    /// The **required** fields on this transition's screen.
    ///
    /// Jira transitions can carry a screen — a change request whose Close
    /// asks for a comment, an ordinary ticket whose Close asks for a
    /// Resolution — and posting the bare `{"transition":{"id":…}}` at one of
    /// those is rejected with a 400 saying nothing the caller anticipated.
    /// There is no two-phase API: the screen cannot be "opened" and then
    /// submitted, so everything must go in one POST. Knowing the screen up
    /// front is what lets the app *ask* first and then send once.
    ///
    /// Optional fields are deliberately dropped. This is a ticket viewer
    /// reproducing a required prompt, not a general issue editor.
    pub fields: Vec<ScreenField>,
}

impl Transition {
    /// True when this move needs a prompt rather than a single click.
    pub fn needs_prompt(&self) -> bool {
        !self.fields.is_empty()
    }

    /// Required fields this window cannot render, by display name. Non-empty
    /// means the move has to be made in the browser — a user picker or a
    /// cascading select is a different feature from a ticket viewer, and
    /// guessing at one would post the wrong value to a live ticket.
    pub fn unsupported(&self) -> Vec<&str> {
        self.fields
            .iter()
            .filter(|f| matches!(f.kind, FieldKind::Unsupported(_)))
            .map(|f| f.name.as_str())
            .collect()
    }
}

/// One required field on a transition screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenField {
    /// The API's key — `resolution`, `comment`, `customfield_10042`. This is
    /// what the payload is built against.
    pub key: String,
    /// The label Jira shows. A custom field's key names nothing, so this is
    /// what the prompt is captioned with.
    pub name: String,
    pub kind: FieldKind,
}

/// How a screen field is rendered, and how its value is sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldKind {
    /// The comment box. Sent under `update.comment`, not `fields` — that is
    /// the shape Jira accepts.
    Comment,
    /// A dropdown. **Options come from Jira**, never a hardcoded list: a
    /// Resolution's choices are per-project, so a built-in list would be
    /// right on one project and wrong on the next.
    Select {
        options: Vec<FieldOption>,
        /// The field takes a list, so the chosen option is wrapped in one.
        array: bool,
    },
    /// A free-text field.
    Text { multiline: bool },
    /// A numeric field.
    Number,
    /// A type this window does not render, carrying the schema type so the
    /// message can say what it was.
    Unsupported(String),
}

/// One choice in a [`FieldKind::Select`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldOption {
    pub id: String,
    pub label: String,
}

/// A value the user supplied for a screen field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldInput {
    /// Typed text — a comment body, or a text/number field.
    Text(String),
    /// The id of a chosen [`FieldOption`].
    Option(String),
}

/// One comment on a ticket.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Comment {
    pub author: String,
    /// The author's Atlassian account id. Kept so mention ranking works on
    /// identity rather than spelling — two people sharing a display name is
    /// the exact case ranking exists for.
    pub author_id: String,
    /// As the API returned it. Rendered in local time.
    pub created: String,
    /// Flattened by [`description_text`] — comment bodies are ADF on v3,
    /// exactly like the description.
    pub body: String,
}

/// Characters that must be typed after `@` before the directory is queried.
///
/// Below this the dropdown offers the ticket's own people, which needs no
/// request at all and is very often the answer — the reporter especially.
pub const MENTION_SEARCH_MIN: usize = 3;

/// How many names the mention dropdown offers.
pub const MENTION_LIMIT: usize = 5;

/// A person who can be mentioned.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct User {
    /// The Atlassian account id — what a mention node actually carries. A
    /// mention without it is just text, which is what typing a name by hand
    /// produces.
    pub account_id: String,
    pub display_name: String,
}

/// True once the typed token is long enough to be worth a directory query.
pub fn should_search_users(token: &str) -> bool {
    token.chars().count() >= MENTION_SEARCH_MIN
}

/// The people already on this ticket, most likely first: **reporter, then
/// whoever has commented (most recent first), then the assignee**.
///
/// Reporter leads deliberately — an `@` in a comment is usually answering the
/// person who raised the ticket. Commenters come next because someone who has
/// spoken on the ticket is more likely to be the one being answered than an
/// assignee who may never have said anything (and is frequently you).
///
/// Deduped by account id, so a reporter who has also commented appears once,
/// at the rank they first earned. Anyone without an account id is dropped:
/// they cannot be mentioned at all.
pub fn ticket_participants(issue: &Issue, comments: &[Comment]) -> Vec<User> {
    let mut out: Vec<User> = Vec::new();
    let push = |id: &str, name: &str, out: &mut Vec<User>| {
        if id.trim().is_empty() || out.iter().any(|u| u.account_id == id) {
            return;
        }
        out.push(User {
            account_id: id.to_string(),
            display_name: name.to_string(),
        });
    };
    push(&issue.reporter_id, &issue.reporter, &mut out);
    for c in comments.iter().rev() {
        push(&c.author_id, &c.author, &mut out);
    }
    push(&issue.assignee_id, &issue.assignee, &mut out);
    out
}

/// The dropdown's contents for what has been typed after `@`.
///
/// Pure, so the ordering is pinned by a test rather than by the order of two
/// loops inside a render function. Participants always outrank directory
/// results — a John who reported the ticket is far more likely to be the John
/// meant than a John who merely exists.
pub fn mention_candidates(
    token: &str,
    participants: &[User],
    directory: &[User],
    limit: usize,
) -> Vec<User> {
    let mut out: Vec<User> = participants
        .iter()
        .filter(|u| matches_token(&u.display_name, token))
        .cloned()
        .collect();
    for u in directory {
        if out.len() >= limit {
            break;
        }
        if u.account_id.trim().is_empty() || out.iter().any(|p| p.account_id == u.account_id) {
            continue;
        }
        out.push(u.clone());
    }
    out.truncate(limit);
    out
}

/// Whether a display name matches what has been typed so far.
///
/// Matched per **word**, not just from the start, so `@smith` finds
/// "John Smith" — a surname is what people type at least as often as a first
/// name, and a whole-string `starts_with` would find nobody.
fn matches_token(display_name: &str, token: &str) -> bool {
    let token = token.trim();
    if token.is_empty() {
        return true;
    }
    let token = token.to_lowercase();
    let name = display_name.to_lowercase();
    name.starts_with(&token) || name.split_whitespace().any(|w| w.starts_with(&token))
}

/// Parse a `/user/search` response.
pub fn parse_users(body: &str) -> Result<Vec<User>> {
    let v: Value = serde_json::from_str(body)
        .map_err(|e| AppError::InvalidArgument(format!("jira: could not parse users: {e}")))?;
    let Some(list) = v.as_array() else {
        return Err(AppError::InvalidArgument(
            "jira: user search did not return a list".to_string(),
        ));
    };
    Ok(list
        .iter()
        .filter(|u| {
            // A deactivated account cannot be notified, and an `app` account
            // is a bot — offering either wastes a slot in a list of five.
            u.get("active").and_then(Value::as_bool).unwrap_or(true)
                && field_str(u, &["accountType"]) != "app"
        })
        .map(|u| User {
            account_id: field_str(u, &["accountId"]),
            display_name: field_str(u, &["displayName"]),
        })
        .filter(|u| !u.account_id.is_empty() && !u.display_name.is_empty())
        .collect())
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
            due: field_str(i, &["fields", "duedate"]),
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
        assignee_id: field_str(&v, &["fields", "assignee", "accountId"]),
        reporter: field_str(&v, &["fields", "reporter", "displayName"]),
        reporter_id: field_str(&v, &["fields", "reporter", "accountId"]),
        created: field_str(&v, &["fields", "created"]),
        updated: field_str(&v, &["fields", "updated"]),
        due: field_str(&v, &["fields", "duedate"]),
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
        .map(|t| {
            // `fields` is only present when the request asked for
            // `expand=transitions.fields`. Absent, every transition looks
            // unconstrained — which is the old behaviour and the reason a
            // required-comment Close came back as a bare 400.
            Transition {
                id: field_str(t, &["id"]),
                name: field_str(t, &["name"]),
                to_status: field_str(t, &["to", "name"]),
                fields: screen_fields(t),
            }
        })
        .filter(|t| !t.id.is_empty())
        .collect())
}

/// The required fields on one transition's screen, in the order Jira lists
/// them with `comment` last — a comment reads as the closing note under
/// whatever else the screen asks for, which is how Jira's own dialog is laid
/// out.
fn screen_fields(t: &Value) -> Vec<ScreenField> {
    let Some(fields) = t.get("fields").and_then(Value::as_object) else {
        // Only present when the request asked for
        // `expand=transitions.fields`. Absent, every transition looks
        // unconstrained — which is the old behaviour, and the reason a
        // required-comment Close came back as a bare 400.
        return Vec::new();
    };
    let mut out: Vec<ScreenField> = fields
        .iter()
        .filter(|(_, spec)| spec.get("required").and_then(Value::as_bool).unwrap_or(false))
        .map(|(key, spec)| {
            let label = field_str(spec, &["name"]);
            ScreenField {
                key: key.clone(),
                name: if label.is_empty() { key.clone() } else { label },
                kind: field_kind(key, spec),
            }
        })
        .collect();
    out.sort_by(|a, b| {
        let rank = |f: &ScreenField| u8::from(matches!(f.kind, FieldKind::Comment));
        rank(a).cmp(&rank(b)).then_with(|| a.name.cmp(&b.name))
    });
    out
}

/// Decide how one screen field is rendered.
///
/// `allowedValues` is the strongest signal and is checked first: whatever the
/// schema calls it, a field that ships its own list of choices is a dropdown,
/// and that covers Resolution, priority and every custom select without
/// naming any of them here.
fn field_kind(key: &str, spec: &Value) -> FieldKind {
    if key == "comment" {
        return FieldKind::Comment;
    }
    let schema_type = field_str(spec, &["schema", "type"]);
    if let Some(values) = spec.get("allowedValues").and_then(Value::as_array) {
        let options: Vec<FieldOption> = values
            .iter()
            .filter_map(|v| {
                let id = field_str(v, &["id"]);
                if id.is_empty() {
                    return None;
                }
                // `name` for a system field, `value` for a custom select.
                let label = [
                    field_str(v, &["name"]),
                    field_str(v, &["value"]),
                    id.clone(),
                ]
                .into_iter()
                .find(|s| !s.is_empty())
                .unwrap_or_default();
                Some(FieldOption { id, label })
            })
            .collect();
        if !options.is_empty() {
            return FieldKind::Select { options, array: schema_type == "array" };
        }
    }
    match schema_type.as_str() {
        "string" => FieldKind::Text { multiline: key == "description" },
        "number" => FieldKind::Number,
        // A user picker, a date, a cascading select, an array with no
        // allowedValues. Guessing at any of these posts a wrong value to a
        // live ticket, so they are named and refused instead.
        other => FieldKind::Unsupported(other.to_string()),
    }
}

/// Parse a `/comment` response, oldest first — the order a thread reads in,
/// and the order Jira itself returns.
pub fn parse_comments(body: &str) -> Result<Vec<Comment>> {
    let v: Value = serde_json::from_str(body)
        .map_err(|e| AppError::InvalidArgument(format!("jira: could not parse comments: {e}")))?;
    let Some(list) = v.get("comments").and_then(Value::as_array) else {
        return Err(AppError::InvalidArgument(
            "jira: response carried no 'comments' list".to_string(),
        ));
    };
    Ok(list
        .iter()
        .map(|c| Comment {
            author: field_str(c, &["author", "displayName"]),
            author_id: field_str(c, &["author", "accountId"]),
            created: field_str(c, &["created"]),
            body: c.get("body").map(description_text).unwrap_or_default(),
        })
        .collect())
}

/// Turn typed text into an ADF document for a comment body.
///
/// The inverse of [`description_text`], and required rather than optional:
/// v3 rejects a plain string here.
///
/// **A blank line starts a new paragraph; a single newline is a soft break
/// inside one.** That is what Jira's own editor does with Enter and
/// Shift+Enter, and it is what makes the round trip faithful — mapping every
/// newline to a paragraph turns each line break the author typed into a blank
/// line, so the posted comment is visibly not what they wrote.
/// `a_typed_comment_round_trips_through_adf` pins it.
///
/// **Built with `serde_json`, never string formatting.** This is arbitrary
/// user text going into a JSON request body; a quote, a backslash or a
/// newline written into a hand-built string would break out of it.
/// `a_comment_body_cannot_break_out_of_its_json` pins that.
pub fn comment_adf(text: &str) -> Value {
    comment_adf_with_mentions(text, &[])
}

/// As [`comment_adf`], but turning known `@Display Name` runs into real
/// mention nodes.
///
/// `mentions` pairs the exact text that was inserted (`"@John Smith"`) with
/// the account id behind it, recorded when the name was **picked from the
/// dropdown**. Text nobody picked stays text: without an account id there is
/// nothing to tag, and guessing at one would notify a person the app never
/// resolved. That is the whole reason typing `@John Smith` by hand did
/// nothing before this existed.
pub fn comment_adf_with_mentions(text: &str, mentions: &[(String, String)]) -> Value {
    // Longest first, so "@John Smithson" is not eaten by "@John Smith".
    let mut ordered: Vec<&(String, String)> = mentions.iter().collect();
    ordered.sort_by_key(|(label, _)| std::cmp::Reverse(label.chars().count()));

    let normalized = text.replace("\r\n", "\n");
    let paragraphs: Vec<Value> = normalized
        .split("\n\n")
        .map(|para| {
            let mut content: Vec<Value> = Vec::new();
            for (i, line) in para.split('\n').enumerate() {
                if i > 0 {
                    content.push(serde_json::json!({ "type": "hardBreak" }));
                }
                content.extend(inline_nodes(line, &ordered));
            }
            if content.is_empty() {
                // An empty paragraph is how ADF spells a blank line; a
                // paragraph carrying an empty text node is rejected.
                serde_json::json!({ "type": "paragraph" })
            } else {
                serde_json::json!({ "type": "paragraph", "content": content })
            }
        })
        .collect();
    serde_json::json!({ "type": "doc", "version": 1, "content": paragraphs })
}

/// One line split into text and mention nodes.
fn inline_nodes(line: &str, mentions: &[&(String, String)]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut buf = String::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        let rest: String = chars[i..].iter().collect();
        let hit = mentions
            .iter()
            .find(|(label, id)| !id.is_empty() && rest.starts_with(label.as_str()));
        match hit {
            Some((label, id)) => {
                if !buf.is_empty() {
                    out.push(serde_json::json!({ "type": "text", "text": buf }));
                    buf = String::new();
                }
                out.push(serde_json::json!({
                    "type": "mention",
                    "attrs": { "id": id, "text": label },
                }));
                i += label.chars().count();
            }
            None => {
                buf.push(chars[i]);
                i += 1;
            }
        }
    }
    if !buf.is_empty() {
        out.push(serde_json::json!({ "type": "text", "text": buf }));
    }
    out
}

/// Reject a comment that would post nothing. Whitespace-only is the case
/// that matters: it passes an `is_empty` check and posts a blank comment
/// everyone on the ticket gets notified about.
fn require_comment_text(text: &str) -> Result<()> {
    if text.trim().is_empty() {
        return Err(AppError::InvalidArgument(
            "jira: a comment needs some text".to_string(),
        ));
    }
    Ok(())
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
    // Without this expand the response says nothing about each transition's
    // screen, so a Close that requires a comment looks identical to one that
    // does not — and the move is attempted and rejected with a 400.
    let body = atlassian_http::request(
        &site.auth.email,
        &site.auth.token,
        &url,
        &[("expand", "transitions.fields".to_string())],
        None,
    )?;
    parse_transitions(&body)
}

/// Search Jira's own user directory — the same list its mention autocomplete
/// draws from.
///
/// Only called once [`should_search_users`] is satisfied; below that the
/// dropdown shows the ticket's own people and makes no request.
pub fn search_users(site: &JiraSite, query: &str) -> Result<Vec<User>> {
    require_complete(site)?;
    let url = format!("{}/user/search", site.api_base);
    let body = atlassian_http::request(
        &site.auth.email,
        &site.auth.token,
        &url,
        &[
            ("query", query.trim().to_string()),
            ("maxResults", "10".to_string()),
        ],
        None,
    )?;
    parse_users(&body)
}

/// A ticket's comment thread, oldest first.
pub fn fetch_comments(site: &JiraSite, key: &str) -> Result<Vec<Comment>> {
    require_complete(site)?;
    validate_issue_key(key)?;
    let url = format!("{}/issue/{}/comment", site.api_base, key.trim());
    let body = atlassian_http::request(
        &site.auth.email,
        &site.auth.token,
        &url,
        &[("maxResults", MAX_COMMENTS.to_string())],
        None,
    )?;
    parse_comments(&body)
}

/// Post a comment.
pub fn add_comment(
    site: &JiraSite,
    key: &str,
    text: &str,
    mentions: &[(String, String)],
) -> Result<()> {
    require_complete(site)?;
    validate_issue_key(key)?;
    require_comment_text(text)?;
    let url = format!("{}/issue/{}/comment", site.api_base, key.trim());
    let payload =
        serde_json::json!({ "body": comment_adf_with_mentions(text, mentions) }).to_string();
    atlassian_http::request(&site.auth.email, &site.auth.token, &url, &[], Some(&payload))?;
    Ok(())
}

/// Build the transition POST body from a screen and the values supplied for
/// it.
///
/// Pure, and separate from [`do_transition`], because this is where a wrong
/// answer is a wrong write to a live ticket — the two shapes are not
/// interchangeable and Jira accepts only one of each:
///
/// - **a comment goes under `update.comment[].add.body`**, never in `fields`;
/// - **everything else goes in `fields`**, an option as `{"id": …}` and an
///   array-typed one as `[{"id": …}]`.
///
/// Every required field must have a non-blank value, checked here rather than
/// left to the server: a 400 from Jira arrives after the click, names the
/// field in its own words, and is exactly the experience this prompt exists
/// to replace.
pub fn transition_payload(
    transition_id: &str,
    fields: &[ScreenField],
    inputs: &[(String, FieldInput)],
) -> Result<Value> {
    validate_transition_id(transition_id)?;
    let mut payload = serde_json::json!({ "transition": { "id": transition_id.trim() } });
    let mut set_fields = serde_json::Map::new();
    let mut update = serde_json::Map::new();

    for field in fields {
        let supplied = inputs.iter().find(|(k, _)| k == &field.key).map(|(_, v)| v);
        let missing = || {
            AppError::InvalidArgument(format!("jira: '{}' is required", field.name))
        };
        match &field.kind {
            FieldKind::Unsupported(kind) => {
                return Err(AppError::InvalidArgument(format!(
                    "jira: '{}' is a required {kind} field, which this window cannot \
                     set — make this change in Jira",
                    field.name
                )));
            }
            FieldKind::Comment => {
                let FieldInput::Text(text) = supplied.ok_or_else(missing)? else {
                    return Err(missing());
                };
                require_comment_text(text)?;
                update.insert(
                    "comment".to_string(),
                    serde_json::json!([{ "add": { "body": comment_adf(text) } }]),
                );
            }
            FieldKind::Select { options, array } => {
                let FieldInput::Option(id) = supplied.ok_or_else(missing)? else {
                    return Err(missing());
                };
                // The id has to be one Jira offered. A free-typed id would be
                // a silent wrong write — Jira accepts any id that exists,
                // including one belonging to another field's option.
                if !options.iter().any(|o| &o.id == id) {
                    return Err(AppError::InvalidArgument(format!(
                        "jira: '{}' is not one of the options for '{}'",
                        id, field.name
                    )));
                }
                let one = serde_json::json!({ "id": id });
                set_fields.insert(
                    field.key.clone(),
                    if *array { serde_json::json!([one]) } else { one },
                );
            }
            FieldKind::Text { .. } => {
                let FieldInput::Text(text) = supplied.ok_or_else(missing)? else {
                    return Err(missing());
                };
                if text.trim().is_empty() {
                    return Err(missing());
                }
                set_fields.insert(field.key.clone(), Value::String(text.clone()));
            }
            FieldKind::Number => {
                let FieldInput::Text(text) = supplied.ok_or_else(missing)? else {
                    return Err(missing());
                };
                let n: f64 = text.trim().parse().map_err(|_| {
                    AppError::InvalidArgument(format!(
                        "jira: '{}' must be a number, got '{}'",
                        field.name,
                        text.trim()
                    ))
                })?;
                set_fields.insert(
                    field.key.clone(),
                    serde_json::Number::from_f64(n).map(Value::Number).ok_or_else(|| {
                        AppError::InvalidArgument(format!(
                            "jira: '{}' is not a finite number",
                            field.name
                        ))
                    })?,
                );
            }
        }
    }

    if !set_fields.is_empty() {
        payload["fields"] = Value::Object(set_fields);
    }
    if !update.is_empty() {
        payload["update"] = Value::Object(update);
    }
    Ok(payload)
}

/// Move a ticket, filling in whatever its screen requires.
///
/// The only call in this module that changes anything. `fields` is the
/// transition's own screen (from [`fetch_transitions`]) and `inputs` are the
/// values collected for it; a transition with no screen passes both empty and
/// posts exactly what it always did.
pub fn do_transition(
    site: &JiraSite,
    key: &str,
    transition_id: &str,
    fields: &[ScreenField],
    inputs: &[(String, FieldInput)],
) -> Result<()> {
    require_complete(site)?;
    validate_issue_key(key)?;
    let payload = transition_payload(transition_id, fields, inputs)?;
    let url = format!("{}/issue/{}/transitions", site.api_base, key.trim());
    atlassian_http::request(
        &site.auth.email,
        &site.auth.token,
        &url,
        &[],
        Some(&payload.to_string()),
    )?;
    Ok(())
}

/// How a due date stands relative to today.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DueState {
    /// No due date on the ticket.
    None,
    /// Due later.
    Future,
    /// Due today.
    Today,
    /// Past its date and not finished.
    Overdue,
}

/// Classify a due date.
///
/// `today` is a parameter rather than read from the clock so the boundaries
/// are testable without freezing time.
///
/// **A ticket in the `done` category is never overdue**, whatever its date
/// says: it is finished, not late, and colouring a closed ticket red is
/// noise on the one row that needs no attention.
pub fn due_state(due: &str, status_category: &str, today: chrono::NaiveDate) -> DueState {
    let Some(date) = parse_due(due) else {
        return DueState::None;
    };
    if status_category.trim().eq_ignore_ascii_case("done") {
        return DueState::Future;
    }
    match date.cmp(&today) {
        std::cmp::Ordering::Less => DueState::Overdue,
        std::cmp::Ordering::Equal => DueState::Today,
        std::cmp::Ordering::Greater => DueState::Future,
    }
}

/// A due date for display.
///
/// **Never routed through [`local_time`].** `duedate` is a bare date with no
/// time and no offset; converting it as though it were an instant shifts the
/// day for anyone not on UTC, so a ticket due the 1st reads as the 31st for
/// half the world. It is rendered as the calendar date Jira stated, and an
/// unparseable value is shown verbatim rather than blanked.
pub fn due_label(due: &str) -> String {
    match parse_due(due) {
        Some(d) => d.format("%Y-%m-%d").to_string(),
        None => due.trim().to_string(),
    }
}

fn parse_due(due: &str) -> Option<chrono::NaiveDate> {
    let due = due.trim();
    if due.is_empty() {
        return None;
    }
    chrono::NaiveDate::parse_from_str(due, "%Y-%m-%d").ok()
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

    /// A transitions response with `expand=transitions.fields`, carrying the
    /// two screens that actually occur here: a change request whose Close
    /// wants a comment, and an ordinary ticket whose Close wants a
    /// Resolution.
    const SCREENS_SAMPLE: &str = r#"{
      "transitions": [
        { "id": "41", "name": "Close", "to": { "name": "Closed" },
          "fields": {
            "comment": { "required": true, "name": "Comment",
                         "schema": { "type": "string", "system": "comment" } },
            "resolution": { "required": true, "name": "Resolution",
                            "schema": { "type": "resolution" },
                            "allowedValues": [
                              { "id": "10000", "name": "Done" },
                              { "id": "10001", "name": "Task Completed" } ] },
            "assignee": { "required": false, "name": "Assignee",
                          "schema": { "type": "user" } } } },
        { "id": "51", "name": "Escalate", "to": { "name": "Escalated" },
          "fields": {
            "customfield_10042": { "required": true, "name": "Approver",
                                   "schema": { "type": "user" } } } },
        { "id": "61", "name": "Reopen", "to": { "name": "To Do" }, "fields": {} }
      ]
    }"#;

    fn screen_for(name: &str) -> Transition {
        parse_transitions(SCREENS_SAMPLE)
            .expect("parses")
            .into_iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("no transition named {name}"))
    }

    #[test]
    fn a_transition_screen_is_read_so_the_prompt_can_be_built() {
        let close = screen_for("Close");
        assert!(close.needs_prompt());
        // Optional fields are dropped -- this reproduces a required prompt,
        // it is not a general issue editor.
        assert_eq!(close.fields.len(), 2, "assignee is optional: {:?}", close.fields);

        // The dropdown's options come from Jira, never a hardcoded list: a
        // Resolution's choices are per-project.
        let res = close.fields.iter().find(|f| f.key == "resolution").expect("resolution");
        assert_eq!(res.name, "Resolution");
        match &res.kind {
            FieldKind::Select { options, array } => {
                assert!(!array);
                assert_eq!(options.len(), 2);
                assert!(options.iter().any(|o| o.label == "Task Completed" && o.id == "10001"));
            }
            other => panic!("resolution should be a dropdown, got {other:?}"),
        }

        // The comment is its own kind: it goes to `update`, not `fields`.
        let c = close.fields.iter().find(|f| f.key == "comment").expect("comment");
        assert_eq!(c.kind, FieldKind::Comment);
        // ...and it sorts last, the way Jira's own dialog reads.
        assert_eq!(close.fields.last().expect("some").key, "comment");
        assert!(close.unsupported().is_empty());
    }

    #[test]
    fn a_transition_with_no_screen_still_needs_no_prompt() {
        let reopen = screen_for("Reopen");
        assert!(!reopen.needs_prompt());
        // And a response fetched without the expand looks the same, which is
        // the pre-existing behaviour rather than a claim that no screen
        // exists.
        for tr in parse_transitions(TRANSITIONS_SAMPLE).expect("parses") {
            assert!(!tr.needs_prompt());
        }
    }

    #[test]
    fn a_field_this_window_cannot_render_is_named_not_guessed_at() {
        // Guessing at a user picker posts a wrong value to a live ticket.
        let esc = screen_for("Escalate");
        assert_eq!(esc.unsupported(), vec!["Approver"]);
        let err = transition_payload("51", &esc.fields, &[])
            .expect_err("must refuse rather than post");
        let msg = err.to_string();
        assert!(msg.contains("Approver"), "names the field: {msg}");
        assert!(msg.contains("in Jira"), "says where to do it instead: {msg}");
    }

    #[test]
    fn the_close_payload_puts_the_comment_and_the_resolution_in_the_right_places() {
        // The two are NOT interchangeable: a comment in `fields` is rejected,
        // and a resolution in `update` does not set anything.
        let close = screen_for("Close");
        let payload = transition_payload(
            "41",
            &close.fields,
            &[
                ("resolution".to_string(), FieldInput::Option("10001".to_string())),
                ("comment".to_string(), FieldInput::Text("closing this out".to_string())),
            ],
        )
        .expect("should build");

        assert_eq!(payload["transition"]["id"], "41");
        assert_eq!(payload["fields"]["resolution"]["id"], "10001");
        assert!(payload["fields"].get("comment").is_none(), "comment must not be a field");

        let body = &payload["update"]["comment"][0]["add"]["body"];
        assert_eq!(body["type"], "doc");
        assert_eq!(body["content"][0]["content"][0]["text"], "closing this out");
    }

    #[test]
    fn a_missing_required_value_is_refused_here_not_by_a_400() {
        // A 400 arrives after the click, in Jira's words, and is exactly the
        // experience this prompt exists to replace.
        let close = screen_for("Close");
        let only_comment =
            [("comment".to_string(), FieldInput::Text("note".to_string()))];
        let err = transition_payload("41", &close.fields, &only_comment)
            .expect_err("resolution is missing");
        assert!(err.to_string().contains("Resolution"), "{err}");

        // Whitespace passes an is_empty check and posts a blank comment that
        // notifies everyone on the ticket.
        let blank_comment = [
            ("resolution".to_string(), FieldInput::Option("10001".to_string())),
            ("comment".to_string(), FieldInput::Text("   ".to_string())),
        ];
        assert!(transition_payload("41", &close.fields, &blank_comment).is_err());
    }

    #[test]
    fn an_option_id_must_be_one_jira_actually_offered() {
        // Jira accepts any id that exists, including one belonging to a
        // different field's options -- so a wrong id is a silent wrong write.
        let close = screen_for("Close");
        let bogus = [
            ("resolution".to_string(), FieldInput::Option("99999".to_string())),
            ("comment".to_string(), FieldInput::Text("x".to_string())),
        ];
        assert!(transition_payload("41", &close.fields, &bogus).is_err());
    }

    #[test]
    fn a_transition_with_no_screen_posts_exactly_what_it_always_did() {
        let payload = transition_payload("11", &[], &[]).expect("should build");
        assert_eq!(payload["transition"]["id"], "11");
        assert!(payload.get("fields").is_none());
        assert!(payload.get("update").is_none());
    }

    #[test]
    fn an_array_valued_dropdown_is_wrapped_in_a_list() {
        let fields = [ScreenField {
            key: "components".to_string(),
            name: "Components".to_string(),
            kind: FieldKind::Select {
                options: vec![FieldOption { id: "1".to_string(), label: "api".to_string() }],
                array: true,
            },
        }];
        let payload = transition_payload(
            "11",
            &fields,
            &[("components".to_string(), FieldInput::Option("1".to_string()))],
        )
        .expect("should build");
        assert!(payload["fields"]["components"].is_array(), "{payload}");
        assert_eq!(payload["fields"]["components"][0]["id"], "1");
    }

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
    fn a_comment_body_cannot_break_out_of_its_json() {
        // Arbitrary user text goes into a JSON request body. Built by hand
        // with format!, every one of these would escape the string it was
        // written into -- which is why `comment_adf` uses serde_json.
        for nasty in [
            r#"he said "close it" and left"#,
            r#"path C:\temp\x"#,
            "{\"type\":\"doc\"} nice try",
            "line one\nline two",
            "trailing backslash \\",
        ] {
            let payload = serde_json::json!({ "body": comment_adf(nasty) }).to_string();
            // It is valid JSON...
            let back: Value = serde_json::from_str(&payload).expect("valid json");
            // ...and the text survives byte for byte, rather than being
            // mangled or truncated at a quote.
            assert_eq!(description_text(&back["body"]), nasty.replace("\r\n", "\n"));
        }
    }

    #[test]
    fn a_typed_comment_round_trips_through_adf() {
        // `comment_adf` is the inverse of `description_text`; if they
        // disagree, what you typed is not what the ticket shows.
        let typed = "First line.\n\nAfter a blank line.";
        let adf = comment_adf(typed);
        assert_eq!(adf["type"], "doc");
        assert_eq!(adf["version"], 1);
        assert_eq!(description_text(&adf), typed);
        // A blank line is a paragraph boundary...
        assert_eq!(adf["content"].as_array().expect("array").len(), 2);

        // ...and a single newline is a soft break INSIDE one, the way
        // Shift+Enter behaves in Jira's editor. Mapping it to a paragraph
        // instead turns every typed line break into a blank line, so the
        // posted comment is visibly not what was written.
        let soft = comment_adf("line one\nline two");
        assert_eq!(soft["content"].as_array().expect("array").len(), 1);
        assert_eq!(soft["content"][0]["content"][1]["type"], "hardBreak");
        assert_eq!(description_text(&soft), "line one\nline two");
    }

    fn user(id: &str, name: &str) -> User {
        User { account_id: id.to_string(), display_name: name.to_string() }
    }

    #[test]
    fn a_picked_name_becomes_a_real_mention_and_a_typed_one_does_not() {
        // Typing "@John Smith" by hand produced literal text, because a
        // mention node carries an ACCOUNT ID and nothing had resolved one.
        // That is the bug; this is the fix, and its boundary.
        let picked = [("@John Smith".to_string(), "acc-1".to_string())];
        let adf = comment_adf_with_mentions("@John Smith any update?", &picked);
        let nodes = adf["content"][0]["content"].as_array().expect("inline nodes");
        assert_eq!(nodes[0]["type"], "mention");
        assert_eq!(nodes[0]["attrs"]["id"], "acc-1");
        assert_eq!(nodes[0]["attrs"]["text"], "@John Smith");
        assert_eq!(nodes[1]["type"], "text");
        assert_eq!(nodes[1]["text"], " any update?");

        // A name nobody picked stays text. Without an account id there is
        // nothing to tag, and inventing one notifies a person the app never
        // resolved.
        let unpicked = comment_adf_with_mentions("@Jane Doe any update?", &picked);
        let only = unpicked["content"][0]["content"].as_array().expect("nodes");
        assert_eq!(only.len(), 1);
        assert_eq!(only[0]["type"], "text");
    }

    #[test]
    fn a_longer_name_is_not_eaten_by_a_shorter_one_it_starts_with() {
        // Both are on the ticket and both were picked; matching in map order
        // would tag John Smith and leave "son" as stray text.
        let picked = [
            ("@John Smith".to_string(), "acc-1".to_string()),
            ("@John Smithson".to_string(), "acc-2".to_string()),
        ];
        let adf = comment_adf_with_mentions("@John Smithson please look", &picked);
        let nodes = adf["content"][0]["content"].as_array().expect("nodes");
        assert_eq!(nodes[0]["attrs"]["id"], "acc-2");
        assert_eq!(nodes[1]["text"], " please look");
    }

    #[test]
    fn a_mention_survives_the_round_trip_back_to_text() {
        // `description_text` renders a mention as its own label, so what is
        // posted reads back as what was typed.
        let picked = [("@John Smith".to_string(), "acc-1".to_string())];
        let adf = comment_adf_with_mentions("hi @John Smith\nsecond line", &picked);
        assert_eq!(description_text(&adf), "hi @John Smith\nsecond line");
    }

    #[test]
    fn the_dropdown_offers_the_ticket_s_own_people_before_the_directory() {
        let mut issue = Issue { ..Default::default() };
        issue.reporter = "John Reporter".to_string();
        issue.reporter_id = "acc-rep".to_string();
        issue.assignee = "Ann Assignee".to_string();
        issue.assignee_id = "acc-asg".to_string();
        let comments = vec![
            Comment { author: "Old Commenter".to_string(),
                      author_id: "acc-old".to_string(), ..Default::default() },
            Comment { author: "John Commenter".to_string(),
                      author_id: "acc-new".to_string(), ..Default::default() },
        ];

        // Bare `@` -- reporter, then commenters most recent first, then the
        // assignee. No request is made for this at all.
        let people = ticket_participants(&issue, &comments);
        let ids: Vec<&str> = people.iter().map(|u| u.account_id.as_str()).collect();
        assert_eq!(ids, vec!["acc-rep", "acc-new", "acc-old", "acc-asg"]);

        // Someone filling two roles appears once, at the rank they first
        // earned -- a reporter who has also commented is still the reporter.
        let dual = vec![Comment {
            author: "John Reporter".to_string(),
            author_id: "acc-rep".to_string(),
            ..Default::default()
        }];
        let once = ticket_participants(&issue, &dual);
        let ids: Vec<&str> = once.iter().map(|u| u.account_id.as_str()).collect();
        assert_eq!(ids, vec!["acc-rep", "acc-asg"]);

        // Typing narrows them, still without a request.
        let johns = mention_candidates("john", &people, &[], MENTION_LIMIT);
        let ids: Vec<&str> = johns.iter().map(|u| u.account_id.as_str()).collect();
        assert_eq!(ids, vec!["acc-rep", "acc-new"], "reporter leads");

        // With directory results, participants still outrank them, and a
        // participant the directory also returned is not listed twice.
        let directory = vec![user("acc-far", "John Faraway"), user("acc-rep", "John Reporter")];
        let merged = mention_candidates("john", &people, &directory, MENTION_LIMIT);
        let ids: Vec<&str> = merged.iter().map(|u| u.account_id.as_str()).collect();
        assert_eq!(ids, vec!["acc-rep", "acc-new", "acc-far"]);
        assert!(merged.len() <= MENTION_LIMIT);
    }

    #[test]
    fn a_surname_matches_too_and_the_directory_waits_for_three_letters() {
        let people = vec![user("acc-1", "John Smith")];
        // A surname is typed at least as often as a first name; a
        // whole-string starts_with would find nobody.
        assert_eq!(mention_candidates("smi", &people, &[], 5).len(), 1);
        assert_eq!(mention_candidates("SMITH", &people, &[], 5).len(), 1);
        assert!(mention_candidates("zz", &people, &[], 5).is_empty());

        // Below three characters the ticket's own people answer it, so no
        // request is made.
        assert!(!should_search_users(""));
        assert!(!should_search_users("jo"));
        assert!(should_search_users("joh"));
    }

    #[test]
    fn the_user_search_drops_accounts_that_cannot_be_mentioned() {
        let body = r#"[
          { "accountId": "acc-1", "displayName": "John Smith", "active": true },
          { "accountId": "acc-2", "displayName": "Gone Away", "active": false },
          { "accountId": "acc-3", "displayName": "Automation", "active": true,
            "accountType": "app" },
          { "accountId": "", "displayName": "No id", "active": true }
        ]"#;
        let users = parse_users(body).expect("parses");
        // A deactivated account cannot be notified and a bot wastes one of
        // five slots.
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].account_id, "acc-1");
        assert!(parse_users(r#"{"not":"a list"}"#).is_err());
    }

    #[test]
    fn a_blank_comment_is_refused_before_it_is_posted() {
        // Whitespace passes an is_empty check and posts a blank comment that
        // notifies everyone watching the ticket.
        for blank in ["", "   ", "\n\n", "\t"] {
            assert!(require_comment_text(blank).is_err(), "{blank:?} must be refused");
        }
        assert!(require_comment_text("ok").is_ok());
    }

    const COMMENTS_SAMPLE: &str = r#"{
      "comments": [
        { "author": { "displayName": "A Person" },
          "created": "2026-08-25T09:00:00.000+0100",
          "body": { "type": "doc", "version": 1, "content": [
            { "type": "paragraph", "content": [{"type":"text","text":"first"}] } ] } },
        { "author": null,
          "created": "2026-08-26T10:30:00.000+0100",
          "body": "a v2-style string body" }
      ]
    }"#;

    #[test]
    fn comments_parse_oldest_first_whichever_way_the_body_arrives() {
        let cs = parse_comments(COMMENTS_SAMPLE).expect("parses");
        assert_eq!(cs.len(), 2);
        assert_eq!(cs[0].author, "A Person");
        assert_eq!(cs[0].body, "first");
        // A deleted or app-authored comment can carry no author; that is not
        // a parse failure.
        assert_eq!(cs[1].author, "");
        assert_eq!(cs[1].body, "a v2-style string body");
        assert!(parse_comments(r#"{"comments":[]}"#).expect("empty is valid").is_empty());
        assert!(parse_comments(r#"{"nope":1}"#).is_err());
    }

    #[test]
    fn a_due_date_is_never_shifted_by_a_timezone() {
        // `duedate` is a bare calendar date. Converting it as an instant
        // moves a ticket due the 1st to the 31st for anyone west of UTC --
        // and the app is used across timezones.
        assert_eq!(due_label("2026-09-01"), "2026-09-01");
        // Blank stays blank; anything unexpected is shown verbatim rather
        // than silently dropped.
        assert_eq!(due_label(""), "");
        assert_eq!(due_label("not a date"), "not a date");
    }

    #[test]
    fn overdue_is_relative_to_today_and_only_while_the_ticket_is_open() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 8, 26).expect("valid");
        assert_eq!(due_state("2026-08-25", "indeterminate", today), DueState::Overdue);
        assert_eq!(due_state("2026-08-26", "indeterminate", today), DueState::Today);
        assert_eq!(due_state("2026-08-27", "indeterminate", today), DueState::Future);
        assert_eq!(due_state("", "indeterminate", today), DueState::None);

        // A closed ticket past its date is finished, not late. Colouring the
        // one row needing no attention red is noise.
        assert_eq!(due_state("2026-01-01", "done", today), DueState::Future);
        assert_eq!(due_state("2026-01-01", "DONE", today), DueState::Future);
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
        assert!(fetch_comments(&blank, "OPS-1").is_err());
        assert!(add_comment(&blank, "OPS-1", "hi", &[]).is_err());
        assert!(do_transition(&blank, "OPS-1", "11", &[], &[]).is_err());
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

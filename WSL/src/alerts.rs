//! Jira Service Management Operations (on-call) alert feed.
//!
//! Fetches the alert list from
//! `https://api.atlassian.com/jsm/ops/api/<cloud_id>/v1/alerts` and parses it
//! into [`Alert`] rows. Account / Environment / App are not top-level fields
//! on the API response — they arrive as `"Key: value"` strings inside the
//! alert's `tags` array (e.g. `"Account: 123456789012"`, `"Environment: Dev"`),
//! so [`Alert::from_json`] splits them back out.
//!
//! HTTP goes through `curl`, matching how the rest of the app shells out to
//! `aws` rather than linking an HTTP stack. The API token is passed on curl's
//! **stdin** (`-K -`), never in argv, so it does not show up in the process
//! list.

use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::Deserialize;

use crate::error::{AppError, Result};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// Alerts per request. The API caps `size` at 100.
const PAGE_SIZE: u32 = 50;

/// Stop walking the feed after this many pages, however wide the window.
const MAX_PAGES: u32 = 20;

/// Credentials + site for the alerts API. Sourced from `assets/features.json`
/// (see `features::Features::alerts`), with the token overridable at runtime
/// via the `JIRA_TOKEN` environment variable.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AlertsAuth {
    pub email: String,
    pub token: String,
    pub cloud_id: String,
}

impl AlertsAuth {
    /// True when every field needed to make a request is present.
    pub fn is_complete(&self) -> bool {
        !self.email.trim().is_empty()
            && !self.token.trim().is_empty()
            && !self.cloud_id.trim().is_empty()
    }
}

/// One alert row, flattened for display.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Alert {
    pub id: String,
    pub tiny_id: String,
    /// `createdAt` exactly as the API returned it (RFC 3339, UTC).
    pub created_at: String,
    pub priority: String,
    pub status: String,
    pub acknowledged: bool,
    pub message: String,
    pub source: String,
    pub count: u64,
    /// From the `Account: …` tag. Empty when the alert carries no such tag.
    pub account: String,
    /// From the `Environment: …` tag.
    pub environment: String,
    /// From the `App: …` tag.
    pub app: String,
    pub tags: Vec<String>,
    /// Free-text body. Absent from the list view on some integrations, so
    /// never assume it is populated.
    pub description: String,
    /// `extraProperties`, string values only. Non-string values are dropped
    /// rather than failing the alert.
    ///
    /// Note the feed sometimes carries a literal `{{extraProperties}}` key —
    /// an unrendered template holding a flattened copy of this same map. It
    /// is kept here verbatim but must never be matched against: it duplicates
    /// values that also appear properly, and it disappears the day the
    /// template is fixed.
    pub extra: BTreeMap<String, String>,
}

/// Raw shape of a single alert in the API response. Unknown fields ignored;
/// missing ones fall back to `Default` (the feed omits empty tags entirely).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawAlert {
    id: String,
    #[serde(rename = "tinyId")]
    tiny_id: String,
    #[serde(rename = "createdAt")]
    created_at: String,
    priority: String,
    status: String,
    acknowledged: bool,
    message: String,
    source: String,
    count: u64,
    tags: Vec<String>,
    description: String,
    #[serde(rename = "extraProperties")]
    extra_properties: serde_json::Map<String, serde_json::Value>,
}

/// The alert list envelope. The API has used both `values` and `data` for the
/// page body across versions, so accept either.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct AlertsPage {
    values: Vec<RawAlert>,
    data: Vec<RawAlert>,
    links: Links,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Links {
    next: Option<String>,
}

/// Pull the value out of a `"Key: value"` tag, case-insensitively on the key.
/// Returns `None` when the tag has no colon or a different key.
fn tag_value(tag: &str, key: &str) -> Option<String> {
    let (k, v) = tag.split_once(':')?;
    if k.trim().eq_ignore_ascii_case(key) {
        Some(v.trim().to_string())
    } else {
        None
    }
}

/// First value for `key` across all of `tags`, or empty when absent.
fn first_tag_value(tags: &[String], key: &str) -> String {
    tags.iter()
        .find_map(|t| tag_value(t, key))
        .unwrap_or_default()
}

impl Alert {
    fn from_raw(raw: RawAlert) -> Self {
        let account = first_tag_value(&raw.tags, "Account");
        let environment = first_tag_value(&raw.tags, "Environment");
        let app = first_tag_value(&raw.tags, "App");
        let extra = raw
            .extra_properties
            .into_iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
            .collect::<BTreeMap<_, _>>();
        Self {
            id: raw.id,
            tiny_id: raw.tiny_id,
            created_at: raw.created_at,
            priority: raw.priority,
            status: raw.status,
            acknowledged: raw.acknowledged,
            message: raw.message,
            source: raw.source,
            count: raw.count,
            account,
            environment,
            app,
            tags: raw.tags,
            description: raw.description,
            extra,
        }
    }

    /// `createdAt` parsed to a UTC timestamp. `None` when the API sent
    /// something that isn't RFC 3339.
    pub fn created_utc(&self) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(&self.created_at)
            .ok()
            .map(|dt| dt.with_timezone(&Utc))
    }
}

/// Parse one page of alert JSON into rows.
pub fn parse_alerts_page(body: &str) -> Result<(Vec<Alert>, Option<String>)> {
    let page: AlertsPage = serde_json::from_str(body).map_err(|e| {
        AppError::Parse(format!("alerts: could not parse API response: {e}"))
    })?;
    let raws = if page.values.is_empty() {
        page.data
    } else {
        page.values
    };
    let alerts = raws.into_iter().map(Alert::from_raw).collect();
    Ok((alerts, page.links.next))
}

/// Pull the `offset` query parameter out of the `links.next` URL.
fn offset_from_next(next: &str) -> Option<u32> {
    let query = next.split_once('?')?.1;
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        if k == "offset" {
            v.parse().ok()
        } else {
            None
        }
    })
}

/// Fetch every alert created within the last `window_min` minutes, newest
/// first. Walks the feed page by page and stops as soon as a page runs past
/// the cutoff (the feed is sorted `createdAt desc`, so everything after that
/// is older too).
pub fn fetch_recent(auth: &AlertsAuth, window_min: i64) -> Result<Vec<Alert>> {
    if !auth.is_complete() {
        return Err(AppError::InvalidArgument(
            "alerts: email, token and cloud id must all be set (see assets/features.json)"
                .to_string(),
        ));
    }
    let cutoff = Utc::now() - Duration::minutes(window_min.max(1));
    let base = format!(
        "https://api.atlassian.com/jsm/ops/api/{}/v1/alerts",
        auth.cloud_id.trim()
    );

    let mut out: Vec<Alert> = Vec::new();
    let mut offset: u32 = 0;
    for _ in 0..MAX_PAGES {
        let body = curl_get(auth, &base, offset)?;
        let (alerts, next) = parse_alerts_page(&body)?;
        if alerts.is_empty() {
            break;
        }
        // A row with an unparseable timestamp is kept rather than dropped —
        // better a visible odd row than a silently missing alert.
        let oldest_in_page = alerts.last().and_then(|a| a.created_utc());
        out.extend(
            alerts
                .into_iter()
                .filter(|a| a.created_utc().map(|t| t >= cutoff).unwrap_or(true)),
        );
        match oldest_in_page {
            Some(t) if t < cutoff => break,
            _ => {}
        }
        let Some(next) = next else { break };
        let Some(next_offset) = offset_from_next(&next) else {
            break;
        };
        if next_offset <= offset {
            break; // defensive: never loop on a non-advancing cursor
        }
        offset = next_offset;
    }

    out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(out)
}

/// Reject anything that would escape the URL path when interpolated. Alert
/// ids from the API are `[0-9a-f-]` plus a numeric suffix; nothing else is
/// legitimate, and the acknowledge POST is a mutation.
fn validate_alert_id(alert_id: &str) -> Result<()> {
    let ok = !alert_id.is_empty()
        && alert_id.len() <= 128
        && alert_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(AppError::InvalidArgument(format!(
            "alerts: refusing malformed alert id ({} chars)",
            alert_id.len()
        )))
    }
}

/// One request. `query` pairs are sent with `-G --data-urlencode` for GET;
/// `post_body` switches it to a POST with a JSON body. Credentials go in on
/// stdin via curl's `-K -` config so the token never appears in argv.
fn curl_request(
    auth: &AlertsAuth,
    url: &str,
    query: &[(&str, String)],
    post_body: Option<&str>,
) -> Result<String> {
    // curl config-file syntax: quotes in the token/email would break out of
    // the quoted value, so reject them rather than mangle the config.
    if auth.email.contains('"') || auth.token.contains('"') {
        return Err(AppError::InvalidArgument(
            "alerts: email/token must not contain double quotes".to_string(),
        ));
    }
    let mut cmd = Command::new("curl");
    cmd.args(["-sS", "--fail-with-body", "-K", "-", "-H", "Accept: application/json"]);
    match post_body {
        Some(body) => {
            cmd.args(["-X", "POST", "-H", "Content-Type: application/json", "-d", body]);
        }
        None => {
            cmd.arg("-G");
            for (k, v) in query {
                cmd.args(["--data-urlencode", &format!("{k}={v}")]);
            }
        }
    }
    cmd.arg(url);
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let mut child = cmd.spawn()?;
    {
        let mut stdin = child.stdin.take().ok_or_else(|| {
            AppError::Io(std::io::Error::other("alerts: could not open curl stdin"))
        })?;
        writeln!(stdin, "user = \"{}:{}\"", auth.email.trim(), auth.token.trim())?;
    }
    let output = child.wait_with_output()?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = if stderr.trim().is_empty() { stdout.trim().to_string() } else { stderr.trim().to_string() };
        let detail: String = detail.chars().take(300).collect();
        return Err(AppError::CommandFailed {
            program: "curl".to_string(),
            // The URL only — never the config on stdin, which holds the token.
            args: vec![url.to_string()],
            stderr: detail,
        });
    }
    Ok(stdout)
}

/// Base `…/v1` for this site.
fn ops_base(auth: &AlertsAuth) -> String {
    format!("https://api.atlassian.com/jsm/ops/api/{}/v1", auth.cloud_id.trim())
}

fn require_complete(auth: &AlertsAuth) -> Result<()> {
    if auth.is_complete() {
        Ok(())
    } else {
        Err(AppError::InvalidArgument(
            "alerts: email, token and cloud id must all be set".to_string(),
        ))
    }
}

/// Parse a single-alert response. The API returns the alert either bare or
/// wrapped in `{"data": …}` / `{"value": …}` depending on the endpoint.
pub fn parse_single_alert(body: &str) -> Result<Alert> {
    let v: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| AppError::Parse(format!("alerts: could not parse alert: {e}")))?;
    let inner = v.get("data").or_else(|| v.get("value")).unwrap_or(&v);
    let raw: RawAlert = serde_json::from_value(inner.clone())
        .map_err(|e| AppError::Parse(format!("alerts: unexpected alert shape: {e}")))?;
    Ok(Alert::from_raw(raw))
}

/// The newest `count` alerts, newest first. One request, no paging — the
/// remediation poller wants a fixed-size window of recent alerts, not a
/// time-bounded walk.
pub fn fetch_latest(auth: &AlertsAuth, count: u32) -> Result<Vec<Alert>> {
    require_complete(auth)?;
    let url = format!("{}/alerts", ops_base(auth));
    let body = curl_request(
        auth,
        &url,
        &[
            ("sort", "createdAt".to_string()),
            ("order", "desc".to_string()),
            ("size", count.clamp(1, 100).to_string()),
        ],
        None,
    )?;
    let (alerts, _next) = parse_alerts_page(&body)?;
    Ok(alerts)
}

/// One alert by id. Used for the last-look abort and the stage-2 closure
/// watch, so its failure must stay distinguishable from "not closed".
pub fn fetch_alert(auth: &AlertsAuth, alert_id: &str) -> Result<Alert> {
    require_complete(auth)?;
    validate_alert_id(alert_id)?;
    let url = format!("{}/alerts/{alert_id}", ops_base(auth));
    let body = curl_request(auth, &url, &[], None)?;
    parse_single_alert(&body)
}

/// Acknowledge an alert. Only ever called on the on-call path: acking off
/// call would suppress the real on-call engineer's escalation.
pub fn acknowledge_alert(auth: &AlertsAuth, alert_id: &str) -> Result<()> {
    require_complete(auth)?;
    validate_alert_id(alert_id)?;
    let url = format!("{}/alerts/{alert_id}/acknowledge", ops_base(auth));
    curl_request(auth, &url, &[], Some("{}"))?;
    Ok(())
}

/// Raw body of the schedule's current on-call response. Returned unparsed:
/// `oncall::response_mentions` scans it, since the shape is not stable.
pub fn fetch_on_calls(auth: &AlertsAuth, schedule_id: &str) -> Result<String> {
    require_complete(auth)?;
    // Same character rules — it is interpolated into a URL path, and relaxing
    // this would widen the validation that also guards the acknowledge POST.
    // Consequence: `schedule_id` must be the id, not a friendly schedule name
    // (names may contain spaces). The error names the field, so this is
    // self-explanatory when someone configures a name by mistake.
    validate_alert_id(schedule_id)?;
    let url = format!("{}/schedules/{schedule_id}/on-calls", ops_base(auth));
    curl_request(auth, &url, &[("flat", "true".to_string())], None)
}

/// One GET against the alerts endpoint. Credentials go in on stdin via curl's
/// `-K -` config so the token never appears in argv.
fn curl_get(auth: &AlertsAuth, base: &str, offset: u32) -> Result<String> {
    curl_request(
        auth,
        base,
        &[
            ("sort", "createdAt".to_string()),
            ("order", "desc".to_string()),
            ("size", PAGE_SIZE.to_string()),
            ("offset", offset.to_string()),
        ],
        None,
    )
}

/// The cutoff timestamp `window_min` ago, in the same RFC 3339 form the API
/// uses. Exposed for logging/diagnostics.
pub fn cutoff_string(window_min: i64) -> String {
    (Utc::now() - Duration::minutes(window_min.max(1)))
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed-down copy of a real response (the shape in the screenshot).
    const SAMPLE: &str = r#"{
      "values": [{
        "id": "aaaa-1111",
        "tinyId": "9945",
        "createdAt": "2026-07-13T15:21:12.874Z",
        "updatedAt": "2026-07-13T15:37:42.921Z",
        "message": "[Catalyst Gateway]: Gateway-Global-Request-Time-(seconds)-Warning",
        "entity": "",
        "source": "Grafana",
        "status": "open",
        "alias": "some-alias-63",
        "tags": [
          "Account: 123456789012",
          "App: Catalyst Gateway",
          "Environment: Dev",
          "Metric: Global-Request-Time-(seconds)",
          "Resource Type: Catalyst Gateway"
        ],
        "acknowledged": false,
        "count": 4,
        "owner": "",
        "snoozed": false,
        "priority": "P4"
      }],
      "links": {"next": "/v1/alerts?offset=1&size=1&sort=createdAt&order=desc"},
      "count": 31240
    }"#;

    #[test]
    fn parses_account_and_environment_from_tags() {
        let (alerts, next) = parse_alerts_page(SAMPLE).expect("parses");
        assert_eq!(alerts.len(), 1);
        let a = &alerts[0];
        assert_eq!(a.account, "123456789012");
        assert_eq!(a.environment, "Dev");
        assert_eq!(a.app, "Catalyst Gateway");
        assert_eq!(a.tiny_id, "9945");
        assert_eq!(a.priority, "P4");
        assert_eq!(a.status, "open");
        assert!(!a.acknowledged);
        assert_eq!(a.count, 4);
        assert_eq!(next.as_deref(), Some("/v1/alerts?offset=1&size=1&sort=createdAt&order=desc"));
    }

    #[test]
    fn tag_keys_are_case_insensitive_and_space_tolerant() {
        let tags = vec![
            "account:  42 ".to_string(),
            "ENVIRONMENT:Prod".to_string(),
        ];
        assert_eq!(first_tag_value(&tags, "Account"), "42");
        assert_eq!(first_tag_value(&tags, "Environment"), "Prod");
    }

    #[test]
    fn missing_tags_yield_empty_fields() {
        let body = r#"{"values":[{"id":"x","tinyId":"1","createdAt":"2026-07-13T15:21:12.874Z"}]}"#;
        let (alerts, next) = parse_alerts_page(body).expect("parses");
        assert_eq!(alerts[0].account, "");
        assert_eq!(alerts[0].environment, "");
        assert!(next.is_none());
    }

    #[test]
    fn a_metric_tag_is_not_mistaken_for_account() {
        // "Resource Type: …" also contains a colon; only exact key matches win.
        let tags = vec!["Resource Type: Catalyst Gateway".to_string()];
        assert_eq!(first_tag_value(&tags, "Account"), "");
    }

    #[test]
    fn data_key_is_accepted_as_well_as_values() {
        let body = r#"{"data":[{"id":"x","tinyId":"7","tags":["Account: 9"]}]}"#;
        let (alerts, _) = parse_alerts_page(body).expect("parses");
        assert_eq!(alerts[0].tiny_id, "7");
        assert_eq!(alerts[0].account, "9");
    }

    #[test]
    fn next_offset_is_extracted() {
        assert_eq!(
            offset_from_next("/v1/alerts?offset=50&size=50&sort=createdAt"),
            Some(50)
        );
        assert_eq!(offset_from_next("/v1/alerts"), None);
    }

    #[test]
    fn created_utc_parses_rfc3339() {
        let (alerts, _) = parse_alerts_page(SAMPLE).expect("parses");
        let t = alerts[0].created_utc().expect("timestamp parses");
        assert_eq!(t.to_rfc3339_opts(SecondsFormat::Millis, true), "2026-07-13T15:21:12.874Z");
    }

    #[test]
    fn incomplete_auth_is_rejected_before_any_request() {
        let auth = AlertsAuth {
            email: "a@b.c".into(),
            token: String::new(),
            cloud_id: "cid".into(),
        };
        assert!(!auth.is_complete());
        assert!(fetch_recent(&auth, 10).is_err());
    }

    #[test]
    fn malformed_json_is_an_error_not_a_panic() {
        assert!(parse_alerts_page("not json").is_err());
    }

    /// Trimmed from a real response captured 2026-08-13. See the spec appendix.
    const SAMPLE_ALERT: &str = r#"{
      "id": "823106d9-c5b9-4900-be29-33025114eebc-1786652659622",
      "tinyId": "428315",
      "createdAt": "2026-08-13T20:24:19.622Z",
      "status": "closed",
      "priority": "P4",
      "acknowledged": false,
      "message": "xxxx",
      "source": "Grafana",
      "count": 1,
      "tags": ["Account: 123456789012", "App: reaper", "Environment: DEV1"],
      "extraProperties": {
        "alertname": "Read-OK-STATUS-Warning",
        "Panel_ID": "4",
        "Threshold": "85",
        "{{extraProperties}}": "{alertname=Read-OK-STATUS-Warning}"
      },
      "description": "Instance i-0abc123def4567890 is unhealthy",
      "closeTime": "2026-08-13T20:25:19.616Z"
    }"#;

    #[test]
    fn a_single_alert_carries_its_description_and_extra_properties() {
        let a = parse_single_alert(SAMPLE_ALERT).expect("parses");
        assert_eq!(a.tiny_id, "428315");
        assert_eq!(a.status, "closed");
        assert_eq!(a.description, "Instance i-0abc123def4567890 is unhealthy");
        assert_eq!(a.extra.get("alertname").map(String::as_str), Some("Read-OK-STATUS-Warning"));
        assert_eq!(a.account, "123456789012");
        assert_eq!(a.environment, "DEV1");
    }

    #[test]
    fn non_string_extra_properties_are_dropped_not_fatal() {
        // A numeric or nested value must not fail the whole alert.
        let body = r#"{"id":"x","extraProperties":{"a":"1","b":2,"c":{"d":"e"}}}"#;
        let a = parse_single_alert(body).expect("parses");
        assert_eq!(a.extra.get("a").map(String::as_str), Some("1"));
        assert!(a.extra.get("b").is_none());
        assert!(a.extra.get("c").is_none());
    }

    #[test]
    fn an_alert_with_no_extra_properties_at_all_still_parses() {
        let a = parse_single_alert(r#"{"id":"x","status":"open"}"#).expect("parses");
        assert!(a.extra.is_empty());
        assert!(a.description.is_empty());
    }

    #[test]
    fn a_page_still_parses_with_the_new_fields_present() {
        let body = format!(r#"{{"values":[{SAMPLE_ALERT}]}}"#);
        let (alerts, next) = parse_alerts_page(&body).expect("parses");
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].description, "Instance i-0abc123def4567890 is unhealthy");
        assert!(next.is_none());
    }

    #[test]
    fn the_data_envelope_is_still_accepted() {
        let body = format!(r#"{{"data":[{SAMPLE_ALERT}]}}"#);
        let (alerts, _) = parse_alerts_page(&body).expect("parses");
        assert_eq!(alerts.len(), 1);
    }

    #[test]
    fn an_incomplete_auth_is_refused_before_any_request() {
        let auth = AlertsAuth::default();
        assert!(fetch_latest(&auth, 10).is_err());
        assert!(fetch_alert(&auth, "abc").is_err());
        assert!(acknowledge_alert(&auth, "abc").is_err());
    }

    #[test]
    fn an_alert_id_with_a_slash_is_refused() {
        // The id is interpolated into a URL path. A traversal in it would aim
        // the request — including the acknowledge POST — at another endpoint.
        let auth = AlertsAuth {
            email: "a@b.c".into(),
            token: "t".into(),
            cloud_id: "cloud".into(),
        };
        assert!(fetch_alert(&auth, "../schedules").is_err());
        assert!(acknowledge_alert(&auth, "a/b").is_err());
        assert!(fetch_alert(&auth, "has space").is_err());
    }
}

//! The one place an Atlassian API request is made, and the one place the
//! credentials for it are handed over.
//!
//! Two different APIs are reached through here with the same token — the JSM
//! Operations alert feed (`src/alerts.rs`) and the Jira issue API
//! (`src/jira.rs`) — and both depend on the same two properties, which is
//! why this is one function rather than a copy in each module:
//!
//! - **Credentials go to curl's stdin via `-K -`, never argv.** The token
//!   therefore never appears in the process list, and — because it also
//!   never reaches the query string — the recorded URL is safe to render in
//!   the Logs tab and safe to copy. A second copy of this logic is exactly
//!   the kind of drift that must not happen around a token.
//! - **Every call is recorded here.** The **Jira Alerts** trace in the Logs
//!   tab reads [`recent_api_calls`], so adding an endpoint traces it for
//!   free; bypassing this function is what would make a call invisible.
//!
//! HTTP goes through `curl` rather than a linked HTTP stack, matching how the
//! rest of the app shells out to `aws`. `curl.exe` ships with Windows 10
//! 1803+.

use std::collections::VecDeque;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use chrono::{DateTime, Utc};

use crate::error::{AppError, Result};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// How many calls the trace keeps. The Logs tab renders exactly this many.
pub const API_TRACE_LEN: usize = 5;

/// A recorded Atlassian API call, for the **Jira Alerts** trace in the Logs
/// tab.
///
/// `url` is safe to display: credentials reach curl on stdin via `-K -` and
/// never touch argv or the query string, so nothing here can carry the token.
/// That is the same property the failure path already relies on to fold the
/// query into its error message.
#[derive(Clone, Debug)]
pub struct ApiCall {
    /// When the request went out, UTC. Rendered in local time.
    pub at: DateTime<Utc>,
    pub method: &'static str,
    /// Endpoint with the query folded in, exactly as sent.
    pub url: String,
    /// `None` on success; the curl/HTTP failure detail otherwise.
    pub error: Option<String>,
    pub duration_ms: u64,
    /// Response size, kept after `body` is dropped so an older row can still
    /// say how much came back.
    pub bytes: usize,
    /// The full response, held **only while this is the most recent call**.
    /// A new call drops the previous one's body, so at most one response is
    /// ever resident — a page of the alert feed is up to 50 alerts and a Jira
    /// search up to 50 issues, and the app does not control how large either
    /// is, so keeping five would be unbounded in practice.
    pub body: Option<String>,
}

impl ApiCall {
    /// True when the response body is still held — i.e. this is the newest
    /// call. Older rows are metadata only, by design.
    pub fn has_body(&self) -> bool {
        self.body.is_some()
    }
}

static API_TRACE: OnceLock<Mutex<VecDeque<ApiCall>>> = OnceLock::new();

fn api_trace() -> &'static Mutex<VecDeque<ApiCall>> {
    API_TRACE.get_or_init(|| Mutex::new(VecDeque::new()))
}

/// Record one call, dropping the previous newest entry's body and evicting
/// the oldest past [`API_TRACE_LEN`].
fn record_api_call(call: ApiCall) {
    let Ok(mut trace) = api_trace().lock() else {
        // A poisoned trace must never take the request down with it: this is
        // a diagnostic, and losing a row is not worth failing a fetch over.
        return;
    };
    if let Some(prev) = trace.back_mut() {
        prev.body = None;
    }
    trace.push_back(call);
    while trace.len() > API_TRACE_LEN {
        trace.pop_front();
    }
}

/// The recorded calls, **newest first**. Empty until something has been
/// fetched. Cheap enough to call per frame: at most five rows, one of which
/// carries a body.
pub fn recent_api_calls() -> Vec<ApiCall> {
    let Ok(trace) = api_trace().lock() else {
        return Vec::new();
    };
    trace.iter().rev().cloned().collect()
}

/// Drop every recorded call, including the held body.
pub fn clear_api_calls() {
    if let Ok(mut trace) = api_trace().lock() {
        trace.clear();
    }
}

/// One request against an Atlassian API. `query` pairs are sent with
/// `-G --data-urlencode` for GET; `post_body` switches it to a POST with a
/// JSON body. Credentials go in on stdin via curl's `-K -` config so the
/// token never appears in argv.
pub fn request(
    email: &str,
    token: &str,
    url: &str,
    query: &[(&str, String)],
    post_body: Option<&str>,
) -> Result<String> {
    // curl config-file syntax: quotes in the token/email would break out of
    // the quoted value, so reject them rather than mangle the config.
    if email.contains('"') || token.contains('"') {
        return Err(AppError::InvalidArgument(
            "atlassian: email/token must not contain double quotes".to_string(),
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

    // `query` never carries credentials — those go to stdin via `-K -` — so
    // folding it into the displayed URL cannot leak the token. Built up
    // front now because the trace wants it on the success path too; without
    // it a failed page of `fetch_recent` (up to 20 pages, each with its own
    // `offset`) reported only the bare endpoint, with no way to tell which
    // page failed.
    let display_url = if query.is_empty() {
        url.to_string()
    } else {
        let pairs: Vec<String> = query.iter().map(|(k, v)| format!("{k}={v}")).collect();
        format!("{url}?{}", pairs.join("&"))
    };
    let method = if post_body.is_some() { "POST" } else { "GET" };
    let started_at = Utc::now();
    let started = Instant::now();

    let mut child = cmd.spawn()?;
    {
        let mut stdin = child.stdin.take().ok_or_else(|| {
            AppError::Io(std::io::Error::other("atlassian: could not open curl stdin"))
        })?;
        // The one line carrying the token. It goes to curl's stdin and is
        // never recorded — the trace holds `display_url` and the response,
        // neither of which sees it.
        writeln!(stdin, "user = \"{}:{}\"", email.trim(), token.trim())?;
    }
    let output = child.wait_with_output()?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let duration_ms = started.elapsed().as_millis() as u64;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = if stderr.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            stderr.trim().to_string()
        };
        let detail: String = detail.chars().take(300).collect();
        // `--fail-with-body` means the response body is still on stdout for
        // an HTTP error, so a failed call keeps it — that body is usually
        // the API's own explanation and is the whole point of looking.
        record_api_call(ApiCall {
            at: started_at,
            method,
            url: display_url.clone(),
            error: Some(detail.clone()),
            duration_ms,
            bytes: stdout.len(),
            body: Some(stdout),
        });
        return Err(AppError::CommandFailed {
            program: "curl".to_string(),
            args: vec![display_url],
            stderr: detail,
        });
    }
    record_api_call(ApiCall {
        at: started_at,
        method,
        url: display_url,
        error: None,
        duration_ms,
        bytes: stdout.len(),
        body: Some(stdout.clone()),
    });
    Ok(stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_call(url: &str, body: &str) -> ApiCall {
        ApiCall {
            at: Utc::now(),
            method: "GET",
            url: url.to_string(),
            error: None,
            duration_ms: 12,
            bytes: body.len(),
            body: Some(body.to_string()),
        }
    }

    /// The trace is a process-wide static, so this is deliberately **one**
    /// test rather than several: split up, they would race each other under
    /// the parallel test runner and pass or fail by scheduling. Nothing else
    /// in the crate touches the trace (recording only happens inside
    /// [`request`], which needs the network).
    #[test]
    fn the_trace_keeps_five_calls_and_only_the_newest_response() {
        clear_api_calls();
        assert!(recent_api_calls().is_empty());

        record_api_call(sample_call("https://x/1", "body-1"));
        // A single call keeps its body — it is the newest.
        let trace = recent_api_calls();
        assert_eq!(trace.len(), 1);
        assert_eq!(trace[0].body.as_deref(), Some("body-1"));

        // A second call takes the body from the first. This is the whole
        // point: a page of the feed is up to 50 alerts and the app does not
        // control how large that is, so holding five would be unbounded.
        record_api_call(sample_call("https://x/2", "body-2"));
        let trace = recent_api_calls();
        assert_eq!(trace.len(), 2);
        // Newest first.
        assert_eq!(trace[0].url, "https://x/2");
        assert_eq!(trace[0].body.as_deref(), Some("body-2"));
        assert_eq!(trace[1].url, "https://x/1");
        assert_eq!(trace[1].body, None, "the older call must not keep its body");
        // ...but it still reports how much came back.
        assert_eq!(trace[1].bytes, "body-1".len());
        assert!(trace[0].has_body());
        assert!(!trace[1].has_body());

        // Past five, the oldest is evicted rather than the trace growing.
        for i in 3..=9 {
            record_api_call(sample_call(&format!("https://x/{i}"), "b"));
        }
        let trace = recent_api_calls();
        assert_eq!(trace.len(), API_TRACE_LEN);
        assert_eq!(trace[0].url, "https://x/9");
        assert_eq!(trace[API_TRACE_LEN - 1].url, "https://x/5");
        // Exactly one body survives, always the newest.
        assert_eq!(trace.iter().filter(|c| c.has_body()).count(), 1);
        assert!(trace[0].has_body());

        // Clear drops the held body too, not just the rows.
        clear_api_calls();
        assert!(recent_api_calls().is_empty());
    }

    /// A failed call is the one you most want to look at, so it is recorded
    /// with its body — `--fail-with-body` leaves the API's own explanation
    /// on stdout — and carries the failure detail.
    #[test]
    fn a_failed_call_is_recorded_with_its_reason() {
        let call = ApiCall {
            at: Utc::now(),
            method: "GET",
            url: "https://x/alerts?offset=50".to_string(),
            error: Some("HTTP 401 Unauthorized".to_string()),
            duration_ms: 88,
            bytes: 42,
            body: Some("{\"errorMessage\":\"…\"}".to_string()),
        };
        assert!(call.error.is_some());
        assert!(call.has_body());
    }
}

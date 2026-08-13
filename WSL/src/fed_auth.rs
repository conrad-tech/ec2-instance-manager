//! Classification and scheduling for the automatic `fed up` refresh.
//!
//! The GUI runs the corporate `fed up` command when the cached credentials
//! expire, so the user never has to remember to refresh them by hand.
//! This module holds the parts that can be reasoned about without a Windows
//! box or an Okta tenant: what one run's output *meant*, and when to run it
//! again.
//!
//! Runs are driven by **credential expiry**, not a timer: `fed_expire` in
//! `~/.aws/credentials` already says when the credentials die, and that file
//! is watched. Backdating it is therefore a realistic test.
//!
//! Signing in is the user's job unless `fed_auth.auto_sign_in` is on. By
//! default the app opens the activation URL, puts the code on the clipboard
//! and stops — nothing here stores a password or synthesises a keystroke.
//!
//! **The output patterns are best-effort.** `fed` is a corporate tool that
//! could not be run while this was written, so the classification leans on
//! the process exit code first and treats the text as corroboration. Every
//! run's raw output is logged by the caller precisely so these patterns can
//! be tightened against the real thing.

use std::time::Duration;

/// The verdict for a **completed** `fed up`.
///
/// A device-authorization prompt is not a verdict — `fed` prints it partway
/// through and then blocks waiting for approval — so it is handled while the
/// output streams (see [`parse_device_code`]) and does not appear here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FedOutcome {
    /// Exited cleanly — credentials are good.
    Authenticated,
    /// The run failed. Carries a one-line summary for the status panel.
    Failed(String),
}

/// Why the automatic refresh failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FedError {
    /// `fed up` reported an error, or could not be run at all.
    Command(String),
    /// The automatic sign-in named a problem precisely: the focus guard
    /// tripped, Chrome was missing, the vault had nothing to read.
    Browser(String),
    /// The sign-in walk ran to completion but `fed up` still did not
    /// authenticate.
    ///
    /// An **inference**, not a reading of the page: SendKeys cannot see the
    /// DOM, so a password Okta rejected is invisible. What we can say is that
    /// every keystroke went in and it still failed.
    NotAuthenticated,
    /// The retry window closed with every attempt failing. Carries the last
    /// error, so the status line still names a cause rather than just
    /// "gave up".
    GaveUp(Box<FedError>),
}

impl std::fmt::Display for FedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Command(msg) => write!(f, "fed up failed: {msg}"),
            Self::Browser(msg) => write!(f, "the automatic sign-in failed: {msg}"),
            Self::NotAuthenticated => write!(
                f,
                "the sign-in completed but fed up did not authenticate — the saved \
                 password was most likely rejected on the Okta page. Re-save it from \
                 the Scripts menu."
            ),
            Self::GaveUp(last) => write!(
                f,
                "gave up after the retry window — last error: {last}. \
                 Sign in manually; the automatic refresh arms itself again as \
                 soon as your credentials are renewed."
            ),
        }
    }
}

/// Timings for the retry loop. Field values come from features.json so a site
/// with a slower `fed` can widen them without a rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Gap between attempts while retrying after a failure.
    pub interval: Duration,
    /// Total time to keep retrying before giving up and waiting for the user.
    pub window: Duration,
    /// Gap between attempts when the failure is [`is_access_pending`] -- the
    /// sign-in worked and only an account entitlement is missing. Shorter,
    /// because that resolves on its own in well under the general interval.
    pub access_interval: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(120),
            window: Duration::from_secs(600),
            access_interval: Duration::from_secs(30),
        }
    }
}

/// Text that means the run failed, matched case-insensitively.
///
/// Only consulted when the exit code is unavailable — a process that exited 0
/// is taken at its word, since ordinary progress output ("0 errors") would
/// otherwise trip these.
const FAILURE_MARKERS: &[&str] = &[
    "error",
    "failed",
    "unable to",
    "unauthorized",
    "access denied",
    "timed out",
    "timeout",
    "not logged in",
    "could not",
];

/// Pull the activation URL and code out of `fed up`'s device-authorization
/// line, e.g.
///
/// ```text
/// Go to https://example.okta.com/activate and enter code MXKD-9QRP
/// ```
///
/// Fed one line at a time while the command streams, so the browser opens
/// while `fed` is still waiting rather than after it gives up.
///
/// Deliberately loose: it takes the first `https://` token and the first
/// token after "enter code", rather than matching the sentence as a whole, so
/// a reworded prompt keeps working. Returns `None` unless **both** are found
/// — a URL with no code is nothing the user can act on.
pub fn parse_device_code(text: &str) -> Option<(String, String)> {
    let url = text
        .split_whitespace()
        .find(|t| t.starts_with("https://") || t.starts_with("http://"))
        .map(|t| t.trim_end_matches(['.', ',', ')', '"', '\'']).to_string())?;

    let lower = text.to_ascii_lowercase();
    let idx = lower.find("enter code")? + "enter code".len();
    let code = text[idx..]
        .split_whitespace()
        .next()?
        .trim_end_matches(['.', ',', ')', '"', '\''])
        .to_string();
    if code.is_empty() {
        return None;
    }
    Some((url, code))
}

/// Interpret a finished `fed up`.
///
/// `output` is stdout and stderr together; `exit_code` is `None` when the
/// process could not be waited on. Any device-authorization prompt in the
/// output is ignored here — by the time the process has exited, whether the
/// user completed the sign-in is what the exit code says.
pub fn classify_exit(output: &str, exit_code: Option<i32>) -> FedOutcome {
    match exit_code {
        Some(0) => FedOutcome::Authenticated,
        Some(code) => FedOutcome::Failed(
            last_meaningful_line(output)
                .unwrap_or_else(|| format!("fed up exited with code {code}")),
        ),
        None => {
            let lower = output.to_ascii_lowercase();
            if FAILURE_MARKERS.iter().any(|m| lower.contains(m)) {
                FedOutcome::Failed(
                    last_meaningful_line(output)
                        .unwrap_or_else(|| "fed up reported an error".to_string()),
                )
            } else {
                FedOutcome::Authenticated
            }
        }
    }
}

/// The line to show the user for a failure: the last non-blank line, which is
/// where a CLI usually puts its error.
fn last_meaningful_line(output: &str) -> Option<String> {
    output
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .map(|l| l.chars().take(200).collect())
}

/// One line of `fed_login.ps1`'s output, when the automatic sign-in is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptEvent {
    /// Progress: `opening-browser`, `entering-code`, `entering-password`, …
    Status(String),
    /// A named failure. These are the precise ones — surface them verbatim.
    Error(String),
    /// A `-DryRun` report of a keystroke that would have been sent.
    DryRun(String),
    /// A diagnostic from the script about what it could and could not see —
    /// whether it managed to focus the activation box, whether the code
    /// arrived prefilled. Belongs in the log, not on the status line: it
    /// explains a sign-in after the fact rather than reporting progress.
    Note(String),
    /// The window handle the sign-in drove, so the caller can close that one
    /// window afterwards.
    Window(isize),
}

/// Parse one line of `fed_login.ps1` output. Anything unrecognised is `None`
/// and belongs in the log, not the status line.
pub fn parse_script_marker(line: &str) -> Option<ScriptEvent> {
    let line = line.trim();
    for (prefix, make) in [
        (
            "FEDLOGIN_STATUS:",
            &ScriptEvent::Status as &dyn Fn(String) -> ScriptEvent,
        ),
        ("FEDLOGIN_ERROR:", &ScriptEvent::Error),
        ("FEDLOGIN_DRYRUN:", &ScriptEvent::DryRun),
        ("FEDLOGIN_NOTE:", &ScriptEvent::Note),
    ] {
        if let Some(rest) = line.strip_prefix(prefix) {
            let rest = rest.trim();
            if rest.is_empty() {
                return None;
            }
            return Some(make(rest.to_string()));
        }
    }
    // The one marker that is not text. A handle of 0 is dropped rather than
    // carried: closing window 0 would close nothing at best.
    if let Some(rest) = line.strip_prefix("FEDLOGIN_HWND:") {
        return rest
            .trim()
            .parse::<isize>()
            .ok()
            .filter(|h| *h != 0)
            .map(ScriptEvent::Window);
    }
    None
}

/// Text saying the sign-in itself worked and only an account entitlement is
/// missing -- `fed` reports this with a URL to request access.
const ACCESS_PENDING_MARKERS: &[&str] = &[
    "do not have access",
    "don't have access",
    "does not have access",
    "no access to",
    "not authorized",
    "not entitled",
    "request access",
    "access request",
];

/// Whether a failure is the "signed in, but not entitled to that account
/// yet" kind.
///
/// Worth separating because it is not really a failed sign-in: the login
/// succeeded, and what is missing is an entitlement that frequently lands
/// within a minute. Retrying on the general interval wastes most of that.
///
/// A URL is **required**, not incidental. `fed` prints one to request access
/// with, and demanding it keeps the phrase matching from catching an
/// ordinary "not authorized" that will never resolve on its own -- which
/// would otherwise be retried hard for the whole window.
pub fn is_access_pending(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    let has_url = lower.contains("http://") || lower.contains("https://");
    has_url && ACCESS_PENDING_MARKERS.iter().any(|m| lower.contains(m))
}

/// How long to wait before running `fed up` again.
///
/// `retrying_for` is how long we have already been retrying this failure —
/// zero for the first failure after a good run.
///
/// `None` means nothing is scheduled. For a success that is the normal state
/// — expiry triggers the next run. For a failure it means the retry window is
/// exhausted: stop, and wait for the user to sign in by hand.
/// The gap to use before the next attempt at this particular failure.
///
/// Exposed so the caller can number attempts against the same interval it is
/// actually waiting -- counting 2-minute attempts while retrying every 30
/// seconds would report a fifth of the truth.
pub fn retry_interval_for(policy: &RetryPolicy, message: &str) -> Duration {
    if is_access_pending(message) {
        policy.access_interval
    } else {
        policy.interval
    }
}

pub fn next_delay(
    policy: &RetryPolicy,
    outcome: &FedOutcome,
    retrying_for: Duration,
) -> Option<Duration> {
    match outcome {
        // Nothing is scheduled after a success: the next run is whenever the
        // credentials expire, which the caller learns from `fed_expire`.
        FedOutcome::Authenticated => None,
        FedOutcome::Failed(msg) => {
            let interval = retry_interval_for(policy, msg);
            // The attempt that would land past the window is not worth
            // making: give up now rather than sleeping through it to time
            // out anyway.
            if retrying_for + interval > policy.window {
                None
            } else {
                Some(interval)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The line as it appears in `fed up`'s own output.
    const PROMPT: &str =
        "Go to https://example.okta.com/activate and enter code MXKD-9QRP";

    /// A note is a diagnostic, not progress: it must not be mistaken for a
    /// status, which drives the toolbar line.
    #[test]
    fn a_note_is_parsed_and_is_not_a_status() {
        assert_eq!(
            parse_script_marker("FEDLOGIN_NOTE:focused the activation code box"),
            Some(ScriptEvent::Note("focused the activation code box".to_string()))
        );
        assert!(matches!(
            parse_script_marker("FEDLOGIN_STATUS:entering-code"),
            Some(ScriptEvent::Status(_))
        ));
        assert_eq!(parse_script_marker("FEDLOGIN_NOTE:"), None);
    }

    #[test]
    fn the_activation_url_and_code_are_pulled_out_of_the_prompt() {
        assert_eq!(
            parse_device_code(PROMPT),
            Some((
                "https://example.okta.com/activate".to_string(),
                "MXKD-9QRP".to_string()
            ))
        );
    }

    #[test]
    fn the_prompt_is_found_among_other_output() {
        let out = format!("checking session...\n{PROMPT}\nwaiting for approval\n");
        let (url, code) = parse_device_code(&out).expect("prompt is still found");
        assert_eq!(url, "https://example.okta.com/activate");
        assert_eq!(code, "MXKD-9QRP");
    }

    #[test]
    fn trailing_punctuation_is_not_part_of_the_code() {
        let (url, code) =
            parse_device_code("Go to https://x.okta.com/activate. and enter code ABCD1234.")
                .expect("parses");
        assert_eq!(url, "https://x.okta.com/activate");
        assert_eq!(code, "ABCD1234");
    }

    #[test]
    fn output_without_both_halves_is_not_a_device_prompt() {
        // A URL with no code leaves the user nothing to paste, and a code
        // with no URL leaves them nowhere to paste it.
        assert_eq!(parse_device_code("see https://example.okta.com/help"), None);
        assert_eq!(parse_device_code("enter code ABCD1234"), None);
        assert_eq!(parse_device_code(""), None);
    }

    #[test]
    fn a_clean_exit_is_authenticated() {
        assert_eq!(classify_exit("", Some(0)), FedOutcome::Authenticated);
        assert_eq!(
            classify_exit("Session refreshed for account 1234\n", Some(0)),
            FedOutcome::Authenticated
        );
    }

    #[test]
    fn a_completed_run_is_judged_on_its_exit_code_not_the_prompt() {
        // The prompt is expected partway through — it was already acted on
        // while the output streamed. What matters at exit is whether the
        // user finished signing in.
        assert_eq!(classify_exit(PROMPT, Some(0)), FedOutcome::Authenticated);
        assert!(matches!(
            classify_exit(PROMPT, Some(1)),
            FedOutcome::Failed(_)
        ));
    }

    #[test]
    fn a_nonzero_exit_is_a_failure_carrying_the_last_line() {
        let out = "connecting\nError: could not reach the identity provider\n";
        match classify_exit(out, Some(1)) {
            FedOutcome::Failed(msg) => assert!(msg.contains("could not reach"), "{msg}"),
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    fn a_nonzero_exit_with_no_output_still_names_the_code() {
        match classify_exit("", Some(3)) {
            FedOutcome::Failed(msg) => assert!(msg.contains('3'), "{msg}"),
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[test]
    fn without_an_exit_code_the_text_decides() {
        assert!(matches!(
            classify_exit("Error: unauthorized", None),
            FedOutcome::Failed(_)
        ));
        assert_eq!(
            classify_exit("Session is current", None),
            FedOutcome::Authenticated
        );
    }

    #[test]
    fn a_clean_exit_is_taken_at_its_word() {
        // Progress text can legitimately contain the word "error" — the exit
        // code is the authority when we have one, or a successful run whose
        // summary mentions "0 errors" would loop forever.
        assert_eq!(
            classify_exit("0 errors, 0 warnings\n", Some(0)),
            FedOutcome::Authenticated
        );
    }

    #[test]
    fn success_schedules_nothing_because_expiry_drives_the_next_run() {
        // Re-running on a clock would ask a question `fed_expire` has
        // already answered.
        let p = RetryPolicy::default();
        assert_eq!(next_delay(&p, &FedOutcome::Authenticated, Duration::ZERO), None);
    }

    /// What `fed` prints when the sign-in worked but an account is not yet
    /// entitled.
    const NO_ACCESS: &str = "You do not have access to account 123456789012. \
        Request it at https://access.example.com/request/123";

    #[test]
    fn a_missing_entitlement_is_told_apart_from_a_failed_sign_in() {
        assert!(is_access_pending(NO_ACCESS));
        assert!(is_access_pending(
            "user not authorized for this account -- see https://x.example/req"
        ));
        // The wording alone is not enough. `fed` prints a URL to request
        // access with, and without one this is an ordinary authorization
        // failure that will not resolve on its own -- retrying it every 30s
        // for ten minutes would just be noise.
        assert!(!is_access_pending("user is not authorized for this account"));
        assert!(!is_access_pending("do not have access"));
        // Nor is a URL alone: plenty of output carries one.
        assert!(!is_access_pending("see https://docs.example.com for details"));
        assert!(!is_access_pending(""));
    }

    #[test]
    fn a_missing_entitlement_retries_far_sooner() {
        let p = RetryPolicy::default();
        let pending = FedOutcome::Failed(NO_ACCESS.to_string());
        assert_eq!(
            next_delay(&p, &pending, Duration::ZERO),
            Some(Duration::from_secs(30)),
            "an entitlement usually lands inside a minute"
        );
        // The window still applies -- it just fits many more attempts.
        assert_eq!(
            next_delay(&p, &pending, Duration::from_secs(9 * 60 + 40)),
            None,
            "the next attempt would land past the 10-minute window"
        );
        // And a general failure is unaffected.
        assert_eq!(
            next_delay(&p, &FedOutcome::Failed("nope".to_string()), Duration::ZERO),
            Some(Duration::from_secs(120))
        );
    }

    #[test]
    fn failures_retry_every_two_minutes_until_the_window_closes() {
        let p = RetryPolicy::default();
        let fail = FedOutcome::Failed("nope".to_string());
        // 0, 2, 4, 6, 8 minutes in: another 2-minute wait still lands inside
        // the 10-minute window.
        for minutes in [0, 2, 4, 6, 8] {
            assert_eq!(
                next_delay(&p, &fail, Duration::from_secs(minutes * 60)),
                Some(Duration::from_secs(120)),
                "{minutes}m in"
            );
        }
        // At 9 minutes the next attempt would land at 11 — past the window,
        // so stop and wait for the user rather than sleep two minutes to fail
        // anyway.
        assert_eq!(next_delay(&p, &fail, Duration::from_secs(9 * 60)), None);
        assert_eq!(next_delay(&p, &fail, Duration::from_secs(10 * 60)), None);
    }

    #[test]
    fn giving_up_names_the_cause_and_says_what_happens_next() {
        let msg =
            FedError::GaveUp(Box::new(FedError::Command("no network".to_string()))).to_string();
        assert!(msg.contains("no network"), "{msg}");
        // The user has to know it arms itself again, or they will assume the
        // feature is dead until they restart the app.
        assert!(msg.contains("arms itself again"), "{msg}");
    }

    #[test]
    fn the_shipped_policy_matches_the_documented_timings() {
        // 2-minute retries for 10 minutes; no periodic refresh at all.
        let p = RetryPolicy::default();
        assert_eq!(p.interval, Duration::from_secs(2 * 60));
        assert_eq!(p.window, Duration::from_secs(10 * 60));
        assert_eq!(p.access_interval, Duration::from_secs(30));
    }

    #[test]
    fn the_script_markers_are_parsed_by_kind() {
        assert_eq!(
            parse_script_marker("FEDLOGIN_STATUS:entering-code"),
            Some(ScriptEvent::Status("entering-code".to_string()))
        );
        assert_eq!(
            parse_script_marker("FEDLOGIN_ERROR:chrome.exe not found"),
            Some(ScriptEvent::Error("chrome.exe not found".to_string()))
        );
        assert_eq!(
            parse_script_marker("FEDLOGIN_DRYRUN:would send the password"),
            Some(ScriptEvent::DryRun("would send the password".to_string()))
        );
    }

    #[test]
    fn a_window_handle_marker_is_parsed_as_a_number() {
        assert_eq!(
            parse_script_marker("FEDLOGIN_HWND:132456"),
            Some(ScriptEvent::Window(132456))
        );
        // Nothing usable: closing window 0 would close nothing at best.
        assert_eq!(parse_script_marker("FEDLOGIN_HWND:0"), None);
        assert_eq!(parse_script_marker("FEDLOGIN_HWND:notanumber"), None);
        assert_eq!(parse_script_marker("FEDLOGIN_HWND:"), None);
    }

    #[test]
    fn ordinary_script_output_is_not_a_marker() {
        assert_eq!(parse_script_marker("PS C:\\> whatever"), None);
        assert_eq!(parse_script_marker(""), None);
        // A marker with nothing after the colon carries no information.
        assert_eq!(parse_script_marker("FEDLOGIN_ERROR:"), None);
        assert_eq!(parse_script_marker("FEDLOGIN_ERROR:   "), None);
    }

    #[test]
    fn the_sign_in_failure_sources_read_differently() {
        // The user has to be able to tell "fed up itself broke" from "the
        // automation broke" from "your saved password is wrong".
        let cmd = FedError::Command("session expired".to_string()).to_string();
        assert!(cmd.contains("fed up failed"), "{cmd}");

        let browser = FedError::Browser("focus guard tripped".to_string()).to_string();
        assert!(browser.contains("automatic sign-in failed"), "{browser}");

        let pw = FedError::NotAuthenticated.to_string();
        assert!(pw.contains("password"), "{pw}");
        assert!(pw.contains("Okta"), "{pw}");
    }
}

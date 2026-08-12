use std::process::Command;

use crate::credentials;
use crate::error::{AppError, Result};

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

/// Retry settings for one `aws` invocation, given whether the user's own
/// environment already sets each.
///
/// **The CLI's default is `legacy` mode: three attempts, then it gives up** —
/// which is the "reached max retries: 2" in `Rate exceeded (reached max
/// retries: 2)`. Legacy also retries a narrower set of errors and backs off
/// less patiently than `standard`, so a throttled account fails a refresh that
/// would have succeeded a moment later.
///
/// It is worth being generous here because one inventory load is three
/// concurrent calls (`describe-instances`, an unfiltered `describe-tags`, and
/// `describe-instance-information`), and `describe-tags` on a large account is
/// exactly the shape of request that gets throttled.
///
/// `standard` rather than `adaptive`: adaptive adds client-side rate limiting
/// that slows every later call once it has seen a throttle, which would make
/// the app feel sluggish long after the burst that caused it.
///
/// Anything the user set themselves is left alone — someone who has tuned
/// these for a corporate endpoint should not have it silently overridden.
fn retry_envs(have_mode: bool, have_attempts: bool) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if !have_mode {
        out.push(("AWS_RETRY_MODE".to_string(), "standard".to_string()));
    }
    if !have_attempts {
        out.push(("AWS_MAX_ATTEMPTS".to_string(), "10".to_string()));
    }
    out
}

pub fn run_aws_cli(profile: Option<&str>, region: Option<&str>, args: &[&str]) -> Result<String> {
    let mut final_args: Vec<String> = Vec::new();
    let mut envs: Vec<(String, String)> = retry_envs(
        std::env::var_os("AWS_RETRY_MODE").is_some(),
        std::env::var_os("AWS_MAX_ATTEMPTS").is_some(),
    );

    // Try to read fed_aws_* credentials directly so we can bypass
    // credential_process (which may open an interactive window).
    let mut creds_injected = false;
    if let Some(p) = profile {
        if let Some(creds) = credentials::read_profile_credentials(p) {
            envs.push(("AWS_ACCESS_KEY_ID".into(), creds.access_key_id));
            envs.push(("AWS_SECRET_ACCESS_KEY".into(), creds.secret_access_key));
            if let Some(token) = creds.session_token {
                envs.push(("AWS_SESSION_TOKEN".into(), token));
            }
            creds_injected = true;
        }
    }

    // Only pass --profile when we didn't inject credentials via env vars.
    // Using --profile with credential_process causes AWS CLI to launch the
    // external helper (e.g. `fed`), which may open a blocking terminal window.
    if !creds_injected {
        if let Some(p) = profile {
            final_args.push("--profile".to_string());
            final_args.push(p.to_string());
        }
    }

    if let Some(r) = region {
        final_args.push("--region".to_string());
        final_args.push(r.to_string());
    }

    final_args.extend(args.iter().map(|s| s.to_string()));

    run_capture("aws", &final_args, &envs)
}

pub fn run_capture(program: &str, args: &[String], envs: &[(String, String)]) -> Result<String> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    #[cfg(target_os = "windows")]
    cmd.creation_flags(CREATE_NO_WINDOW);

    for (k, v) in envs {
        cmd.env(k, v);
    }

    let output = cmd.output()?;
    if !output.status.success() {
        return Err(AppError::CommandFailed {
            program: program.to_string(),
            args: args.to_vec(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The CLI's default is `legacy`: three attempts, then "reached max
    /// retries: 2". A refresh is three concurrent calls per account, so a
    /// throttled account gives up on work that would succeed shortly after.
    #[test]
    fn retries_are_raised_above_the_cli_default() {
        let envs = retry_envs(false, false);
        let get = |k: &str| {
            envs.iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("AWS_RETRY_MODE"), Some("standard"));
        let attempts: u32 = get("AWS_MAX_ATTEMPTS").expect("set").parse().expect("a number");
        assert!(attempts > 3, "must beat the legacy default of 3, got {attempts}");
    }

    /// Someone who tuned these for a corporate endpoint must not have it
    /// silently overridden.
    #[test]
    fn a_users_own_settings_are_left_alone() {
        assert!(retry_envs(true, true).is_empty());
        let only_attempts = retry_envs(true, false);
        assert_eq!(only_attempts.len(), 1);
        assert_eq!(only_attempts[0].0, "AWS_MAX_ATTEMPTS");
        let only_mode = retry_envs(false, true);
        assert_eq!(only_mode.len(), 1);
        assert_eq!(only_mode[0].0, "AWS_RETRY_MODE");
    }
}

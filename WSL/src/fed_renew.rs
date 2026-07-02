//! Federated-auth renewal helper.
//!
//! When a profile's auth expires the GUI can drive the `fed up` device
//! activation flow (see `scripts/fedup.py`) to mint fresh credentials.
//! Access is gated by a plaintext `secrets.env` that the operator owns:
//! it lists the OS usernames allowed to use the feature plus every var the
//! script needs. If the current OS user isn't listed, the feature is inert
//! for them.
//!
//! The `secrets.env` lives at `~/.fedup/secrets.env` and should be readable
//! only by its owner (the `init` subcommand of `fedup.py` locks it down).
//!
//! ```text
//! # ~/.fedup/secrets.env
//! ALLOWED_USERS=alice,bob      # OS usernames allowed to renew
//! USERNAME=alice@example.com   # what the login pages ask for
//! PASSWORD=hunter2
//! FED_CMD=fed up               # optional; defaults to "fed up"
//! HEADLESS=false               # true = run Chrome invisibly
//! AUTO_RENEW=false             # true = auto-start renewal on expiry
//! ```
//!
//! This module only *reads* the file and decides whether the current user is
//! allowed; it never writes secrets. Editing is done via `fedup.py init/edit`.

use std::collections::HashMap;
use std::env;
use std::path::PathBuf;

use crate::util::{home_dir, split_csv};

/// Directory holding the renewal config: `~/.fedup`.
pub fn fedup_dir() -> Option<PathBuf> {
    home_dir().map(|p| p.join(".fedup"))
}

/// Path to the plaintext secrets file: `~/.fedup/secrets.env`.
pub fn secrets_path() -> Option<PathBuf> {
    fedup_dir().map(|p| p.join("secrets.env"))
}

/// Parsed `secrets.env`: the raw key/value pairs plus the derived allowlist.
#[derive(Clone, Debug, Default)]
pub struct Secrets {
    pub vars: HashMap<String, String>,
}

impl Secrets {
    /// OS usernames permitted to use the renewal feature (case preserved as
    /// written; matching is case-insensitive — see [`allows_user`]).
    pub fn allowed_users(&self) -> Vec<String> {
        self.vars
            .get("ALLOWED_USERS")
            .map(|v| split_csv(v))
            .unwrap_or_default()
    }

    /// True when `user` is on the allowlist (case- and whitespace-insensitive).
    /// An empty allowlist denies everyone — the feature fails closed.
    pub fn allows_user(&self, user: &str) -> bool {
        let u = user.trim().to_ascii_lowercase();
        if u.is_empty() {
            return false;
        }
        self.allowed_users()
            .iter()
            .any(|a| a.trim().to_ascii_lowercase() == u)
    }

    /// True when the operator opted into automatic renewal on expiry.
    pub fn auto_renew(&self) -> bool {
        self.vars
            .get("AUTO_RENEW")
            .map(|v| is_truthy(v))
            .unwrap_or(false)
    }

    /// True when Chrome should run headless (invisible).
    pub fn headless(&self) -> bool {
        self.vars
            .get("HEADLESS")
            .map(|v| is_truthy(v))
            .unwrap_or(false)
    }
}

/// Interpret common truthy spellings from the env file.
fn is_truthy(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Parse a plaintext `KEY=VALUE` env file. Blank lines and `#` comments are
/// ignored; surrounding whitespace and a single layer of matching quotes are
/// stripped from values.
pub fn parse_secrets(text: &str) -> Secrets {
    let mut vars = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Support an optional leading `export ` for shell-sourced files.
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let key = k.trim();
        if key.is_empty() {
            continue;
        }
        let val = strip_quotes(v.trim());
        vars.insert(key.to_string(), val.to_string());
    }
    Secrets { vars }
}

/// Remove one matching pair of surrounding single or double quotes.
fn strip_quotes(v: &str) -> &str {
    let bytes = v.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' || first == b'\'') && first == last {
            return &v[1..v.len() - 1];
        }
    }
    v
}

/// Read and parse `~/.fedup/secrets.env`, or `None` if it doesn't exist / can't
/// be read.
pub fn load_secrets() -> Option<Secrets> {
    let path = secrets_path()?;
    let text = std::fs::read_to_string(path).ok()?;
    Some(parse_secrets(&text))
}

/// The current OS username. Windows: `USERNAME`; Unix: `USER` then `LOGNAME`.
pub fn current_os_user() -> Option<String> {
    let keys: &[&str] = if cfg!(windows) {
        &["USERNAME"]
    } else {
        &["USER", "LOGNAME"]
    };
    for k in keys {
        if let Some(v) = env::var_os(k) {
            let s = v.to_string_lossy().trim().to_string();
            if !s.is_empty() {
                return Some(s);
            }
        }
    }
    None
}

/// Whether the current OS user may use the renewal feature: the secrets file
/// must exist and list them in `ALLOWED_USERS`.
pub fn current_user_allowed(secrets: &Secrets) -> bool {
    match current_os_user() {
        Some(u) => secrets.allows_user(&u),
        None => false,
    }
}

/// Locate `fedup.py`. Search order:
///   1. `$FEDUP_SCRIPT` (explicit override)
///   2. next to the running executable
///   3. `scripts/fedup.py` next to the executable
///   4. `~/.fedup/fedup.py`
///   5. `scripts/fedup.py` under the current working directory
pub fn script_path() -> Option<PathBuf> {
    if let Some(p) = env::var_os("FEDUP_SCRIPT") {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("fedup.py"));
            candidates.push(dir.join("scripts").join("fedup.py"));
        }
    }
    if let Some(dir) = fedup_dir() {
        candidates.push(dir.join("fedup.py"));
    }
    if let Ok(cwd) = env::current_dir() {
        candidates.push(cwd.join("scripts").join("fedup.py"));
    }

    candidates.into_iter().find(|p| p.is_file())
}

/// The Python interpreter to invoke: `$FEDUP_PYTHON`, else `PYTHON` from the
/// secrets file, else `python`.
pub fn python_program(secrets: &Secrets) -> String {
    if let Some(p) = env::var_os("FEDUP_PYTHON") {
        let s = p.to_string_lossy().trim().to_string();
        if !s.is_empty() {
            return s;
        }
    }
    secrets
        .vars
        .get("PYTHON")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "python".to_string())
}

/// A ready-to-spawn description of the renewal command.
#[derive(Clone, Debug)]
pub struct RenewCommand {
    pub program: String,
    pub args: Vec<String>,
}

/// Build the command that runs the renewal flow, or a human-readable reason it
/// can't run (secrets missing, user not allowed, or script not found).
pub fn build_renew_command() -> std::result::Result<RenewCommand, String> {
    let secrets = load_secrets().ok_or_else(|| {
        let where_ = secrets_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "~/.fedup/secrets.env".to_string());
        format!("No secrets file at {where_}. Create it with: python fedup.py init")
    })?;

    let user = current_os_user().unwrap_or_default();
    if !secrets.allows_user(&user) {
        return Err(format!(
            "User '{user}' is not permitted to use auto-renew (not in ALLOWED_USERS)."
        ));
    }

    let script = script_path()
        .ok_or_else(|| "Could not find fedup.py (set FEDUP_SCRIPT or place it next to the app).".to_string())?;

    Ok(RenewCommand {
        program: python_program(&secrets),
        args: vec![script.to_string_lossy().to_string(), "run".to_string()],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_pairs() {
        let s = parse_secrets("ALLOWED_USERS=alice,bob\nUSERNAME=alice@x.com\nPASSWORD=pw\n");
        assert_eq!(s.vars.get("USERNAME").unwrap(), "alice@x.com");
        assert_eq!(s.allowed_users(), vec!["alice", "bob"]);
    }

    #[test]
    fn comments_blank_and_export_ignored() {
        let s = parse_secrets("# comment\n\nexport HEADLESS=true\n  FED_CMD = fed up \n");
        assert!(s.headless());
        assert_eq!(s.vars.get("FED_CMD").unwrap(), "fed up");
    }

    #[test]
    fn quoted_values_stripped() {
        let s = parse_secrets("PASSWORD=\"p@ss word\"\nUSERNAME='alice'\n");
        assert_eq!(s.vars.get("PASSWORD").unwrap(), "p@ss word");
        assert_eq!(s.vars.get("USERNAME").unwrap(), "alice");
    }

    #[test]
    fn allowlist_is_case_insensitive() {
        let s = parse_secrets("ALLOWED_USERS=Alice, BOB");
        assert!(s.allows_user("alice"));
        assert!(s.allows_user(" bob "));
        assert!(s.allows_user("BOB"));
        assert!(!s.allows_user("carol"));
    }

    #[test]
    fn empty_allowlist_denies_everyone() {
        let s = parse_secrets("USERNAME=x");
        assert!(!s.allows_user("alice"));
        let s2 = parse_secrets("ALLOWED_USERS=");
        assert!(!s2.allows_user("alice"));
    }

    #[test]
    fn blank_user_never_allowed() {
        let s = parse_secrets("ALLOWED_USERS=alice");
        assert!(!s.allows_user(""));
        assert!(!s.allows_user("   "));
    }

    #[test]
    fn truthy_variants() {
        for v in ["1", "true", "TRUE", "yes", "On"] {
            assert!(is_truthy(v), "{v} should be truthy");
        }
        for v in ["0", "false", "no", "", "off", "maybe"] {
            assert!(!is_truthy(v), "{v} should be falsy");
        }
    }

    #[test]
    fn auto_renew_defaults_off() {
        assert!(!parse_secrets("USERNAME=x").auto_renew());
        assert!(parse_secrets("AUTO_RENEW=yes").auto_renew());
    }

    #[test]
    fn headless_defaults_off() {
        assert!(!parse_secrets("USERNAME=x").headless());
        assert!(parse_secrets("HEADLESS=1").headless());
    }

    #[test]
    fn python_program_prefers_secrets_over_default() {
        let s = parse_secrets("PYTHON=py -3");
        // FEDUP_PYTHON env is not set in the test, so secrets value wins.
        assert_eq!(python_program(&s), "py -3");
        let s2 = parse_secrets("USERNAME=x");
        assert_eq!(python_program(&s2), "python");
    }

    #[test]
    fn malformed_lines_skipped() {
        let s = parse_secrets("no_equals_here\n=novalue\nGOOD=1\n");
        assert_eq!(s.vars.len(), 1);
        assert_eq!(s.vars.get("GOOD").unwrap(), "1");
    }
}

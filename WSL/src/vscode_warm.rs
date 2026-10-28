//! Pre-installing the VS Code server on a bastion, so the first Remote-SSH
//! open does not sit through the download and extract.
//!
//! Pure: a commit and a login in, a shell command out, and the reply parsed
//! back. The thread that sends it and the places it is triggered from live in
//! the GUI binary.
//!
//! The install is **per login and per VS Code commit**, under that login's
//! own `~/.vscode-server`, so one user warming a box does nothing for
//! another. It is also idempotent: a box that already has the commit answers
//! `PRESENT` after one quick call, so the app never needs to remember what
//! it has warmed.

use base64::Engine;

/// What a warm run reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WarmOutcome {
    /// Downloaded and put in place just now.
    Installed,
    /// Already there; nothing was downloaded.
    Present,
    /// The script ran and said why it could not finish.
    Failed(String),
    /// No marker at all: the script did not run, or was cut off. Never read
    /// as success.
    Unknown,
}

/// The commit out of `code --version`, whose three lines are the version, the
/// commit and the architecture. Anything that is not a 40-digit hex id is
/// refused, because the value is interpolated into a URL and a shell command.
pub fn parse_commit(code_version_output: &str) -> Option<String> {
    let line = code_version_output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .nth(1)?;
    valid_commit(line).then(|| line.to_ascii_lowercase())
}

fn valid_commit(s: &str) -> bool {
    s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn valid_login(user: &str) -> bool {
    !user.is_empty()
        && user.len() <= 32
        && !user.starts_with('-')
        && user
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Where VS Code keeps the server for one commit, relative to the login's
/// home.
pub fn server_dir(commit: &str) -> String {
    format!(".vscode-server/cli/servers/Stable-{commit}")
}

/// The script that installs the server for `commit`, run **as the login**.
///
/// Extracted beside its final name and moved in with one `mv`, so a download
/// that dies half way leaves nothing VS Code could mistake for an install.
/// The marker is assembled at runtime, so nothing that echoes the command
/// line can be read as a result.
fn script(commit: &str) -> String {
    format!(
        r#"C={commit}
D="$HOME/{dir}"
if [ -f "$D/server/product.json" ]; then v=PRESENT; m=; else
  T="$D.part.$$"
  rm -rf "$T"; mkdir -p "$T" || {{ v=FAIL; m="cannot create $T"; }}
  if [ -z "${{v:-}}" ]; then
    if curl -fsSL --retry 3 "https://update.code.visualstudio.com/commit:$C/server-linux-x64/stable" | tar -xz -C "$T" --strip-components=1; then
      if [ -f "$T/product.json" ]; then
        mkdir -p "$D" && rm -rf "$D/server" && mv "$T" "$D/server" && v=OK || {{ v=FAIL; m="could not move into place"; }}
      else v=FAIL; m="archive has no product.json"; fi
    else v=FAIL; m="download or extract failed"; fi
  fi
  rm -rf "$T"
fi
echo "__VW_${{v}}__$m"
"#,
        dir = server_dir(commit)
    )
}

/// The one-line command to hand to SSM `send-command` (which runs as root):
/// the script, base64-encoded, run as `user` so the files are theirs. `None`
/// for a commit or login that does not pass the whitelist.
pub fn command(commit: &str, user: &str) -> Option<String> {
    if !valid_commit(commit) || !valid_login(user) {
        return None;
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(script(commit));
    Some(format!(
        "echo {b64} | base64 -d | sudo -n -u '{user}' -H bash -s"
    ))
}

/// Read a run's output. FAIL is checked first, and a missing marker is
/// `Unknown`.
pub fn parse_outcome(output: &str) -> WarmOutcome {
    for line in output.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("__VW_FAIL__") {
            return WarmOutcome::Failed(rest.trim().to_string());
        }
    }
    for line in output.lines().map(str::trim) {
        match line {
            "__VW_OK__" => return WarmOutcome::Installed,
            "__VW_PRESENT__" => return WarmOutcome::Present,
            _ => {}
        }
    }
    WarmOutcome::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn the_commit_is_the_second_line_of_code_version() {
        let out = format!("1.95.3\n{COMMIT}\nx64\n");
        assert_eq!(parse_commit(&out).as_deref(), Some(COMMIT));
    }

    #[test]
    fn a_commit_that_is_not_forty_hex_digits_is_refused() {
        assert_eq!(parse_commit("1.95.3\nnot-a-commit\nx64"), None);
        assert_eq!(parse_commit("1.95.3\n0123abc\nx64"), None);
        assert_eq!(parse_commit("1.95.3"), None);
        assert_eq!(parse_commit(""), None);
    }

    #[test]
    fn the_command_refuses_values_that_could_break_out_of_the_shell() {
        assert!(command(COMMIT, "jdoe").is_some());
        assert!(command(COMMIT, "jdoe; rm -rf /").is_none());
        assert!(command(COMMIT, "-jdoe").is_none());
        assert!(command(COMMIT, "").is_none());
        assert!(command("abc; id", "jdoe").is_none());
    }

    #[test]
    fn the_command_runs_as_the_login_not_as_root() {
        let cmd = command(COMMIT, "jdoe").unwrap();
        assert!(cmd.contains("sudo -n -u 'jdoe' -H bash -s"));
        assert!(!cmd.contains('\n'), "one line, for send-command");
    }

    #[test]
    fn the_script_installs_under_the_commits_own_directory() {
        let s = script(COMMIT);
        assert!(s.contains(&format!(".vscode-server/cli/servers/Stable-{COMMIT}")));
        assert!(s.contains(&format!("commit:$C/server-linux-x64/stable")));
    }

    #[test]
    fn the_script_skips_a_box_that_already_has_the_commit() {
        let s = script(COMMIT);
        let present = s.find("PRESENT").unwrap();
        let download = s.find("curl").unwrap();
        assert!(present < download, "the check comes before any download");
    }

    #[test]
    fn the_script_never_leaves_a_partial_install_in_the_final_place() {
        let s = script(COMMIT);
        assert!(s.contains("mv \"$T\" \"$D/server\""));
        assert!(s.contains("rm -rf \"$T\""));
    }

    #[test]
    fn the_marker_is_not_a_literal_in_the_script() {
        let s = script(COMMIT);
        assert!(!s.contains("__VW_OK__"));
        assert!(!s.contains("__VW_PRESENT__"));
        assert!(s.contains("__VW_${v}__"));
    }

    #[test]
    fn outcomes_are_read_with_failure_first_and_silence_unknown() {
        assert_eq!(parse_outcome("__VW_OK__\n"), WarmOutcome::Installed);
        assert_eq!(parse_outcome("__VW_PRESENT__"), WarmOutcome::Present);
        assert_eq!(
            parse_outcome("__VW_OK__\n__VW_FAIL__download or extract failed"),
            WarmOutcome::Failed("download or extract failed".to_string())
        );
        assert_eq!(parse_outcome(""), WarmOutcome::Unknown);
        assert_eq!(parse_outcome("sudo: a password is required"), WarmOutcome::Unknown);
    }
}

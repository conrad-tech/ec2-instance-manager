//! Background SSH tunnels carrying the port forwards.
//!
//! One hidden `ssh` process per environment, holding that environment's
//! `LocalForward`s open independently of VS Code. Two properties matter:
//!
//! - **The window is never shown** (`CREATE_NO_WINDOW` on Windows), which
//!   makes every failure silent unless we surface it. So stderr is captured
//!   and kept, `ExitOnForwardFailure=yes` turns a half-forwarded session into
//!   a dead one, and the caller polls [`Tunnel::is_running`] to notice.
//! - **There is no remote shell** (`ssh -N`). The forwards are the whole
//!   point, and a shell only adds a `TMOUT` that logs an idle session out
//!   however healthy the connection is. `ServerAliveInterval` keeps the
//!   transport up, and the caller restarts anything that dies — both of
//!   which work on a session with no shell to keep busy.
//! - **A tunnel cannot outlive the app.** [`Drop`] kills the child, and the
//!   GUI also stops them all on close. An invisible ssh session left running
//!   after the app quits is a process the user has no way to find.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use crate::forwards::ResolvedForward;

/// A running background tunnel.
pub struct Tunnel {
    child: Child,
    /// Host alias the session connects to.
    pub alias: String,
    /// Signature of the forward set it was started with, so the caller can
    /// tell that the resolved forwards have changed underneath it.
    pub signature: String,
    /// stderr from the hidden process — the only account of why it died.
    errors: Arc<Mutex<Vec<String>>>,
}

impl Tunnel {
    /// Spawn a hidden `ssh` session carrying `forwards`.
    pub fn spawn(
        alias: &str,
        forwards: &[ResolvedForward],
        signature: String,
    ) -> std::result::Result<Self, String> {
        let args = crate::forwards::tunnel_args(alias, forwards);
        let mut command = Command::new("ssh");
        command
            .args(&args)
            // `-N` opens no session channel, so stdin is never read. Null
            // rather than piped: there is nothing to write to, and an open
            // pipe would only invite someone to try.
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW: no console flashes up, and none stays on the
            // taskbar. The user asked for these to be invisible.
            command.creation_flags(0x0800_0000);
        }

        let mut child = command
            .spawn()
            .map_err(|e| format!("could not start ssh: {e}"))?;

        let errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        // Capture stderr. Without this a hidden process fails mutely.
        if let Some(stderr) = child.stderr.take() {
            let sink = Arc::clone(&errors);
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let line = line.trim().to_string();
                    if line.is_empty() {
                        continue;
                    }
                    if let Ok(mut sink) = sink.lock() {
                        // Bounded: a session that spews cannot grow forever.
                        if sink.len() >= 50 {
                            sink.remove(0);
                        }
                        sink.push(line);
                    }
                }
            });
        }

        Ok(Self {
            child,
            alias: alias.to_string(),
            signature,
            errors,
        })
    }

    /// Whether the session is still up. Polling `try_wait` rather than
    /// waiting on a thread keeps the child owned here, so `stop` can kill it.
    pub fn is_running(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(Some(_)) | Err(_))
    }

    /// Everything the process wrote to stderr, most recent last.
    pub fn errors(&self) -> Vec<String> {
        self.errors.lock().map(|e| e.clone()).unwrap_or_default()
    }

    /// The last stderr line, which is usually the reason it died.
    pub fn last_error(&self) -> Option<String> {
        self.errors().last().cloned()
    }

    /// Kill the session.
    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// A command that exits immediately stands in for a dead session.
    #[test]
    fn is_running_reports_a_finished_process() {
        let mut child = Command::new("true")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn true");
        // Give it a moment to exit before polling.
        std::thread::sleep(Duration::from_millis(200));
        let mut tunnel = Tunnel {
            child: {
                let _ = child.wait();
                child
            },
            alias: "test".to_string(),
            signature: String::new(),
            errors: Arc::new(Mutex::new(Vec::new())),
        };
        assert!(!tunnel.is_running());
    }
}

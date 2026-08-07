//! Background SSH tunnels carrying the port forwards.
//!
//! One hidden `ssh` process per environment, holding that environment's
//! `LocalForward`s open independently of VS Code. Two properties matter:
//!
//! - **The window is never shown** (`CREATE_NO_WINDOW` on Windows), which
//!   makes every failure silent unless we surface it. So stderr is captured
//!   and kept, `ExitOnForwardFailure=yes` turns a half-forwarded session into
//!   a dead one, and the caller polls [`Tunnel::is_running`] to notice.
//! - **A tunnel cannot outlive the app.** [`Drop`] kills the child, and the
//!   GUI also stops them all on close. An invisible ssh session left running
//!   after the app quits is a process the user has no way to find.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::forwards::ResolvedForward;

/// How often a newline is written to the session.
///
/// `ServerAliveInterval` already keeps the transport up; this is for the
/// remote *shell*, which on a hardened bastion has a `TMOUT` that logs an
/// idle login out regardless of how healthy the connection is.
const KEEPALIVE: Duration = Duration::from_secs(60);

/// A running background tunnel.
pub struct Tunnel {
    child: Child,
    stop: Arc<AtomicBool>,
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
            .stdin(Stdio::piped())
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

        let stop = Arc::new(AtomicBool::new(false));
        let errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        // Keepalive: a newline every minute for as long as the tunnel lives.
        if let Some(mut stdin) = child.stdin.take() {
            let stop_flag = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop_flag.load(Ordering::Relaxed) {
                    // Sleep in slices so stopping does not wait a full minute.
                    for _ in 0..60 {
                        if stop_flag.load(Ordering::Relaxed) {
                            return;
                        }
                        std::thread::sleep(KEEPALIVE / 60);
                    }
                    if stdin.write_all(b"\n").is_err() || stdin.flush().is_err() {
                        // Session is gone; the caller notices via is_running.
                        return;
                    }
                }
            });
        }

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
            stop,
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

    /// Kill the session and stop its keepalive.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
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
    use super::*;

    /// The keepalive has to be sliced finely enough that stopping a tunnel
    /// is not a minute-long wait — the GUI kills these on close.
    #[test]
    fn keepalive_slice_is_responsive() {
        assert_eq!(KEEPALIVE, Duration::from_secs(60));
        assert!(KEEPALIVE / 60 <= Duration::from_secs(1));
    }

    /// A command that exits immediately stands in for a dead session.
    #[test]
    fn is_running_reports_a_finished_process() {
        let mut child = Command::new("true")
            .stdin(Stdio::piped())
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
            stop: Arc::new(AtomicBool::new(false)),
            alias: "test".to_string(),
            signature: String::new(),
            errors: Arc::new(Mutex::new(Vec::new())),
        };
        assert!(!tunnel.is_running());
    }
}

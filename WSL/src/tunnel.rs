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
use std::time::{Duration, Instant};

use crate::forwards::ResolvedForward;

/// How much of a session's stderr is kept.
///
/// The window shows this live, so it is the only account of a tunnel whose
/// local binds all succeeded — the session looks perfectly healthy — while
/// every remote dial is refused. ssh reports that as `channel N: open
/// failed: connect failed: …`, one line per attempt.
///
/// Deep enough to hold the `-v` handshake (~100 lines) *and* the channel
/// errors that come after it: the handshake must not push out the failure
/// it was there to explain.
const MAX_STDERR_LINES: usize = 500;

/// A running background tunnel.
pub struct Tunnel {
    child: Child,
    /// Host alias the session connects to.
    pub alias: String,
    /// Signature of the forward set it was started with, so the caller can
    /// tell that the resolved forwards have changed underneath it.
    pub signature: String,
    /// Instance id of the bastion this session goes through — the pair's
    /// primary, or the secondary after a failover. The caller shows this,
    /// because an environment quietly running on its backup looks exactly
    /// like one running normally.
    pub bastion: String,
    /// When the session was spawned. The caller uses [`Tunnel::age`] at the
    /// moment it notices a death to tell "never connected" from "ran fine
    /// for an hour and then dropped" — only the first is worth failing over.
    started: Instant,
    /// stderr from the hidden process — the only account of why it died.
    errors: Arc<Mutex<Vec<String>>>,
}

/// What to spawn, and with which client.
///
/// The client is a parameter because a WSL build has to run the **Windows**
/// ssh, or its forwards bind WSL's loopback where no Windows browser can
/// reach them. See [`crate::wsl`].
pub struct TunnelSpec<'a> {
    /// ssh binary to run — `"ssh"` natively, the Windows client under WSL.
    pub program: &'a str,
    /// Passed as `-F`. Set when the block this alias lives in is not the one
    /// the chosen client reads by default, which is the WSL case: the
    /// Windows client would otherwise look in the Windows user's own config.
    pub config_file: Option<&'a str>,
    pub alias: &'a str,
    pub forwards: &'a [ResolvedForward],
    pub signature: String,
    pub bastion: String,
}

impl Tunnel {
    /// Spawn a hidden ssh session carrying `forwards`.
    pub fn spawn(spec: TunnelSpec<'_>) -> std::result::Result<Self, String> {
        let TunnelSpec {
            program,
            config_file,
            alias,
            forwards,
            signature,
            bastion,
        } = spec;
        let mut args: Vec<String> = Vec::new();
        if let Some(config) = config_file {
            // Before everything else: ssh keeps the first value it obtains
            // for a keyword, and this file is the whole configuration for
            // this connection.
            args.push("-F".to_string());
            args.push(config.to_string());
        }
        args.extend(crate::forwards::tunnel_args(alias, forwards));
        let mut command = Command::new(program);
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
                        // Deep enough to hold a run of `channel N: open
                        // failed` lines, which is what diagnoses a forward
                        // whose local bind works and whose remote dial does
                        // not — one per attempt, so a browser reload can
                        // produce several at once.
                        if sink.len() >= MAX_STDERR_LINES {
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
            bastion,
            started: Instant::now(),
            errors,
        })
    }

    /// Whether a forward's local port is actually accepting connections.
    ///
    /// This is the only honest test that a tunnel is working. `is_running`
    /// says the process exists, and a session behind an SSM `ProxyCommand`
    /// will sit alive indefinitely without ever finishing its connection —
    /// binding nothing, writing nothing, exiting never. Age cannot tell that
    /// apart from a healthy session either; only asking the listener can.
    ///
    /// A refused connection means not bound. Anything else — accepted, or
    /// even accepted and then closed because the *remote* dial failed —
    /// means ssh is listening, which is what this answers.
    pub fn is_bound(ip: &str, port: u16) -> bool {
        use std::net::{SocketAddr, TcpStream};
        let Ok(addr) = format!("{ip}:{port}").parse::<SocketAddr>() else {
            return false;
        };
        TcpStream::connect_timeout(&addr, Duration::from_millis(400)).is_ok()
    }

    /// How long since the session was spawned.
    ///
    /// Read when a death is noticed, not continuously: a session that never
    /// managed to connect dies within a few seconds, so a small age at the
    /// moment of detection means "this bastion did not work", while a large
    /// one means a working tunnel dropped and should simply be restarted.
    pub fn age(&self) -> Duration {
        self.started.elapsed()
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
            bastion: "i-0test".to_string(),
            started: Instant::now(),
            errors: Arc::new(Mutex::new(Vec::new())),
        };
        assert!(!tunnel.is_running());
    }

    /// A session that has only just been spawned reports a small age. The
    /// caller's failover decision hangs on this: a young corpse means the
    /// bastion never worked, an old one means a good tunnel dropped.
    #[test]
    fn age_starts_near_zero() {
        let child = Command::new("true")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn true");
        let tunnel = Tunnel {
            child,
            alias: "test".to_string(),
            signature: String::new(),
            bastion: "i-0test".to_string(),
            started: Instant::now(),
            errors: Arc::new(Mutex::new(Vec::new())),
        };
        assert!(tunnel.age() < Duration::from_secs(1));
    }
}

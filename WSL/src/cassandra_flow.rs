//! Cassandra cert renewal and rollback: orchestration over an `exec` closure
//! (instance id, shell command, timeout -> output), so the sequencing is
//! tested with a fake instead of AWS. The decisions live in `cassandra_cert`.

use std::collections::HashMap;
use std::time::Duration;

use base64::Engine;

use crate::cassandra_cert::{
    compare_certs, parse_check, parse_preflight, parse_rc, served_matches, stale_against, stale_unselected,
    CertChange, CertInfo, OldCert, PreflightRaw, StabilityWatch, WatchState,
};
use crate::obf_core::obf_transform;

/// Runs `command` on `instance_id` and returns its combined output. The real
/// one is built from `ssm_send_command` + `ssm_wait_for_command`; tests pass
/// a fake. Must be callable from several threads at once.
pub type ExecFn<'a> = &'a (dyn Fn(&str, &str, Duration) -> Result<String, String> + Sync);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub instance_id: String,
    pub name: String,
}

/// How long one script invocation may take. `cassandra.sh` builds a keystore
/// and, without `--no-restart`, waits for the node: generous, but bounded.
const SCRIPT_TIMEOUT: Duration = Duration::from_secs(300);
const QUICK_TIMEOUT: Duration = Duration::from_secs(60);

fn asset(blob: &[u8]) -> String {
    String::from_utf8(obf_transform(blob)).expect("bundled script is valid UTF-8")
}

pub fn renew_script() -> String {
    asset(include_bytes!(concat!(env!("OUT_DIR"), "/cassandra.sh.obf")))
}
pub fn check_script() -> String {
    asset(include_bytes!(concat!(env!("OUT_DIR"), "/cassandra_check.sh.obf")))
}
pub fn rollback_check_script() -> String {
    asset(include_bytes!(concat!(env!("OUT_DIR"), "/cassandra_rollback_check.sh.obf")))
}
pub fn rollback_script() -> String {
    asset(include_bytes!(concat!(env!("OUT_DIR"), "/cassandra_rollback.sh.obf")))
}

/// POSIX single-quote an argument.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// A script handed over as base64 on bash's stdin (how every script here
/// travels), with its arguments quoted and its exit code appended as a
/// `__CC_RC__<n>` marker. `exec_remote_command` discards the command's own
/// exit code, so without the marker a failed script is indistinguishable from
/// a successful one.
pub fn invocation(script: &str, args: &[&str]) -> String {
    // The sentinel is the script's own first statement: empty, partial or cut
    // stdin (e.g. no `base64`) never prints it, so `script_rc` can tell.
    let b64 = base64::engine::general_purpose::STANDARD.encode(format!("echo __CC_RAN__\n{script}").as_bytes());
    let quoted: Vec<String> = args.iter().map(|a| shell_quote(a)).collect();
    format!(
        "out=$(echo {b64} | base64 -d | bash -s -- {} 2>&1); rc=$?; printf '%s\\n' \"$out\"; echo \"__CC_RC__$rc\"",
        quoted.join(" ")
    )
}

/// A bundled script's exit code, only when the output proves the script ran
/// (its first statement printed `__CC_RAN__`) and the wrapper reported a code.
pub(crate) fn script_rc(out: &str) -> Option<i32> {
    if out.lines().any(|l| l.trim_end() == "__CC_RAN__") {
        parse_rc(out)
    } else {
        None
    }
}

/// Run `f` for every target on its own thread, keeping input order.
pub(crate) fn per_node<T: Send>(
    targets: &[Target],
    f: &(dyn Fn(&Target) -> T + Sync),
) -> Vec<T> {
    std::thread::scope(|s| {
        let handles: Vec<_> = targets.iter().map(|t| s.spawn(move || f(t))).collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("node worker panicked"))
            .collect()
    })
}

fn read_cert(exec: ExecFn, t: &Target) -> Result<(Option<CertInfo>, String, String), String> {
    let cmd = invocation(&check_script(), &[]);
    let out = exec(&t.instance_id, &cmd, QUICK_TIMEOUT)?;
    let raw = parse_check(&out).ok_or_else(|| "the cert check returned no readable result".to_string())?;
    let cert_text = out
        .split("__CC_CERT_BEGIN__")
        .nth(1)
        .and_then(|s| s.split("__CC_CERT_END__").next())
        .unwrap_or("")
        .trim()
        .to_string();
    Ok((raw.cert, raw.active, cert_text))
}

#[derive(Clone, Debug)]
pub struct NodeDryRun {
    pub target: Target,
    pub current: Option<CertInfo>,
    /// The raw openssl text, kept so the "after" output can be compared by eye.
    pub raw_cert: String,
    pub dry_run_ok: bool,
    pub detail: String,
}

pub fn dry_run(exec: ExecFn, targets: &[Target], domain_arg: Option<&str>) -> Vec<NodeDryRun> {
    per_node(targets, &|t| {
        let mut res = NodeDryRun {
            target: t.clone(),
            current: None,
            raw_cert: String::new(),
            dry_run_ok: false,
            detail: String::new(),
        };
        match read_cert(exec, t) {
            Ok((cert, _active, raw)) => {
                res.current = cert;
                res.raw_cert = raw;
            }
            Err(e) => {
                res.detail = format!("could not read the current cert: {e}");
                return res;
            }
        }
        let mut args: Vec<&str> = Vec::new();
        if let Some(d) = domain_arg {
            args.extend(["-d", d]);
        }
        args.push("--dry-run");
        match exec(&t.instance_id, &invocation(&renew_script(), &args), SCRIPT_TIMEOUT) {
            Err(e) => res.detail = format!("dry run could not be sent: {e}"),
            Ok(out) => match script_rc(&out) {
                Some(0) => res.dry_run_ok = true,
                Some(rc) => res.detail = format!("dry run exited {rc}: {}", last_lines(&out, 3)),
                None => res.detail = "dry run returned no return code (the script did not run to completion)".into(),
            },
        }
        res
    })
}

/// Last `n` non-marker lines of a script's output, for an error message.
pub(crate) fn last_lines(out: &str, n: usize) -> String {
    let lines: Vec<&str> = out
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with("__CC_RC__"))
        .collect();
    lines[lines.len().saturating_sub(n)..].join(" | ")
}

pub fn dry_run_passed(results: &[NodeDryRun]) -> bool {
    !results.is_empty() && results.iter().all(|r| r.dry_run_ok)
}

#[derive(Clone, Debug)]
pub struct NodePreflight {
    pub target: Target,
    pub raw: Result<PreflightRaw, String>,
    /// What the node serves now; `None` when it could not be read.
    pub served: Option<CertInfo>,
}

pub fn preflight(exec: ExecFn, targets: &[Target]) -> Vec<NodePreflight> {
    per_node(targets, &|t| {
        let raw = exec(&t.instance_id, &invocation(&rollback_check_script(), &[]), QUICK_TIMEOUT)
            .and_then(|out| {
                parse_preflight(&out).ok_or_else(|| "the preflight returned no readable result".to_string())
            });
        let served = read_cert(exec, t).ok().and_then(|(c, _, _)| c);
        NodePreflight { target: t.clone(), raw, served }
    })
}

/// Time source for the watch. Tests pass a fake clock whose `sleep` advances
/// `now`; the GUI passes `Instant`-backed closures and `thread::sleep`.
pub struct Pacer<'a> {
    pub now: &'a (dyn Fn() -> u64 + Sync),
    pub sleep: &'a (dyn Fn(Duration) + Sync),
}

const POLL: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeStatus {
    StageFailed(String),
    /// Another node failed to stage, so this one was left alone: it holds the
    /// newly staged keystore and will switch cert at its next restart; the
    /// previous stores are backed up as `<keystore>.bak.<TS>`.
    NotRestarted,
    RestartFailed(String),
    DidNotStabilise,
    /// Active for the whole requirement. `change` is `None` when there was no
    /// "before" capture to compare against.
    Up { change: Option<CertChange> },
    /// Active, but the cert could not be read afterwards.
    Unverified(String),
}

pub struct ApplyInput<'a> {
    pub targets: &'a [Target],
    pub unselected: &'a [Target],
    pub domain_arg: Option<&'a str>,
    /// Each selected node's cert from the dry run, by instance id.
    pub before: &'a HashMap<String, CertInfo>,
    pub required_secs: u64,
    pub ceiling_secs: u64,
}

#[derive(Clone, Debug)]
pub struct ApplyReport {
    pub nodes: Vec<(Target, NodeStatus)>,
    /// Unselected nodes not serving what the selected ones now do. `None` means
    /// the check was NOT run (no restart, or no selected node came up with a
    /// readable cert to compare against); `Some(vec![])` means it ran and every
    /// unselected node is consistent.
    pub stale: Option<Vec<String>>,
    pub restarted: bool,
}

/// `systemctl restart --no-block`: returns as soon as systemd has queued it,
/// so ten nodes' send-commands are not each held for a Cassandra start. The
/// watch is what proves a node actually came up. A plain command, not a
/// bundled script, so its verdict is the plain `__CC_RC__` and has no
/// `__CC_RAN__` sentinel.
const RESTART_CMD: &str = "systemctl restart --no-block cassandra; echo \"__CC_RC__$?\"";
const ACTIVE_CMD: &str = "systemctl is-active cassandra";

/// Restart every target at the same moment, then poll until each is judged.
/// The first observation of every node happens right after the restarts are
/// issued, so the ceiling is measured from the restart.
fn restart_and_watch(
    exec: ExecFn,
    targets: &[Target],
    required: u64,
    ceiling: u64,
    pacer: &Pacer,
    emit: &(dyn Fn(String) + Sync),
) -> Vec<Result<WatchState, String>> {
    // One thread per node, released together so the restarts are issued in
    // the same instant rather than one after another.
    let barrier = std::sync::Barrier::new(targets.len().max(1));
    let sent: Vec<Result<(), String>> = per_node(targets, &|t| {
        barrier.wait();
        match exec(&t.instance_id, RESTART_CMD, QUICK_TIMEOUT) {
            Err(e) => Err(e),
            Ok(out) => match parse_rc(&out) {
                Some(0) => Ok(()),
                Some(rc) => Err(format!("systemctl restart exited {rc}")),
                None => Err("restart returned no return code".into()),
            },
        }
    });
    emit(format!("restart issued to {} node(s)", sent.iter().filter(|r| r.is_ok()).count()));

    let mut watches: Vec<StabilityWatch> =
        targets.iter().map(|_| StabilityWatch::new(required, ceiling)).collect();
    let mut states: Vec<WatchState> = vec![WatchState::Pending; targets.len()];
    loop {
        let now = (pacer.now)();
        let live: Vec<usize> = (0..targets.len())
            .filter(|&i| sent[i].is_ok() && states[i] == WatchState::Pending)
            .collect();
        if live.is_empty() {
            break;
        }
        let subset: Vec<Target> = live.iter().map(|&i| targets[i].clone()).collect();
        let actives: Vec<bool> = per_node(&subset, &|t| {
            exec(&t.instance_id, ACTIVE_CMD, QUICK_TIMEOUT)
                .map(|o| o.trim() == "active")
                .unwrap_or(false)
        });
        for (k, &i) in live.iter().enumerate() {
            states[i] = watches[i].observe(now, actives[k]);
            if states[i] != WatchState::Pending {
                emit(format!("{}: {:?}", targets[i].name, states[i]));
            }
        }
        if live.iter().all(|&i| states[i] != WatchState::Pending) {
            break;
        }
        (pacer.sleep)(POLL);
    }
    sent.into_iter().zip(states).map(|(r, s)| r.map(|_| s)).collect()
}

pub fn apply(
    exec: ExecFn,
    input: &ApplyInput,
    pacer: &Pacer,
    emit: &(dyn Fn(String) + Sync),
) -> ApplyReport {
    // 1. Stage everywhere. cassandra.sh backs up the old stores itself.
    let mut args: Vec<&str> = Vec::new();
    if let Some(d) = input.domain_arg {
        args.extend(["-d", d]);
    }
    args.push("--no-restart");
    let staged: Vec<Result<(), String>> = per_node(input.targets, &|t| {
        match exec(&t.instance_id, &invocation(&renew_script(), &args), SCRIPT_TIMEOUT) {
            Err(e) => Err(format!("could not be sent: {e}")),
            Ok(out) => match script_rc(&out) {
                Some(0) => Ok(()),
                Some(rc) => Err(format!("exited {rc}: {}", last_lines(&out, 3))),
                None => Err("returned no return code (the script did not run to completion)".into()),
            },
        }
    });
    emit(format!("staged {} node(s)", staged.iter().filter(|r| r.is_ok()).count()));

    // 2. One failure and nobody restarts: half a cluster on a new keystore is
    //    worse than none.
    if input.targets.is_empty() || staged.iter().any(|r| r.is_err()) {
        let nodes = input
            .targets
            .iter()
            .cloned()
            .zip(staged)
            .map(|(t, r)| match r {
                Ok(()) => (t, NodeStatus::NotRestarted),
                Err(e) => (t, NodeStatus::StageFailed(e)),
            })
            .collect();
        return ApplyReport { nodes, stale: None, restarted: false };
    }

    // 3-4. Restart together and watch.
    let watched = restart_and_watch(exec, input.targets, input.required_secs, input.ceiling_secs, pacer, emit);
    let restarted = watched.iter().any(|w| w.is_ok());

    // 5. Verify what each node serves now.
    let certs: Vec<Result<Option<CertInfo>, String>> =
        per_node(input.targets, &|t| read_cert(exec, t).map(|(c, _, _)| c));
    let mut reference: Option<CertInfo> = None;
    let nodes: Vec<(Target, NodeStatus)> = input
        .targets
        .iter()
        .cloned()
        .zip(watched)
        .zip(certs)
        .map(|((t, w), c)| {
            let st = match w {
                Err(e) => NodeStatus::RestartFailed(e),
                Ok(WatchState::Failed) | Ok(WatchState::Pending) => NodeStatus::DidNotStabilise,
                Ok(WatchState::Stable) => match c {
                    Err(e) => NodeStatus::Unverified(e),
                    Ok(None) => NodeStatus::Unverified("the node serves no readable cert".into()),
                    Ok(Some(after)) => {
                        let change = input.before.get(&t.instance_id).map(|b| compare_certs(b, &after));
                        // Whatever came up and could be read is what the
                        // unselected nodes are compared with, renewed or not:
                        // a flagged or unrenewed result is exactly when the
                        // others matter.
                        if reference.is_none() {
                            reference = Some(after);
                        }
                        NodeStatus::Up { change }
                    }
                },
            };
            (t, st)
        })
        .collect();

    // Read-only: the check script only, never a change.
    let stale = reference.map(|result| {
        let others: Vec<(String, Option<CertInfo>)> = per_node(input.unselected, &|t| {
            (t.name.clone(), read_cert(exec, t).ok().and_then(|(c, _, _)| c))
        });
        stale_unselected(&result, &others)
    });
    ApplyReport { nodes, stale, restarted }
}

#[derive(Clone, Debug)]
pub struct RollbackNode {
    pub target: Target,
    /// The backup timestamp chosen for this node.
    pub ts: String,
}

pub struct RollbackInput<'a> {
    pub restore: &'a [RollbackNode],
    /// Nodes the preflight found already on the old cert; left untouched.
    pub skipped: &'a [Target],
    pub unselected: &'a [Target],
    pub old: &'a OldCert,
    pub required_secs: u64,
    pub ceiling_secs: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RollbackStatus {
    RestoreFailed(String),
    /// The node holds the restored old stores but has not been restarted, so it
    /// still serves the cert it had before; the stores it had before are saved
    /// as `<keystore>.rollback.<TS>`.
    NotRestarted,
    NothingToRollBack,
    RestartFailed(String),
    DidNotStabilise,
    /// Active for the whole requirement and serving the old cert.
    Up,
    /// Came up and the cert was read, but it is not the old cert.
    WrongCert(String),
    /// Came up, but the cert could not be read afterwards.
    Unverified(String),
}

#[derive(Clone, Debug)]
pub struct RollbackReport {
    pub nodes: Vec<(Target, RollbackStatus)>,
    /// Unselected nodes not serving the cert rolled back to. `None` means the
    /// check was NOT run (the restore aborted or no restart was sent);
    /// `Some(vec![])` means it ran and every unselected node is consistent.
    pub stale: Option<Vec<String>>,
    pub restarted: bool,
}

/// A restore succeeded only if the script ran, said so, said nothing of
/// failure, and exited 0. Anything else is a failure, with the best reason.
fn restore_verdict(out: &str) -> Result<(), String> {
    if let Some(reason) = out
        .lines()
        .find_map(|l| l.trim().strip_prefix("__CC_RESTORE_FAIL__"))
    {
        return Err(format!("restore failed: {}", reason.trim()));
    }
    if out.contains("__CC_RESTORE_OK__") && script_rc(out) == Some(0) {
        Ok(())
    } else {
        Err(format!("no restore confirmation: {}", last_lines(out, 3)))
    }
}

pub fn rollback(
    exec: ExecFn,
    input: &RollbackInput,
    pacer: &Pacer,
    emit: &(dyn Fn(String) + Sync),
) -> RollbackReport {
    let skipped: Vec<(Target, RollbackStatus)> = input
        .skipped
        .iter()
        .cloned()
        .map(|t| (t, RollbackStatus::NothingToRollBack))
        .collect();
    let restore_targets: Vec<Target> = input.restore.iter().map(|r| r.target.clone()).collect();

    // 1. Restore everywhere first. The script saves .rollback copies itself.
    let restored: Vec<Result<(), String>> = std::thread::scope(|s| {
        let hs: Vec<_> = input
            .restore
            .iter()
            .map(|r| {
                s.spawn(move || {
                    exec(
                        &r.target.instance_id,
                        &invocation(&rollback_script(), &["--restore", &r.ts]),
                        SCRIPT_TIMEOUT,
                    )
                    .map_err(|e| format!("could not be sent: {e}"))
                    .and_then(|out| restore_verdict(&out))
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().expect("restore worker panicked")).collect()
    });
    emit(format!("restored {} node(s)", restored.iter().filter(|r| r.is_ok()).count()));

    if restore_targets.is_empty() || restored.iter().any(|r| r.is_err()) {
        let mut nodes: Vec<(Target, RollbackStatus)> = restore_targets
            .into_iter()
            .zip(restored)
            .map(|(t, r)| match r {
                Ok(()) => (t, RollbackStatus::NotRestarted),
                Err(e) => (t, RollbackStatus::RestoreFailed(e)),
            })
            .collect();
        nodes.extend(skipped);
        return RollbackReport { nodes, stale: None, restarted: false };
    }

    // 2. Restart together, watch, verify against the cert rolled back to.
    let watched = restart_and_watch(exec, &restore_targets, input.required_secs, input.ceiling_secs, pacer, emit);
    let restarted = watched.iter().any(|w| w.is_ok());
    let certs: Vec<Result<Option<CertInfo>, String>> =
        per_node(&restore_targets, &|t| read_cert(exec, t).map(|(c, _, _)| c));
    let mut nodes: Vec<(Target, RollbackStatus)> = restore_targets
        .iter()
        .cloned()
        .zip(watched)
        .zip(certs)
        .map(|((t, w), c)| {
            let st = match w {
                Err(e) => RollbackStatus::RestartFailed(e),
                Ok(WatchState::Failed) | Ok(WatchState::Pending) => RollbackStatus::DidNotStabilise,
                Ok(WatchState::Stable) => match c {
                    Ok(Some(served)) if served_matches(&served, input.old) => RollbackStatus::Up,
                    Ok(Some(_)) => RollbackStatus::WrongCert("it is not serving the cert rolled back to".into()),
                    Ok(None) => RollbackStatus::WrongCert("it serves no readable cert".into()),
                    Err(e) => RollbackStatus::Unverified(e),
                },
            };
            (t, st)
        })
        .collect();
    nodes.extend(skipped);

    // 3. Unselected nodes still on the cert being rolled away from (read-only).
    let others: Vec<(String, Option<CertInfo>)> = per_node(input.unselected, &|t| {
        (t.name.clone(), read_cert(exec, t).ok().and_then(|(c, _, _)| c))
    });
    let stale = if restarted { Some(stale_against(input.old, &others)) } else { None };
    RollbackReport { nodes, stale, restarted }
}

/// Selected nodes an apply left in a state worth diagnosing. `NotRestarted`
/// is not one: that node was deliberately left alone. An `Up` node counts
/// when its cert did not move forward or looks wrong.
pub fn apply_failed_nodes(report: &ApplyReport) -> Vec<Target> {
    report
        .nodes
        .iter()
        .filter(|(_, s)| match s {
            NodeStatus::StageFailed(_)
            | NodeStatus::RestartFailed(_)
            | NodeStatus::DidNotStabilise
            | NodeStatus::Unverified(_) => true,
            NodeStatus::Up { change } => {
                matches!(change, Some(CertChange::NotRenewed) | Some(CertChange::Flagged(_)))
            }
            NodeStatus::NotRestarted => false,
        })
        .map(|(t, _)| t.clone())
        .collect()
}

/// Nodes a rollback left in a state worth diagnosing.
pub fn rollback_failed_nodes(report: &RollbackReport) -> Vec<Target> {
    report
        .nodes
        .iter()
        .filter(|(_, s)| match s {
            RollbackStatus::RestoreFailed(_)
            | RollbackStatus::RestartFailed(_)
            | RollbackStatus::DidNotStabilise
            | RollbackStatus::WrongCert(_)
            | RollbackStatus::Unverified(_) => true,
            RollbackStatus::NotRestarted | RollbackStatus::NothingToRollBack | RollbackStatus::Up => false,
        })
        .map(|(t, _)| t.clone())
        .collect()
}

/// The one read-only look at a failing node: the unit's status and its
/// recent journal, both bounded.
pub const DIAGNOSTICS_CMD: &str = "systemctl status cassandra --no-pager -l 2>&1 | tail -n 20; echo ----; \
     journalctl -u cassandra -n 30 --no-pager 2>&1 | tail -n 30";
const DIAGNOSTICS_TIMEOUT: Duration = Duration::from_secs(45);

/// Run `DIAGNOSTICS_CMD` on every target at once. `(node name, text)`; a
/// failed read is kept as the text, so a node is never silently missing.
pub fn diagnostics(exec: ExecFn, targets: &[Target]) -> Vec<(String, String)> {
    per_node(targets, &|t| {
        let text = match exec(&t.instance_id, DIAGNOSTICS_CMD, DIAGNOSTICS_TIMEOUT) {
            Ok(out) => out,
            Err(e) => format!("diagnostics could not be read: {e}"),
        };
        (t.name.clone(), text)
    })
}

/// How a result line is drawn: green, plain, amber or red.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Good,
    Neutral,
    Warn,
    Bad,
}

impl Severity {
    /// Worth a warning in the log and the Roll back shortcut in the dialog.
    pub fn is_problem(self) -> bool {
        matches!(self, Severity::Warn | Severity::Bad)
    }
}

/// One node's apply outcome in words, for the result panel and the log.
/// ASCII only.
pub fn describe_apply_status(status: &NodeStatus) -> (Severity, String) {
    match status {
        NodeStatus::StageFailed(e) => (Severity::Bad, format!("stage failed: {e}")),
        NodeStatus::NotRestarted => (
            Severity::Warn,
            "not restarted (another node failed to stage): left on the newly staged \
             keystore; it switches cert at its next restart; backup is `<keystore>.bak.<TS>`"
                .to_string(),
        ),
        NodeStatus::RestartFailed(e) => (Severity::Bad, format!("restart outcome unknown: {e}")),
        NodeStatus::DidNotStabilise => (
            Severity::Bad,
            "did not stay active for the required time after the restart".to_string(),
        ),
        NodeStatus::Up { change: Some(CertChange::Renewed) } => {
            (Severity::Good, "Renewed: up, serving the new cert".to_string())
        }
        NodeStatus::Up { change: Some(CertChange::NotRenewed) } => (
            Severity::Bad,
            "Not renewed: up, but the expiry date did not move".to_string(),
        ),
        NodeStatus::Up { change: Some(CertChange::Flagged(diffs)) } => (
            Severity::Bad,
            format!("Flagged: up, but the new cert looks different: {}", diffs.join("; ")),
        ),
        NodeStatus::Up { change: None } => (
            Severity::Warn,
            "up, but there was no before capture to compare the cert against".to_string(),
        ),
        NodeStatus::Unverified(e) => (
            Severity::Bad,
            format!("unverified: up, but the cert could not be read: {e}"),
        ),
    }
}

/// One node's rollback outcome in words. ASCII only.
pub fn describe_rollback_status(status: &RollbackStatus) -> (Severity, String) {
    match status {
        RollbackStatus::RestoreFailed(e) => (Severity::Bad, format!("restore failed: {e}")),
        RollbackStatus::NotRestarted => (
            Severity::Warn,
            "not restarted (another node failed to restore): left on the restored old \
             stores, still serving the cert it had; the stores it had are saved as \
             `<keystore>.rollback.<TS>`"
                .to_string(),
        ),
        RollbackStatus::NothingToRollBack => (
            Severity::Neutral,
            "Nothing to roll back: it already served the old cert; left untouched".to_string(),
        ),
        RollbackStatus::RestartFailed(e) => {
            (Severity::Bad, format!("restart outcome unknown: {e}"))
        }
        RollbackStatus::DidNotStabilise => (
            Severity::Bad,
            "did not stay active for the required time after the restart".to_string(),
        ),
        RollbackStatus::Up => (
            Severity::Good,
            "Rolled back: up, serving the old cert".to_string(),
        ),
        RollbackStatus::WrongCert(e) => {
            (Severity::Bad, format!("up, but not serving the old cert: {e}"))
        }
        RollbackStatus::Unverified(e) => (
            Severity::Bad,
            format!("unverified: up, but the cert could not be read: {e}"),
        ),
    }
}

#[cfg(test)]
mod script_tests {
    const RENEW: &str = include_str!("../assets/scripts/cassandra.sh");
    const CHECK: &str = include_str!("../assets/scripts/cassandra_check.sh");
    const RB_CHECK: &str = include_str!("../assets/scripts/cassandra_rollback_check.sh");
    const RB: &str = include_str!("../assets/scripts/cassandra_rollback.sh");

    /// Executable lines only: comments name the commands a script must never
    /// run, and that prose is worth keeping.
    fn code(script: &str) -> Vec<&str> {
        script
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect()
    }

    const MUTATING: &[&str] = &[
        "systemctl restart", "systemctl stop", "systemctl start", "rm ", "rm -",
        "cp ", "mv ", "install ", "chmod ", "chown ", "tee ", "sed -i", "mkdir ",
        "touch ", "truncate ", "dd ",
    ];

    fn assert_read_only(name: &str, script: &str) {
        for line in code(script) {
            for verb in MUTATING {
                assert!(!line.contains(verb), "{name} runs `{verb}`: {line}");
            }
            let cleaned = line
                .replace("2>/dev/null", "")
                .replace("2>&1", "")
                .replace(">/dev/null", "");
            assert!(!cleaned.contains('>'), "{name} redirects output: {line}");
        }
    }

    #[test]
    fn the_check_scripts_change_nothing_on_the_box() {
        assert_read_only("cassandra_check.sh", CHECK);
        assert_read_only("cassandra_rollback_check.sh", RB_CHECK);
    }

    #[test]
    fn no_script_enables_errexit() {
        for (name, s) in [("renew", RENEW), ("check", CHECK), ("rb_check", RB_CHECK), ("rb", RB)] {
            for line in code(s) {
                if let Some(rest) = line.strip_prefix("set ") {
                    for flag in rest.split_whitespace() {
                        let short = flag.trim_start_matches('-');
                        assert!(
                            !(flag.starts_with('-') && short.contains('e')),
                            "{name} enables errexit: {line}"
                        );
                        assert!(flag != "errexit", "{name}: {line}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_renew_script_still_has_the_flags_the_app_drives() {
        for flag in ["--dry-run", "--no-restart", "-d DOMAIN", "-p STORE_PASSWORD"] {
            assert!(RENEW.contains(flag), "cassandra.sh lost `{flag}`");
        }
    }

    #[test]
    fn the_rollback_never_restarts_cassandra() {
        for line in code(RB) {
            assert!(!line.contains("systemctl"), "rollback touches systemd: {line}");
        }
    }

    #[test]
    fn the_rollback_saves_the_current_stores_before_overwriting_them() {
        // Executable lines only, so a header comment cannot satisfy the order.
        let body = code(RB).join("\n");
        let save = body.find(".rollback.").expect("saves a .rollback copy");
        let install = body.find("install -m").expect("installs the backup");
        assert!(save < install, "the safety copy must precede the overwrite");
    }

    #[test]
    fn the_scripts_emit_the_markers_the_parsers_read() {
        for m in ["__CC_BEGIN__", "__CC_CERT_BEGIN__", "__CC_CERT_END__", "__CC_ACTIVE__", "__CC_END__"] {
            assert!(CHECK.contains(m), "check script never emits {m}");
        }
        for m in ["__CC_PF_BEGIN__", "__CC_PF_BACKUP__", "__CC_PF_SPACE__", "__CC_PF_END__"] {
            assert!(RB_CHECK.contains(m), "rollback check never emits {m}");
        }
        for m in ["__CC_RESTORE_OK__", "__CC_RESTORE_FAIL__"] {
            assert!(RB.contains(m), "rollback never emits {m}");
        }
    }

    #[test]
    fn the_rollback_scripts_read_the_password_from_cassandra_yaml_first() {
        for (name, s) in [("rb_check", RB_CHECK), ("rb", RB)] {
            assert!(s.contains("detect_store_password"), "{name}");
            assert!(s.contains("keystore_password:"), "{name}");
        }
    }

    #[test]
    fn the_rollback_only_accepts_a_numeric_timestamp() {
        assert!(RB.contains("[0-9]{14}"), "the --restore argument must be validated");
    }

    #[test]
    fn the_preflight_compares_ownership_only_and_lists_only_real_backups() {
        let lines = code(RB_CHECK);
        for l in lines.iter().filter(|l| l.contains("stat -c '%U:%G")) {
            assert!(!l.contains("%a"), "perms check must ignore mode: {l}");
        }
        assert!(
            lines.iter().any(|l| l.contains("stat -c '%U:%G'")),
            "ownership comparison missing"
        );
        assert!(
            lines.iter().any(|l| l.contains("[0-9]{14}") && l.contains("continue")),
            "non-timestamp .bak.* entries must be skipped"
        );
        assert!(
            lines.iter().any(|l| l.contains("LC_ALL=C keytool")),
            "keytool must run under LC_ALL=C"
        );
        assert!(
            lines.iter().any(|l| l.contains("LC_ALL=C date -u")),
            "date must run under LC_ALL=C"
        );
    }
}

#[cfg(test)]
mod flow_tests {
    use super::*;
    use std::sync::Mutex;

    /// A fake box: records every command and answers from a closure that sees
    /// (instance id, command).
    struct Fake {
        calls: Mutex<Vec<(String, String)>>,
        reply: Box<dyn Fn(&str, &str) -> Result<String, String> + Sync + Send>,
    }
    impl Fake {
        fn new(reply: impl Fn(&str, &str) -> Result<String, String> + Sync + Send + 'static) -> Self {
            Self { calls: Mutex::new(Vec::new()), reply: Box::new(reply) }
        }
        fn exec(&self, id: &str, cmd: &str, _t: Duration) -> Result<String, String> {
            self.calls.lock().unwrap().push((id.to_string(), cmd.to_string()));
            (self.reply)(id, cmd)
        }
        fn commands_for(&self, id: &str) -> Vec<String> {
            self.calls.lock().unwrap().iter().filter(|(i, _)| i == id).map(|(_, c)| c.clone()).collect()
        }
    }

    pub(super) fn t(id: &str, name: &str) -> Target {
        Target { instance_id: id.into(), name: name.into() }
    }

    const CERT: &str = "subject= /CN=*.a\nissuer= /CN=ca\nnotBefore=Sep  3 07:01:23 2025 GMT\nnotAfter=Oct  3 08:01:22 2026 GMT\nserial=01\n";
    fn check_out(active: &str) -> String {
        format!("__CC_BEGIN__\n__CC_CERT_BEGIN__\n{CERT}__CC_CERT_END__\n__CC_ACTIVE__ {active}\n__CC_END__\n")
    }

    #[test]
    fn a_shell_argument_is_quoted_not_trusted() {
        assert_eq!(shell_quote("dev1.net"), "'dev1.net'");
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn an_invocation_appends_the_return_code_marker() {
        let cmd = invocation("echo hi", &["--dry-run", "-d", "dev1.net"]);
        assert!(cmd.starts_with("out=$(echo "), "{cmd}");
        assert!(cmd.contains("base64 -d | bash -s -- '--dry-run' '-d' 'dev1.net'"), "{cmd}");
        assert!(cmd.contains("printf '%s\\n' \"$out\""), "{cmd}");
        assert!(cmd.contains("rc=$?"), "{cmd}");
        assert!(cmd.ends_with("echo \"__CC_RC__$rc\""), "{cmd}");
    }

    #[test]
    fn the_payload_starts_with_the_ran_sentinel() {
        use base64::Engine;
        let cmd = invocation("echo body", &[]);
        let b64 = cmd.trim_start_matches("out=$(echo ").split(' ').next().unwrap();
        let text = String::from_utf8(base64::engine::general_purpose::STANDARD.decode(b64).unwrap()).unwrap();
        assert!(text.starts_with("echo __CC_RAN__\n"), "{text}");
        assert!(text.ends_with("echo body"), "{text}");
    }

    #[test]
    fn script_rc_needs_both_the_ran_line_and_the_code() {
        assert_eq!(script_rc("__CC_RAN__\n"), None);
        assert_eq!(script_rc("x\n__CC_RC__0\n"), None);
        assert_eq!(script_rc("__CC_RAN__\nx\n__CC_RC__3\n"), Some(3));
        assert_eq!(script_rc("echo __CC_RAN__ done\n__CC_RC__0\n"), None);
    }

    #[test]
    fn a_dry_run_that_never_ran_the_script_fails_even_with_rc_zero() {
        let fake = Fake::new(|_id, cmd| {
            if cmd.contains("--dry-run") { Ok("__CC_RC__0\n".into()) } else { Ok(check_out("active")) }
        });
        let exec = |i: &str, c: &str, t: Duration| fake.exec(i, c, t);
        let out = dry_run(&exec, &[t("i-1", "a")], None);
        assert!(!out[0].dry_run_ok);
        assert!(out[0].detail.contains("no return code"), "{}", out[0].detail);
    }

    #[test]
    fn the_bundled_scripts_deobfuscate_to_the_real_text() {
        assert!(renew_script().contains("cassandra_renew_ssl_cert.sh"));
        assert!(check_script().contains("__CC_CERT_BEGIN__"));
        assert!(rollback_check_script().contains("__CC_PF_BEGIN__"));
        assert!(rollback_script().contains("__CC_RESTORE_OK__"));
    }

    #[test]
    fn the_dry_run_reads_the_cert_and_runs_the_script_with_dry_run() {
        let fake = Fake::new(|_id, cmd| {
            if cmd.contains("--dry-run") {
                Ok("__CC_RAN__\nINFO: validated\n__CC_RC__0\n".into())
            } else {
                Ok(check_out("active"))
            }
        });
        let exec = |i: &str, c: &str, t: Duration| fake.exec(i, c, t);
        let out = dry_run(&exec, &[t("i-1", "cassandra-001")], Some("dev1.net"));
        assert_eq!(out.len(), 1);
        assert!(out[0].dry_run_ok);
        assert!(out[0].current.is_some());
        assert!(out[0].raw_cert.contains("notAfter="));
        let cmds = fake.commands_for("i-1");
        assert!(cmds.iter().any(|c| c.contains("'--dry-run'") && c.contains("'-d' 'dev1.net'")));
        assert!(dry_run_passed(&out));
    }

    #[test]
    fn a_dry_run_with_no_return_code_is_a_failure() {
        let fake = Fake::new(|_id, cmd| {
            if cmd.contains("--dry-run") {
                Ok("INFO: validated...".into()) // truncated: no marker
            } else {
                Ok(check_out("active"))
            }
        });
        let exec = |i: &str, c: &str, t: Duration| fake.exec(i, c, t);
        let out = dry_run(&exec, &[t("i-1", "cassandra-001")], None);
        assert!(!out[0].dry_run_ok);
        assert!(out[0].detail.contains("no return code"), "{}", out[0].detail);
        assert!(!dry_run_passed(&out));
    }

    #[test]
    fn a_failed_dry_run_on_any_node_fails_the_gate() {
        let fake = Fake::new(|id, cmd| {
            if cmd.contains("--dry-run") {
                Ok(format!("__CC_RAN__\n__CC_RC__{}\n", if id == "i-2" { 1 } else { 0 }))
            } else {
                Ok(check_out("active"))
            }
        });
        let exec = |i: &str, c: &str, t: Duration| fake.exec(i, c, t);
        let out = dry_run(&exec, &[t("i-1", "a"), t("i-2", "b")], None);
        assert!(out[0].dry_run_ok && !out[1].dry_run_ok);
        assert!(!dry_run_passed(&out));
        assert!(!dry_run_passed(&[]), "nothing selected cannot pass");
    }

    #[test]
    fn an_unreachable_node_reports_its_error_and_the_rest_continue() {
        let fake = Fake::new(|id, cmd| {
            if id == "i-1" { return Err("TargetNotConnected".into()); }
            if cmd.contains("--dry-run") { Ok("__CC_RAN__\n__CC_RC__0\n".into()) } else { Ok(check_out("active")) }
        });
        let exec = |i: &str, c: &str, t: Duration| fake.exec(i, c, t);
        let out = dry_run(&exec, &[t("i-1", "a"), t("i-2", "b")], None);
        assert!(!out[0].dry_run_ok);
        assert!(out[0].detail.contains("TargetNotConnected"));
        assert!(out[1].dry_run_ok);
    }

    #[test]
    fn the_preflight_reads_backups_and_the_served_cert() {
        // The markers live inside the base64 and are invisible in the command
        // string, so the fake recognises the rollback-check script by its text.
        let rollback_check_b64 = {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.encode(format!("echo __CC_RAN__\n{}", rollback_check_script()).as_bytes())
        };
        let fake = Fake::new(move |_id, cmd| {
            if cmd.contains(&rollback_check_b64) {
                Ok("__CC_PF_BEGIN__\n__CC_PF_BACKUP__ 20260903070123 /k.bak.20260903070123 1791014482 0AB1 1 1 1\n__CC_PF_SPACE__ 1\n__CC_PF_END__\n".into())
            } else {
                Ok(check_out("active"))
            }
        });
        let exec = |i: &str, c: &str, t: Duration| fake.exec(i, c, t);
        let out = preflight(&exec, &[t("i-1", "cassandra-001")]);
        assert_eq!(out.len(), 1);
        let raw = out[0].raw.as_ref().expect("parsed");
        assert_eq!(raw.backups.len(), 1);
        assert_eq!(raw.backups[0].ts, "20260903070123");
        assert!(out[0].served.is_some());
    }

    #[test]
    fn a_preflight_with_no_readable_result_is_an_error_not_no_backups() {
        let fake = Fake::new(|_id, cmd| {
            if cmd.contains("bash -s --") { Ok("garbage".into()) } else { Ok(check_out("active")) }
        });
        let exec = |i: &str, c: &str, t: Duration| fake.exec(i, c, t);
        let out = preflight(&exec, &[t("i-1", "cassandra-001")]);
        assert!(out[0].raw.is_err());
    }

    use crate::cassandra_cert::parse_openssl;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A pacer on a fake clock: `sleep` advances it, `now` reads it.
    struct Clock(AtomicU64);
    impl Clock {
        fn new() -> Self { Clock(AtomicU64::new(0)) }
    }

    fn cert_out(after: &str, serial: &str, active: &str) -> String {
        format!(
            "__CC_BEGIN__\n__CC_CERT_BEGIN__\nsubject= /CN=*.a\nissuer= /CN=ca\nnotBefore=Sep  3 07:01:23 2025 GMT\nnotAfter={after}\nserial={serial}\n__CC_CERT_END__\n__CC_ACTIVE__ {active}\n__CC_END__\n"
        )
    }
    const OLD_AFTER: &str = "Oct  3 08:01:22 2026 GMT";
    const NEW_AFTER: &str = "Nov  3 08:01:22 2027 GMT";
    /// A stage / restore that ran and succeeded (the script printed its sentinel).
    const STAGED: &str = "__CC_RAN__\nINFO: staged\n__CC_RC__0\n";
    const RESTORED: &str = "__CC_RAN__\n__CC_RESTORE_OK__ 20260903070123\n__CC_RC__0\n";

    fn run_apply(
        fake: &Fake,
        targets: &[Target],
        unselected: &[Target],
        before: &HashMap<String, CertInfo>,
    ) -> ApplyReport {
        let clock = Clock::new();
        let now = || clock.0.load(Ordering::SeqCst);
        let sleep = |d: Duration| { clock.0.fetch_add(d.as_secs(), Ordering::SeqCst); };
        let pacer = Pacer { now: &now, sleep: &sleep };
        let exec = |i: &str, c: &str, t: Duration| fake.exec(i, c, t);
        let input = ApplyInput {
            targets,
            unselected,
            domain_arg: Some("dev1.net"),
            before,
            required_secs: 60,
            ceiling_secs: 300,
        };
        apply(&exec, &input, &pacer, &|_| {})
    }

    fn old_cert_info() -> CertInfo {
        parse_openssl(&format!("subject= /CN=*.a\nissuer= /CN=ca\nnotBefore=Sep  3 07:01:23 2025 GMT\nnotAfter={OLD_AFTER}\nserial=01\n")).unwrap()
    }

    fn before_map(ids: &[&str]) -> HashMap<String, CertInfo> {
        ids.iter().map(|id| (id.to_string(), old_cert_info())).collect()
    }

    /// A box that stages fine, restarts fine, comes up active, and serves the new cert afterwards.
    ///
    /// The fakes below tell commands apart by a flag that appears in the
    /// command text (`--no-restart` for staging, `--restore` for a rollback
    /// restore), never by `bash -s --`: the cert-read command is also a
    /// `bash -s --` invocation, and answering it as a stage would hide bugs.
    fn healthy_box() -> Fake {
        Fake::new(|_id, cmd| {
            if cmd.contains("is-active") {
                Ok("active\n".into())
            } else if cmd.contains("systemctl restart") {
                Ok("__CC_RC__0\n".into())
            } else if cmd.contains("--no-restart") {
                Ok(STAGED.into())
            } else {
                Ok(cert_out(NEW_AFTER, "02", "active"))
            }
        })
    }

    #[test]
    fn a_healthy_run_stages_restarts_watches_and_verifies() {
        let fake = healthy_box();
        let targets = [t("i-1", "cassandra-001"), t("i-2", "cassandra-002")];
        let rep = run_apply(&fake, &targets, &[], &before_map(&["i-1", "i-2"]));
        assert!(rep.restarted);
        for (_, st) in &rep.nodes {
            assert_eq!(st, &NodeStatus::Up { change: Some(CertChange::Renewed) });
        }
        // Staging uses --no-restart and the configured domain.
        let c = fake.commands_for("i-1");
        assert!(c.iter().any(|c| c.contains("'-d' 'dev1.net' '--no-restart'")), "{c:?}");
    }

    #[test]
    fn nothing_is_restarted_if_any_node_fails_to_stage() {
        let fake = Fake::new(|id, cmd| {
            if cmd.contains("is-active") || cmd.contains("systemctl restart") {
                panic!("a restart-phase command ran: {cmd}");
            }
            if cmd.contains("--no-restart") {
                let rc = if id == "i-2" { 1 } else { 0 };
                return Ok(format!("__CC_RAN__\n__CC_RC__{rc}\n"));
            }
            Ok(cert_out(OLD_AFTER, "01", "active"))
        });
        let targets = [t("i-1", "cassandra-001"), t("i-2", "cassandra-002")];
        let rep = run_apply(&fake, &targets, &[], &before_map(&["i-1", "i-2"]));
        assert!(!rep.restarted);
        assert!(matches!(rep.nodes[0].1, NodeStatus::NotRestarted));
        assert!(matches!(rep.nodes[1].1, NodeStatus::StageFailed(_)));
    }

    #[test]
    fn a_stage_with_no_return_code_counts_as_failed() {
        let fake = Fake::new(|_id, cmd| {
            if cmd.contains("--no-restart") { return Ok("__CC_RAN__\nINFO: building...".into()); }
            Ok(cert_out(OLD_AFTER, "01", "active"))
        });
        let rep = run_apply(&fake, &[t("i-1", "a")], &[], &before_map(&["i-1"]));
        assert!(!rep.restarted);
        assert!(matches!(rep.nodes[0].1, NodeStatus::StageFailed(_)));
    }

    #[test]
    fn a_stage_that_never_ran_the_script_fails_even_with_rc_zero_and_blocks_every_restart() {
        // rc 0 but no __CC_RAN__: e.g. `base64` missing, so bash read nothing.
        let fake = Fake::new(|id, cmd| {
            if cmd.contains("is-active") || cmd.contains("systemctl restart") {
                panic!("a restart-phase command ran: {cmd}");
            }
            if cmd.contains("--no-restart") {
                return Ok(if id == "i-2" { "__CC_RC__0\n".into() } else { STAGED.into() });
            }
            Ok(cert_out(OLD_AFTER, "01", "active"))
        });
        let targets = [t("i-1", "a"), t("i-2", "b")];
        let rep = run_apply(&fake, &targets, &[], &before_map(&["i-1", "i-2"]));
        assert!(!rep.restarted);
        assert!(matches!(rep.nodes[0].1, NodeStatus::NotRestarted));
        assert!(matches!(rep.nodes[1].1, NodeStatus::StageFailed(_)));
    }

    #[test]
    fn every_node_is_restarted_at_the_same_moment() {
        // A barrier inside the fake's restart reply only releases when ALL
        // restarts are in flight together; a sequential loop would deadlock.
        use std::sync::{Arc, Barrier};
        let barrier = Arc::new(Barrier::new(3));
        let b = barrier.clone();
        let fake = Fake::new(move |_id, cmd| {
            if cmd.contains("systemctl restart") {
                b.wait();
                Ok("__CC_RC__0\n".into())
            } else if cmd.contains("is-active") {
                Ok("active\n".into())
            } else if cmd.contains("--no-restart") {
                Ok(STAGED.into())
            } else {
                Ok(cert_out(NEW_AFTER, "02", "active"))
            }
        });
        let targets = [t("i-1", "a"), t("i-2", "b"), t("i-3", "c")];
        let rep = run_apply(&fake, &targets, &[], &before_map(&["i-1", "i-2", "i-3"]));
        assert!(rep.restarted);
    }

    #[test]
    fn restart_uses_no_block_so_the_send_command_stays_short() {
        let fake = healthy_box();
        run_apply(&fake, &[t("i-1", "a")], &[], &before_map(&["i-1"]));
        assert!(fake.commands_for("i-1").iter().any(|c| c.contains("systemctl restart --no-block cassandra")));
    }

    #[test]
    fn a_node_that_flaps_does_not_pass_the_watch() {
        // Active, then failed, then active: the clock must restart.
        let polls = std::sync::atomic::AtomicUsize::new(0);
        let fake = Fake::new(move |_id, cmd| {
            if cmd.contains("is-active") {
                let n = polls.fetch_add(1, Ordering::SeqCst);
                // 5s polls: active for the first 10 (50s), one failure, then active.
                Ok(if n == 10 { "failed\n" } else { "active\n" }.into())
            } else if cmd.contains("systemctl restart") {
                Ok("__CC_RC__0\n".into())
            } else if cmd.contains("--no-restart") {
                Ok(STAGED.into())
            } else {
                Ok(cert_out(NEW_AFTER, "02", "active"))
            }
        });
        let clock = Clock::new();
        let now = || clock.0.load(Ordering::SeqCst);
        let sleep = |d: Duration| { clock.0.fetch_add(d.as_secs(), Ordering::SeqCst); };
        let pacer = Pacer { now: &now, sleep: &sleep };
        let exec = |i: &str, c: &str, t: Duration| fake.exec(i, c, t);
        let before = before_map(&["i-1"]);
        let targets = [t("i-1", "a")];
        let input = ApplyInput { targets: &targets, unselected: &[], domain_arg: None, before: &before, required_secs: 60, ceiling_secs: 300 };
        let rep = apply(&exec, &input, &pacer, &|_| {});
        assert!(matches!(rep.nodes[0].1, NodeStatus::Up { .. }));
        // It took more than 60s of fake time: the flap restarted the clock.
        assert!(clock.0.load(Ordering::SeqCst) >= 110, "took {}s", clock.0.load(Ordering::SeqCst));
    }

    #[test]
    fn a_node_that_never_comes_up_is_failed_at_the_ceiling() {
        let fake = Fake::new(|_id, cmd| {
            if cmd.contains("is-active") { Ok("activating\n".into()) }
            else if cmd.contains("systemctl restart") { Ok("__CC_RC__0\n".into()) }
            else if cmd.contains("--no-restart") { Ok(STAGED.into()) }
            else { Ok(cert_out(OLD_AFTER, "01", "failed")) }
        });
        let rep = run_apply(&fake, &[t("i-1", "a")], &[], &before_map(&["i-1"]));
        assert_eq!(rep.nodes[0].1, NodeStatus::DidNotStabilise);
    }

    #[test]
    fn a_new_date_on_a_different_cert_shape_is_flagged() {
        let fake = Fake::new(|_id, cmd| {
            if cmd.contains("is-active") { Ok("active\n".into()) }
            else if cmd.contains("systemctl restart") { Ok("__CC_RC__0\n".into()) }
            else if cmd.contains("--no-restart") { Ok(STAGED.into()) }
            else {
                Ok(format!("__CC_BEGIN__\n__CC_CERT_BEGIN__\nsubject= /CN=different\nissuer= /CN=ca\nnotBefore=Sep  3 07:01:23 2025 GMT\nnotAfter={NEW_AFTER}\nserial=02\n__CC_CERT_END__\n__CC_ACTIVE__ active\n__CC_END__\n"))
            }
        });
        let rep = run_apply(&fake, &[t("i-1", "a")], &[], &before_map(&["i-1"]));
        assert!(matches!(&rep.nodes[0].1, NodeStatus::Up { change: Some(CertChange::Flagged(_)) }));
    }

    #[test]
    fn a_failed_restart_command_is_reported_and_the_others_still_judged() {
        let fake = Fake::new(|id, cmd| {
            if cmd.contains("is-active") { Ok("active\n".into()) }
            else if cmd.contains("systemctl restart") {
                Ok(if id == "i-1" { "__CC_RC__1\n" } else { "__CC_RC__0\n" }.into())
            }
            else if cmd.contains("--no-restart") { Ok(STAGED.into()) }
            else { Ok(cert_out(NEW_AFTER, "02", "active")) }
        });
        let rep = run_apply(&fake, &[t("i-1", "a"), t("i-2", "b")], &[], &before_map(&["i-1", "i-2"]));
        assert!(matches!(rep.nodes[0].1, NodeStatus::RestartFailed(_)));
        assert!(matches!(rep.nodes[1].1, NodeStatus::Up { .. }));
    }

    #[test]
    fn unselected_nodes_are_read_never_changed() {
        let fake = Fake::new(|id, cmd| {
            if id == "i-9" {
                assert!(!cmd.contains("systemctl") || cmd.contains("is-active"), "an unselected node was touched: {cmd}");
                assert!(!cmd.contains("bash -s -- '"), "an unselected node ran a script with args: {cmd}");
                return Ok(cert_out(OLD_AFTER, "01", "active"));
            }
            if cmd.contains("is-active") { Ok("active\n".into()) }
            else if cmd.contains("systemctl restart") { Ok("__CC_RC__0\n".into()) }
            else if cmd.contains("--no-restart") { Ok(STAGED.into()) }
            else { Ok(cert_out(NEW_AFTER, "02", "active")) }
        });
        let rep = run_apply(&fake, &[t("i-1", "cassandra-001")], &[t("i-9", "cassandra-101")], &before_map(&["i-1"]));
        assert_eq!(rep.stale, Some(vec!["cassandra-101".to_string()]), "still on the old cert");
    }

    fn run_rollback(fake: &Fake, restore: &[RollbackNode], skipped: &[Target], unselected: &[Target]) -> RollbackReport {
        let clock = Clock::new();
        let now = || clock.0.load(Ordering::SeqCst);
        let sleep = |d: Duration| { clock.0.fetch_add(d.as_secs(), Ordering::SeqCst); };
        let pacer = Pacer { now: &now, sleep: &sleep };
        let exec = |i: &str, c: &str, t: Duration| fake.exec(i, c, t);
        let old = OldCert::from(&old_cert_info());
        let input = RollbackInput { restore, skipped, unselected, old: &old, required_secs: 60, ceiling_secs: 300 };
        rollback(&exec, &input, &pacer, &|_| {})
    }

    fn rb_node(id: &str, ts: &str) -> RollbackNode {
        RollbackNode { target: t(id, id), ts: ts.into() }
    }

    #[test]
    fn a_rollback_restores_everywhere_before_any_restart() {
        let fake = Fake::new(|_id, cmd| {
            if cmd.contains("is-active") { Ok("active\n".into()) }
            else if cmd.contains("systemctl restart") { Ok("__CC_RC__0\n".into()) }
            else if cmd.contains("--restore") { Ok(RESTORED.into()) }
            else { Ok(cert_out(OLD_AFTER, "01", "active")) }
        });
        let rep = run_rollback(&fake, &[rb_node("i-1", "20260903070123")], &[], &[]);
        assert!(rep.restarted);
        assert_eq!(rep.nodes[0].1, RollbackStatus::Up);
        assert!(fake.commands_for("i-1").iter().any(|c| c.contains("'--restore' '20260903070123'")));
    }

    #[test]
    fn a_failed_restore_stops_every_restart_and_surfaces_the_reason() {
        let fake = Fake::new(|id, cmd| {
            if cmd.contains("is-active") || cmd.contains("systemctl restart") {
                panic!("restart-phase command after a failed restore: {cmd}");
            }
            if cmd.contains("--restore") {
                return Ok(if id == "i-2" {
                    "__CC_RAN__\n__CC_RESTORE_FAIL__ no space\n__CC_RC__0\n".into()
                } else {
                    RESTORED.into()
                });
            }
            Ok(cert_out(NEW_AFTER, "02", "active"))
        });
        let rep = run_rollback(&fake, &[rb_node("i-1", "20260903070123"), rb_node("i-2", "20260903070123")], &[], &[]);
        assert!(!rep.restarted);
        assert_eq!(rep.nodes[0].1, RollbackStatus::NotRestarted);
        match &rep.nodes[1].1 {
            RollbackStatus::RestoreFailed(why) => assert!(why.contains("no space"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_restore_that_reports_both_ok_and_fail_is_a_failure() {
        let fake = Fake::new(|_id, cmd| {
            if cmd.contains("is-active") || cmd.contains("systemctl restart") {
                panic!("restart-phase command after a failed restore: {cmd}");
            }
            Ok("__CC_RAN__\n__CC_RESTORE_OK__ 1\n__CC_RESTORE_FAIL__ half done\n__CC_RC__0\n".into())
        });
        let rep = run_rollback(&fake, &[rb_node("i-1", "1")], &[], &[]);
        assert!(!rep.restarted);
        assert!(matches!(&rep.nodes[0].1, RollbackStatus::RestoreFailed(w) if w.contains("half done")));
    }

    #[test]
    fn a_restore_that_never_ran_the_script_fails_even_with_ok_marker_and_rc_zero() {
        let fake = Fake::new(|_id, cmd| {
            if cmd.contains("is-active") || cmd.contains("systemctl restart") {
                panic!("restart-phase command after a failed restore: {cmd}");
            }
            // No __CC_RAN__: the pipeline failed before bash ran anything.
            Ok("__CC_RESTORE_OK__ 1\n__CC_RC__0\n".into())
        });
        let rep = run_rollback(&fake, &[rb_node("i-1", "1")], &[], &[]);
        assert!(!rep.restarted);
        assert!(matches!(rep.nodes[0].1, RollbackStatus::RestoreFailed(_)));
    }

    #[test]
    fn skipped_nodes_are_neither_restored_nor_restarted() {
        let fake = Fake::new(|id, cmd| {
            if id == "i-skip" {
                assert!(!cmd.contains("--restore") && !cmd.contains("systemctl restart"), "{cmd}");
                return Ok(cert_out(OLD_AFTER, "01", "active"));
            }
            if cmd.contains("is-active") { Ok("active\n".into()) }
            else if cmd.contains("systemctl restart") { Ok("__CC_RC__0\n".into()) }
            else if cmd.contains("--restore") { Ok(RESTORED.into()) }
            else { Ok(cert_out(OLD_AFTER, "01", "active")) }
        });
        let rep = run_rollback(&fake, &[rb_node("i-1", "20260903070123")], &[t("i-skip", "cassandra-009")], &[]);
        assert!(rep.restarted);
        assert!(rep.nodes.iter().any(|(tg, st)| tg.instance_id == "i-skip" && *st == RollbackStatus::NothingToRollBack));
    }

    #[test]
    fn a_rollback_is_verified_against_the_old_cert() {
        // The node comes back serving the NEW cert: the restore did not take.
        let fake = Fake::new(|_id, cmd| {
            if cmd.contains("is-active") { Ok("active\n".into()) }
            else if cmd.contains("systemctl restart") { Ok("__CC_RC__0\n".into()) }
            else if cmd.contains("--restore") { Ok(RESTORED.into()) }
            else { Ok(cert_out(NEW_AFTER, "02", "active")) }
        });
        let rep = run_rollback(&fake, &[rb_node("i-1", "20260903070123")], &[], &[]);
        assert!(matches!(rep.nodes[0].1, RollbackStatus::WrongCert(_)));
    }

    #[test]
    fn unselected_nodes_still_on_the_new_cert_are_stale_after_a_rollback() {
        let fake = Fake::new(|id, cmd| {
            if id == "i-9" { return Ok(cert_out(NEW_AFTER, "02", "active")); }
            if cmd.contains("is-active") { Ok("active\n".into()) }
            else if cmd.contains("systemctl restart") { Ok("__CC_RC__0\n".into()) }
            else if cmd.contains("--restore") { Ok(RESTORED.into()) }
            else { Ok(cert_out(OLD_AFTER, "01", "active")) }
        });
        let rep = run_rollback(&fake, &[rb_node("i-1", "1")], &[], &[t("i-9", "cassandra-101")]);
        assert_eq!(rep.stale, Some(vec!["cassandra-101".to_string()]));
    }

    fn flagged_box(unselected_after: &'static str) -> Fake {
        Fake::new(move |id, cmd| {
            if id == "i-9" { return Ok(cert_out(unselected_after, if unselected_after == NEW_AFTER { "02" } else { "01" }, "active")); }
            if cmd.contains("is-active") { Ok("active\n".into()) }
            else if cmd.contains("systemctl restart") { Ok("__CC_RC__0\n".into()) }
            else if cmd.contains("--no-restart") { Ok(STAGED.into()) }
            else {
                Ok(format!("__CC_BEGIN__\n__CC_CERT_BEGIN__\nsubject= /CN=different\nissuer= /CN=ca\nnotBefore=Sep  3 07:01:23 2025 GMT\nnotAfter={NEW_AFTER}\nserial=02\n__CC_CERT_END__\n__CC_ACTIVE__ active\n__CC_END__\n"))
            }
        })
    }

    #[test]
    fn a_flagged_result_still_checks_the_unselected_nodes() {
        let fake = flagged_box(OLD_AFTER);
        let rep = run_apply(&fake, &[t("i-1", "a")], &[t("i-9", "cassandra-101")], &before_map(&["i-1"]));
        assert!(matches!(&rep.nodes[0].1, NodeStatus::Up { change: Some(CertChange::Flagged(_)) }));
        assert_eq!(rep.stale, Some(vec!["cassandra-101".to_string()]));
        let fake = flagged_box(NEW_AFTER);
        let rep = run_apply(&fake, &[t("i-1", "a")], &[t("i-9", "cassandra-101")], &before_map(&["i-1"]));
        assert_eq!(rep.stale, Some(vec![]));
    }

    #[test]
    fn with_no_before_capture_the_unselected_nodes_are_still_checked() {
        let fake = healthy_box();
        let rep = run_apply(&fake, &[t("i-1", "a")], &[t("i-9", "cassandra-101")], &HashMap::new());
        assert!(matches!(rep.nodes[0].1, NodeStatus::Up { change: None }));
        assert!(rep.stale.is_some());
    }

    #[test]
    fn nothing_up_means_the_consistency_check_was_not_run() {
        let fake = Fake::new(|id, cmd| {
            assert_ne!(id, "i-9", "unselected node read although nothing came up");
            if cmd.contains("is-active") { Ok("activating\n".into()) }
            else if cmd.contains("systemctl restart") { Ok("__CC_RC__0\n".into()) }
            else if cmd.contains("--no-restart") { Ok(STAGED.into()) }
            else { Ok(cert_out(OLD_AFTER, "01", "failed")) }
        });
        let rep = run_apply(&fake, &[t("i-1", "a")], &[t("i-9", "cassandra-101")], &before_map(&["i-1"]));
        assert_eq!(rep.nodes[0].1, NodeStatus::DidNotStabilise);
        assert_eq!(rep.stale, None);
    }

    #[test]
    fn no_unselected_nodes_is_a_checked_result() {
        let rep = run_apply(&healthy_box(), &[t("i-1", "a")], &[], &before_map(&["i-1"]));
        assert_eq!(rep.stale, Some(vec![]));
    }

    #[test]
    fn restarted_is_false_when_no_restart_was_sent_ok() {
        let fake = Fake::new(|_id, cmd| {
            if cmd.contains("systemctl restart") { Ok("__CC_RC__1\n".into()) }
            else if cmd.contains("--no-restart") { Ok(STAGED.into()) }
            else { Ok(cert_out(OLD_AFTER, "01", "active")) }
        });
        let rep = run_apply(&fake, &[t("i-1", "a")], &[t("i-9", "b")], &before_map(&["i-1"]));
        assert!(!rep.restarted);
        assert!(matches!(rep.nodes[0].1, NodeStatus::RestartFailed(_)));
        assert_eq!(rep.stale, None);
    }

    #[test]
    fn an_aborted_rollback_did_not_check_the_unselected_nodes() {
        let fake = Fake::new(|id, cmd| {
            assert_ne!(id, "i-9");
            assert!(!cmd.contains("is-active") && !cmd.contains("systemctl restart"));
            Ok("__CC_RAN__\n__CC_RESTORE_FAIL__ x\n__CC_RC__0\n".into())
        });
        let rep = run_rollback(&fake, &[rb_node("i-1", "1")], &[], &[t("i-9", "cassandra-101")]);
        assert!(!rep.restarted);
        assert_eq!(rep.stale, None);
    }

    #[test]
    fn a_rollback_with_no_unselected_nodes_is_a_checked_result() {
        let fake = Fake::new(|_id, cmd| {
            if cmd.contains("is-active") { Ok("active\n".into()) }
            else if cmd.contains("systemctl restart") { Ok("__CC_RC__0\n".into()) }
            else if cmd.contains("--restore") { Ok(RESTORED.into()) }
            else { Ok(cert_out(OLD_AFTER, "01", "active")) }
        });
        let rep = run_rollback(&fake, &[rb_node("i-1", "1")], &[], &[]);
        assert_eq!(rep.stale, Some(vec![]));
    }

    #[test]
    fn an_unreadable_cert_after_a_rollback_is_unverified_not_wrong() {
        let fake = Fake::new(|_id, cmd| {
            if cmd.contains("is-active") { Ok("active\n".into()) }
            else if cmd.contains("systemctl restart") { Ok("__CC_RC__0\n".into()) }
            else if cmd.contains("--restore") { Ok(RESTORED.into()) }
            else { Err("timed out".into()) }
        });
        let rep = run_rollback(&fake, &[rb_node("i-1", "1")], &[], &[]);
        assert!(matches!(&rep.nodes[0].1, RollbackStatus::Unverified(e) if e.contains("timed out")));
    }

    #[test]
    fn an_apply_counts_every_bad_outcome_as_failing_and_nothing_else() {
        let rep = ApplyReport {
            nodes: vec![
                (t("i-1", "a"), NodeStatus::StageFailed("x".into())),
                (t("i-2", "b"), NodeStatus::NotRestarted),
                (t("i-3", "c"), NodeStatus::RestartFailed("x".into())),
                (t("i-4", "d"), NodeStatus::DidNotStabilise),
                (t("i-5", "e"), NodeStatus::Up { change: None }),
                (t("i-6", "f"), NodeStatus::Up { change: Some(CertChange::Renewed) }),
                (t("i-7", "g"), NodeStatus::Up { change: Some(CertChange::NotRenewed) }),
                (t("i-8", "h"), NodeStatus::Up { change: Some(CertChange::Flagged(vec!["s".into()])) }),
                (t("i-9", "i"), NodeStatus::Unverified("x".into())),
            ],
            stale: None,
            restarted: true,
        };
        let ids: Vec<String> = apply_failed_nodes(&rep).into_iter().map(|t| t.instance_id).collect();
        assert_eq!(ids, ["i-1", "i-3", "i-4", "i-7", "i-8", "i-9"]);
    }

    #[test]
    fn a_rollback_counts_every_bad_outcome_as_failing_and_nothing_else() {
        let rep = RollbackReport {
            nodes: vec![
                (t("i-1", "a"), RollbackStatus::RestoreFailed("x".into())),
                (t("i-2", "b"), RollbackStatus::NotRestarted),
                (t("i-3", "c"), RollbackStatus::NothingToRollBack),
                (t("i-4", "d"), RollbackStatus::RestartFailed("x".into())),
                (t("i-5", "e"), RollbackStatus::DidNotStabilise),
                (t("i-6", "f"), RollbackStatus::Up),
                (t("i-7", "g"), RollbackStatus::WrongCert("x".into())),
                (t("i-8", "h"), RollbackStatus::Unverified("x".into())),
            ],
            stale: None,
            restarted: true,
        };
        let ids: Vec<String> = rollback_failed_nodes(&rep).into_iter().map(|t| t.instance_id).collect();
        assert_eq!(ids, ["i-1", "i-4", "i-5", "i-7", "i-8"]);
    }

    #[test]
    fn diagnostics_run_the_one_read_only_command_on_each_node_and_keep_errors() {
        let fake = Fake::new(|id, _cmd| {
            if id == "i-2" { Err("timed out".into()) } else { Ok("Active: failed\n".into()) }
        });
        let exec = |i: &str, c: &str, t: Duration| fake.exec(i, c, t);
        let out = diagnostics(&exec, &[t("i-1", "cassandra-001"), t("i-2", "cassandra-002")]);
        assert_eq!(out[0], ("cassandra-001".to_string(), "Active: failed\n".to_string()));
        assert_eq!(out[1].0, "cassandra-002");
        assert!(out[1].1.contains("timed out"), "{}", out[1].1);
        assert_eq!(fake.commands_for("i-1"), [DIAGNOSTICS_CMD]);
        assert_eq!(
            DIAGNOSTICS_CMD,
            "systemctl status cassandra --no-pager -l 2>&1 | tail -n 20; echo ----; \
             journalctl -u cassandra -n 30 --no-pager 2>&1 | tail -n 30"
        );
    }
}

#[cfg(test)]
mod describe_tests {
    use super::*;
    use crate::cassandra_cert::CertChange;

    fn apply(s: NodeStatus) -> (Severity, String) {
        describe_apply_status(&s)
    }

    fn rollback(s: RollbackStatus) -> (Severity, String) {
        describe_rollback_status(&s)
    }

    #[test]
    fn every_apply_status_has_its_own_words() {
        let (sev, text) = apply(NodeStatus::StageFailed("rc 3".into()));
        assert_eq!(sev, Severity::Bad);
        assert!(text.contains("stage failed") && text.contains("rc 3"), "{text}");

        let (sev, text) = apply(NodeStatus::NotRestarted);
        assert_eq!(sev, Severity::Warn);
        assert!(text.contains("left on the newly staged keystore"), "{text}");
        assert!(text.contains("switches cert at its next restart"), "{text}");
        assert!(text.contains("<keystore>.bak.<TS>"), "{text}");

        let (sev, text) = apply(NodeStatus::RestartFailed("timeout".into()));
        assert_eq!(sev, Severity::Bad);
        assert_eq!(text, "restart outcome unknown: timeout");

        let (sev, text) = apply(NodeStatus::DidNotStabilise);
        assert_eq!(sev, Severity::Bad);
        assert!(text.contains("did not stay active"), "{text}");

        let (sev, text) = apply(NodeStatus::Up { change: Some(CertChange::Renewed) });
        assert_eq!(sev, Severity::Good);
        assert!(text.contains("Renewed"), "{text}");

        let (sev, text) = apply(NodeStatus::Up { change: Some(CertChange::NotRenewed) });
        assert_eq!(sev, Severity::Bad);
        assert!(text.contains("the expiry date did not move"), "{text}");

        let (sev, text) = apply(NodeStatus::Up {
            change: Some(CertChange::Flagged(vec!["subject changed: a -> b".into()])),
        });
        assert_eq!(sev, Severity::Bad);
        assert!(text.contains("subject changed: a -> b"), "{text}");

        let (sev, text) = apply(NodeStatus::Up { change: None });
        assert_eq!(sev, Severity::Warn);
        assert!(text.contains("no before capture"), "{text}");

        let (sev, text) = apply(NodeStatus::Unverified("no output".into()));
        assert_eq!(sev, Severity::Bad);
        assert!(text.contains("unverified") && text.contains("no output"), "{text}");
    }

    #[test]
    fn every_rollback_status_has_its_own_words() {
        let (sev, text) = rollback(RollbackStatus::RestoreFailed("no space".into()));
        assert_eq!(sev, Severity::Bad);
        assert!(text.contains("restore failed") && text.contains("no space"), "{text}");

        let (sev, text) = rollback(RollbackStatus::NotRestarted);
        assert_eq!(sev, Severity::Warn);
        assert!(text.contains("left on the restored old stores"), "{text}");
        assert!(text.contains("<keystore>.rollback.<TS>"), "{text}");

        let (sev, text) = rollback(RollbackStatus::NothingToRollBack);
        assert_eq!(sev, Severity::Neutral);
        assert!(text.contains("Nothing to roll back"), "{text}");

        let (sev, text) = rollback(RollbackStatus::RestartFailed("timeout".into()));
        assert_eq!(sev, Severity::Bad);
        assert_eq!(text, "restart outcome unknown: timeout");

        let (sev, _) = rollback(RollbackStatus::DidNotStabilise);
        assert_eq!(sev, Severity::Bad);

        let (sev, text) = rollback(RollbackStatus::Up);
        assert_eq!(sev, Severity::Good);
        assert!(text.contains("Rolled back"), "{text}");

        let (sev, text) = rollback(RollbackStatus::WrongCert("serial 02".into()));
        assert_eq!(sev, Severity::Bad);
        assert!(text.contains("serial 02"), "{text}");

        let (sev, text) = rollback(RollbackStatus::Unverified("no output".into()));
        assert_eq!(sev, Severity::Bad);
        assert!(text.contains("unverified"), "{text}");
    }

    #[test]
    fn a_problem_is_a_warning_or_worse() {
        assert!(Severity::Bad.is_problem());
        assert!(Severity::Warn.is_problem());
        assert!(!Severity::Good.is_problem());
        assert!(!Severity::Neutral.is_problem());
    }

    #[test]
    fn status_texts_are_ascii() {
        let all = [
            apply(NodeStatus::StageFailed("e".into())),
            apply(NodeStatus::NotRestarted),
            apply(NodeStatus::RestartFailed("e".into())),
            apply(NodeStatus::DidNotStabilise),
            apply(NodeStatus::Up { change: None }),
            apply(NodeStatus::Up { change: Some(CertChange::Renewed) }),
            apply(NodeStatus::Up { change: Some(CertChange::NotRenewed) }),
            apply(NodeStatus::Unverified("e".into())),
            rollback(RollbackStatus::RestoreFailed("e".into())),
            rollback(RollbackStatus::NotRestarted),
            rollback(RollbackStatus::NothingToRollBack),
            rollback(RollbackStatus::RestartFailed("e".into())),
            rollback(RollbackStatus::DidNotStabilise),
            rollback(RollbackStatus::Up),
            rollback(RollbackStatus::WrongCert("e".into())),
            rollback(RollbackStatus::Unverified("e".into())),
        ];
        for (_, text) in all {
            assert!(text.is_ascii(), "{text}");
        }
    }
}

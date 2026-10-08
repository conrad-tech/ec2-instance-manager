//! Cassandra cert renewal and rollback: orchestration over an `exec` closure
//! (instance id, shell command, timeout -> output), so the sequencing is
//! tested with a fake instead of AWS. The decisions live in `cassandra_cert`.

use std::time::Duration;

use base64::Engine;

use crate::cassandra_cert::{
    parse_check, parse_preflight, parse_rc, CertInfo, PreflightRaw,
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
}

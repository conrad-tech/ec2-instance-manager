//! Cassandra cert renewal and rollback: orchestration over an `exec` closure
//! (instance id, shell command, timeout -> output), so the sequencing is
//! tested with a fake instead of AWS. The decisions live in `cassandra_cert`.

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

//! Vault Health Check: build the commands, read each box's output, and judge
//! whether a pair of Vault boxes is healthy.
//!
//! Pure on purpose — the GUI owns the terminals and the dialog. Everything
//! here is text in, verdict out, so the rules that can be *wrong* (what counts
//! as a leader, what an echoed command line looks like next to a real result)
//! are settled by tests rather than by reading a screen.
//!
//! A pair is healthy only when one box is the leader and the other a follower
//! (each judged from its **own** answer, never from which dropdown it was
//! picked in), both report `OVERALL: HEALTHY`, both run the same version, and
//! `docker ps` shows the `vault` container Up on both. Anything missing is a
//! failure, never success.

use serde_json::Value;

/// The health check, run on each box as root. Read-only: three `curl` GETs
/// against the local Vault and a `jq` test.
pub const HEALTH_COMMAND: &str = concat!(
    r#"echo "HEALTH"; curl -ks https://localhost:8200/v1/sys/health | jq '{initialized, sealed, standby, cluster_name, cluster_id, version}'; "#,
    r#"echo "---"; echo "LEADER"; curl -ks https://localhost:8200/v1/sys/leader | jq '{leader_address, is_self}'; "#,
    r#"echo "---"; echo "SEAL STATUS"; curl -ks https://localhost:8200/v1/sys/seal-status | jq '{initialized, sealed, t, n, progress, version}'; "#,
    r#"echo "---"; if curl -ks https://localhost:8200/v1/sys/health | jq -e '.initialized == true and .sealed == false' >/dev/null 2>&1; "#,
    r#"then echo "OVERALL: HEALTHY"; else echo "OVERALL: UNHEALTHY"; fi"#,
);

const DONE_MARKER: &str = "__VAULT_HEALTH_DONE__";

/// The step that prints the done marker. Assembled by the shell at run time:
/// the shell echoes every command line before running it, so a literal
/// marker in the command would be matched off the echo and a tab would read
/// as finished before anything ran. Same trick as `vault_iam::verdict_step`.
fn done_step() -> String {
    "m=DONE; echo \"__VAULT_HEALTH_${m}__\"".to_string()
}

/// The steps sent to each box after `sudo su` and the terminal prep.
pub fn steps() -> Vec<String> {
    vec![HEALTH_COMMAND.to_string(), "docker ps".to_string(), done_step()]
}

/// True once the done marker has been printed as output (a line that is
/// exactly the marker — the echoed command line never is).
pub fn is_done(screen: &str) -> bool {
    screen.lines().any(|l| l.trim() == DONE_MARKER)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Leader,
    Follower,
    Unknown,
}

/// What one box said, as far as it could be read. `None` means "not read",
/// which is never the same as "false".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BoxReport {
    pub initialized: Option<bool>,
    pub sealed: Option<bool>,
    pub standby: Option<bool>,
    pub is_self: Option<bool>,
    pub version: Option<String>,
    pub overall_healthy: Option<bool>,
    pub container_up: Option<bool>,
    pub done: bool,
}

impl BoxReport {
    pub fn role(&self) -> Role {
        if self.is_self == Some(true) && self.standby != Some(true) {
            Role::Leader
        } else if self.standby == Some(true) && self.is_self != Some(true) {
            Role::Follower
        } else {
            Role::Unknown
        }
    }

    /// Whether any of the box's own answers was read at all.
    fn read_anything(&self) -> bool {
        self.initialized.is_some()
            || self.sealed.is_some()
            || self.standby.is_some()
            || self.is_self.is_some()
            || self.version.is_some()
            || self.overall_healthy.is_some()
    }
}

/// The JSON object under the *last* line that is exactly `heading`. The last,
/// because a re-run on the same tab prints a second copy and the newest is
/// the one that matters.
fn section_json(screen: &str, heading: &str) -> Option<Value> {
    let lines: Vec<&str> = screen.lines().collect();
    let start = lines.iter().rposition(|l| l.trim() == heading)?;
    let mut body = String::new();
    let mut closed = false;
    for line in lines.iter().skip(start + 1) {
        body.push_str(line);
        body.push('\n');
        if line.trim() == "}" {
            closed = true;
            break;
        }
    }
    if !closed {
        return None;
    }
    serde_json::from_str(&body).ok()
}

fn json_bool(v: &Option<Value>, key: &str) -> Option<bool> {
    v.as_ref()?.get(key)?.as_bool()
}

fn json_str(v: &Option<Value>, key: &str) -> Option<String> {
    v.as_ref()?.get(key)?.as_str().map(str::to_string)
}

/// Whether `docker ps` output shows a container named exactly `vault` that is
/// Up. `None` when there is no table at all (the command never ran).
fn vault_container_up(screen: &str) -> Option<bool> {
    let lines: Vec<&str> = screen.lines().collect();
    let header = lines
        .iter()
        .rposition(|l| l.contains("CONTAINER ID") && l.contains("NAMES"))?;
    for row in lines.iter().skip(header + 1) {
        let row = row.trim();
        if row.is_empty() || row.starts_with('#') || row.starts_with("__VAULT_HEALTH_") {
            break;
        }
        let name = row.split_whitespace().last();
        if name == Some("vault") && row.contains(" Up ") {
            return Some(true);
        }
    }
    Some(false)
}

/// Read one box's tab text.
pub fn parse_box(screen: &str) -> BoxReport {
    let health = section_json(screen, "HEALTH");
    let leader = section_json(screen, "LEADER");
    let seal = section_json(screen, "SEAL STATUS");
    let overall_healthy = screen
        .lines()
        .rev()
        .map(str::trim)
        .find_map(|l| match l {
            "OVERALL: HEALTHY" => Some(true),
            "OVERALL: UNHEALTHY" => Some(false),
            _ => None,
        });
    BoxReport {
        initialized: json_bool(&health, "initialized").or_else(|| json_bool(&seal, "initialized")),
        sealed: json_bool(&health, "sealed").or_else(|| json_bool(&seal, "sealed")),
        standby: json_bool(&health, "standby"),
        is_self: json_bool(&leader, "is_self"),
        version: json_str(&health, "version").or_else(|| json_str(&seal, "version")),
        overall_healthy,
        container_up: vault_container_up(screen),
        done: is_done(screen),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub healthy: bool,
    pub problems: Vec<String>,
}

/// Judge a pair. One problem string per failed rule; healthy only when there
/// are none.
pub fn judge(a: &BoxReport, b: &BoxReport, label_a: &str, label_b: &str) -> Verdict {
    let mut problems = Vec::new();
    let mut readable = Vec::new();
    for (r, label) in [(a, label_a), (b, label_b)] {
        if !r.done || !r.read_anything() {
            problems.push(format!(
                "{label}: no result from the box (the check never finished or could not be read)"
            ));
            continue;
        }
        readable.push((r, label));
        if r.sealed == Some(true) {
            problems.push(format!("{label}: Vault is sealed"));
        }
        if r.initialized == Some(false) {
            problems.push(format!("{label}: Vault is not initialized"));
        }
        if r.overall_healthy != Some(true) {
            problems.push(format!("{label}: the health check did not report OVERALL: HEALTHY"));
        }
        if r.container_up != Some(true) {
            problems.push(format!("{label}: the vault container is not Up in docker ps"));
        }
    }
    if readable.len() == 2 {
        let (ra, rb) = (readable[0].0.role(), readable[1].0.role());
        for (r, label) in &readable {
            if r.role() == Role::Unknown {
                problems.push(format!("{label}: could not tell whether it is the leader or a follower"));
            }
        }
        if ra == Role::Leader && rb == Role::Leader {
            problems.push("both boxes claim to be the leader".to_string());
        } else if ra == Role::Follower && rb == Role::Follower {
            problems.push("no leader: both boxes are followers".to_string());
        }
        match (&readable[0].0.version, &readable[1].0.version) {
            (Some(x), Some(y)) if x != y => problems.push(format!(
                "version mismatch: {label_a} runs {x}, {label_b} runs {y}"
            )),
            (Some(_), Some(_)) => {}
            _ => {
                for (r, label) in &readable {
                    if r.version.is_none() {
                        problems.push(format!("{label}: version not reported"));
                    }
                }
            }
        }
    }
    Verdict { healthy: problems.is_empty(), problems }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PS_UP: &str = "CONTAINER ID   IMAGE                                                COMMAND                  CREATED        STATUS         PORTS     NAMES\nc4fb581a84a7   docker-proxy-hub.example.net/vault:1.13.3   \"docker-entrypoint.s…\"   15 hours ago   Up 15 hours              vault";
    const PS_DOWN: &str = "CONTAINER ID   IMAGE                                                COMMAND                  CREATED        STATUS                     PORTS     NAMES\nc4fb581a84a7   docker-proxy-hub.example.net/vault:1.13.3   \"docker-entrypoint.s…\"   15 hours ago   Exited (1) 2 minutes ago             vault";

    /// What a tab's text looks like after the steps ran: the echoed command
    /// lines (which contain the result words), then the real output.
    fn tab(standby: bool, is_self: bool, version: &str, sealed: bool, overall: &str, ps: &str, done: bool) -> String {
        let mut s = String::new();
        s.push_str("# sudo su\n# ");
        s.push_str(HEALTH_COMMAND);
        s.push('\n');
        s.push_str(&format!(
            "HEALTH\n{{\n  \"initialized\": true,\n  \"sealed\": {sealed},\n  \"standby\": {standby},\n  \"cluster_name\": \"vault-cluster-8b23ca83\",\n  \"cluster_id\": \"803bd9b7-abfc-dc42-f7e5-13a15ee34c7c\",\n  \"version\": \"{version}\"\n}}\n---\n"
        ));
        s.push_str(&format!(
            "LEADER\n{{\n  \"leader_address\": \"https://10.255.200.238:8200\",\n  \"is_self\": {is_self}\n}}\n---\n"
        ));
        s.push_str(&format!(
            "SEAL STATUS\n{{\n  \"initialized\": true,\n  \"sealed\": {sealed},\n  \"t\": 1,\n  \"n\": 1,\n  \"progress\": 0,\n  \"version\": \"{version}\"\n}}\n---\n"
        ));
        s.push_str(overall);
        s.push_str("\n# docker ps\n");
        s.push_str(ps);
        s.push_str("\n# m=DONE; echo \"__VAULT_HEALTH_${m}__\"\n");
        if done {
            s.push_str("__VAULT_HEALTH_DONE__\n");
        }
        s
    }

    fn leader() -> String {
        tab(false, true, "1.13.3", false, "OVERALL: HEALTHY", PS_UP, true)
    }
    fn follower() -> String {
        tab(true, false, "1.13.3", false, "OVERALL: HEALTHY", PS_UP, true)
    }
    fn judged(a: &str, b: &str) -> Verdict {
        judge(&parse_box(a), &parse_box(b), "Vault 1", "Vault 2")
    }

    #[test]
    fn healthy_leader_and_follower() {
        let v = judged(&leader(), &follower());
        assert!(v.healthy, "{:?}", v.problems);
        assert!(v.problems.is_empty());
        // Either order: nothing here is primary or secondary.
        assert!(judged(&follower(), &leader()).healthy);
    }

    #[test]
    fn roles_come_from_the_boxs_own_answer() {
        assert_eq!(parse_box(&leader()).role(), Role::Leader);
        assert_eq!(parse_box(&follower()).role(), Role::Follower);
    }

    #[test]
    fn command_echo_is_not_a_result() {
        // Only the echoed command lines: they contain OVERALL: HEALTHY and
        // the marker text, and neither may count.
        let echo = format!("# sudo su\n# {HEALTH_COMMAND}\n# docker ps\n# m=DONE; echo \"__VAULT_HEALTH_${{m}}__\"\n");
        assert!(!is_done(&echo));
        let b = parse_box(&echo);
        assert_eq!(b.overall_healthy, None);
        assert!(!b.done);
        let v = judge(&b, &parse_box(&follower()), "Vault 1", "Vault 2");
        assert!(!v.healthy);
        assert!(v.problems.iter().any(|p| p.contains("Vault 1") && p.contains("no result")), "{:?}", v.problems);
    }

    #[test]
    fn two_leaders() {
        let v = judged(&leader(), &leader());
        assert!(!v.healthy);
        assert!(v.problems.iter().any(|p| p.contains("both") && p.contains("leader")), "{:?}", v.problems);
    }

    #[test]
    fn no_leader_both_followers() {
        let v = judged(&follower(), &follower());
        assert!(!v.healthy);
        assert!(v.problems.iter().any(|p| p.contains("no leader")), "{:?}", v.problems);
    }

    #[test]
    fn version_mismatch_names_both() {
        let other = tab(true, false, "1.15.2", false, "OVERALL: HEALTHY", PS_UP, true);
        let v = judged(&leader(), &other);
        assert!(!v.healthy);
        assert!(
            v.problems.iter().any(|p| p.contains("1.13.3") && p.contains("1.15.2")),
            "{:?}",
            v.problems
        );
    }

    #[test]
    fn sealed_box() {
        let sealed = tab(true, false, "1.13.3", true, "OVERALL: UNHEALTHY", PS_UP, true);
        let v = judged(&leader(), &sealed);
        assert!(!v.healthy);
        assert!(v.problems.iter().any(|p| p.contains("Vault 2") && p.contains("sealed")), "{:?}", v.problems);
    }

    #[test]
    fn overall_unhealthy() {
        let bad = tab(true, false, "1.13.3", false, "OVERALL: UNHEALTHY", PS_UP, true);
        let v = judged(&leader(), &bad);
        assert!(!v.healthy);
        assert!(v.problems.iter().any(|p| p.contains("Vault 2") && p.contains("OVERALL")), "{:?}", v.problems);
    }

    #[test]
    fn container_missing_or_exited() {
        let down = tab(false, true, "1.13.3", false, "OVERALL: HEALTHY", PS_DOWN, true);
        assert_eq!(parse_box(&down).container_up, Some(false));
        let v = judged(&down, &follower());
        assert!(!v.healthy);
        assert!(v.problems.iter().any(|p| p.contains("Vault 1") && p.contains("container")), "{:?}", v.problems);

        let empty = tab(false, true, "1.13.3", false, "OVERALL: HEALTHY", "CONTAINER ID   IMAGE   COMMAND   CREATED   STATUS   PORTS   NAMES", true);
        assert_eq!(parse_box(&empty).container_up, Some(false));
        let no_table = tab(false, true, "1.13.3", false, "OVERALL: HEALTHY", "", true);
        assert_eq!(parse_box(&no_table).container_up, None);
        assert!(!judged(&no_table, &follower()).healthy);
    }

    #[test]
    fn a_container_merely_containing_vault_in_its_name_is_not_vault() {
        let other = "CONTAINER ID   IMAGE   COMMAND   CREATED   STATUS   PORTS   NAMES\nabc   img:1   \"x\"   1 hour ago   Up 1 hour       vault-agent";
        let t = tab(false, true, "1.13.3", false, "OVERALL: HEALTHY", other, true);
        assert_eq!(parse_box(&t).container_up, Some(false));
    }

    #[test]
    fn unreachable_box_is_a_problem_not_healthy() {
        let v = judged(&leader(), "");
        assert!(!v.healthy);
        assert!(v.problems.iter().any(|p| p.contains("Vault 2") && p.contains("no result")), "{:?}", v.problems);
        // A box that printed results but never reached the done marker.
        let cut = tab(true, false, "1.13.3", false, "OVERALL: HEALTHY", PS_UP, false);
        assert!(!judged(&leader(), &cut).healthy);
    }

    #[test]
    fn garbled_json_does_not_panic() {
        let junk = "HEALTH\n{\n  \"sealed\": tru\n}\n---\nLEADER\n{\nOVERALL: HEALTHY\n__VAULT_HEALTH_DONE__\n";
        let b = parse_box(junk);
        assert_eq!(b.sealed, None);
        let _ = judge(&b, &b, "Vault 1", "Vault 2");
        let _ = parse_box("");
        let _ = parse_box("HEALTH\n{");
    }

    #[test]
    fn the_steps_never_contain_the_literal_marker() {
        for s in steps() {
            assert!(!s.contains("__VAULT_HEALTH_DONE__"), "{s}");
        }
        assert!(steps().last().unwrap().contains("__VAULT_HEALTH_${"));
    }

    #[test]
    fn the_steps_are_read_only() {
        let all = steps().join("\n");
        for bad in [
            "restart", "unseal", "step-down", "docker rm", "docker stop", "docker kill",
            "-X POST", "-X PUT", "-X DELETE", "rm ", "tee",
        ] {
            assert!(!all.contains(bad), "step mentions {bad}");
        }
        assert!(!all.replace(">/dev/null 2>&1", "").contains('>'));
        assert_eq!(steps()[1], "docker ps");
        assert_eq!(steps()[0], HEALTH_COMMAND);
    }
}

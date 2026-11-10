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

/// Memory and disk use, read-only. Memory is used/total from `free` (using
/// "available", so cache does not count as used); disk is one `use% mount`
/// line per mounted filesystem, skipping in-memory and container overlay
/// mounts. Headings are exact lines, like the health command's.
pub const RESOURCE_COMMAND: &str = concat!(
    r#"echo "MEMORY"; free | awk '/^Mem:/ {printf "%d\n", ($2-$7)*100/$2}'; "#,
    r#"echo "DISK"; df -P -x tmpfs -x devtmpfs -x overlay -x squashfs -x efivarfs | tail -n +2 | awk '{print $5, $6}'; "#,
    r#"echo "END RESOURCES""#,
);

/// Keep only what is safe to put in a shell command and in a marker line.
fn clean(nonce: &str) -> String {
    nonce.chars().filter(char::is_ascii_alphanumeric).collect()
}

fn marker_line(kind: &str, nonce: &str) -> String {
    format!("__VAULT_HEALTH_{kind}_{}__", clean(nonce))
}

/// A step that prints a marker line. The marker is assembled by the shell at
/// run time: the shell echoes every command line before running it, so a
/// literal marker in the command would be matched off the echo and a tab
/// would read as started or finished before anything ran. Same trick as
/// `vault_iam::verdict_step`.
fn marker_step(kind: &str, nonce: &str) -> String {
    format!("n={}; echo \"__VAULT_HEALTH_{kind}_${{n}}__\"", clean(nonce))
}

/// The steps sent to each box after `sudo su` and the terminal prep.
///
/// `nonce` is unique to the run. The tab is reused between runs and holds
/// the previous run's output, so a run's results are only read from after
/// *its own* start marker: otherwise a re-run whose steps never executed
/// (the tab sits in `less`, or `sudo` is asking for a password) would read
/// the previous run's healthy output back.
pub fn steps(nonce: &str) -> Vec<String> {
    vec![
        marker_step("START", nonce),
        HEALTH_COMMAND.to_string(),
        RESOURCE_COMMAND.to_string(),
        "docker ps".to_string(),
        marker_step("DONE", nonce),
    ]
}

/// True once this run's done marker has been printed as output (a line that
/// is exactly the marker — the echoed command line never is).
pub fn is_done(screen: &str, nonce: &str) -> bool {
    let done = marker_line("DONE", nonce);
    screen.lines().any(|l| l.trim() == done)
}

/// The part of the tab after the last line that is exactly this run's start
/// marker; `None` when the run never started.
fn this_run<'a>(screen: &'a str, nonce: &str) -> Option<&'a str> {
    let start = marker_line("START", nonce);
    let mut offset = 0;
    let mut found = None;
    for line in screen.split_inclusive('\n') {
        offset += line.len();
        if line.trim() == start {
            found = Some(offset);
        }
    }
    found.map(|at| &screen[at..])
}

/// How full a resource is: 90% or more is red, 80% or more yellow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Ok,
    Yellow,
    Red,
}

pub const RED_PCT: u8 = 90;
pub const YELLOW_PCT: u8 = 80;

pub fn level(pct: u8) -> Level {
    if pct >= RED_PCT {
        Level::Red
    } else if pct >= YELLOW_PCT {
        Level::Yellow
    } else {
        Level::Ok
    }
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
    /// How long the vault container has been Up: seconds, and docker's words.
    pub uptime: Option<(u64, String)>,
    /// Memory in use, percent. `None` = not read.
    pub memory_pct: Option<u8>,
    /// `(mount, percent used)` for every mounted filesystem. Empty = not read.
    pub disks: Vec<(String, u8)>,
    pub done: bool,
}

impl BoxReport {
    /// The worst level among memory and every disk; `None` when neither was read.
    pub fn worst_level(&self) -> Option<Level> {
        self.memory_pct
            .map(level)
            .into_iter()
            .chain(self.disks.iter().map(|(_, p)| level(*p)))
            .max()
    }

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
///
/// The table is read as a stream of tokens, not line by line: a terminal that
/// is narrower than the ~138-column table (or a Windows pty that hard-breaks
/// at its own width) splits rows at arbitrary points. Rows are told apart by
/// their 12-hex container id, and a row is the vault one when its last token
/// is exactly `vault`.
pub fn vault_container_up(screen: &str) -> Option<bool> {
    vault_up_row(screen).map(|row| row.is_some())
}

/// A container younger than this is shown red in the report: it has just
/// (re)started, whatever else is true.
pub const FRESH_UPTIME_SECS: u64 = 120;

/// How long the vault container has been Up, in seconds, and as docker words
/// it (`33 hours`). `None` when there is no Up vault row or it can't be read.
pub fn vault_uptime(screen: &str) -> Option<(u64, String)> {
    let row = vault_up_row(screen)??;
    let i = row.iter().position(|t| t == "Up")?;
    let words: Vec<&str> = row[i + 1..]
        .iter()
        .map(String::as_str)
        .take_while(|t| !t.starts_with('('))
        .collect();
    let secs = parse_docker_uptime(&words)?;
    // Status is followed by PORTS / NAMES; keep only the duration words.
    let text: Vec<&str> = match words.iter().position(|w| {
        matches!(*w, "second" | "seconds" | "minute" | "minutes" | "hour" | "hours" | "day" | "days"
            | "week" | "weeks" | "month" | "months" | "year" | "years")
    }) {
        Some(u) => words[..=u].to_vec(),
        None => words.iter().take(4).copied().collect(),
    };
    Some((secs, text.join(" ")))
}

/// `Less than a second`, `5 seconds`, `About a minute`, `33 hours`, `3 days`...
/// The words after `Up`; trailing columns (ports, name) are ignored.
fn parse_docker_uptime(words: &[&str]) -> Option<u64> {
    let unit = |w: &str| -> Option<u64> {
        Some(match w {
            "second" | "seconds" => 1,
            "minute" | "minutes" => 60,
            "hour" | "hours" => 3600,
            "day" | "days" => 86_400,
            "week" | "weeks" => 604_800,
            "month" | "months" => 2_592_000,
            "year" | "years" => 31_536_000,
            _ => return None,
        })
    };
    let w: Vec<String> = words.iter().map(|w| w.to_ascii_lowercase()).collect();
    let w: Vec<&str> = w.iter().map(String::as_str).collect();
    match w.as_slice() {
        ["less", "than", ..] => Some(0),
        ["about", "a" | "an", u, ..] => unit(u),
        [n, u, ..] => Some(n.parse::<u64>().ok()? * unit(u)?),
        _ => None,
    }
}

/// `None` = no table; `Some(None)` = table but no Up vault row; `Some(Some(tokens))`
/// = the Up vault row, split into words.
fn vault_up_row(screen: &str) -> Option<Option<Vec<String>>> {
    let lines: Vec<&str> = screen.lines().collect();
    let header = lines.iter().rposition(|l| l.contains("CONTAINER ID"))?;
    let mut body: Vec<&str> = Vec::new();
    for row in lines.iter().skip(header + 1) {
        let row = row.trim();
        // Blank lines are skipped, not a stop: a hard break inside a run of
        // column padding leaves one. The prompt / marker is what ends it.
        if row.is_empty() {
            continue;
        }
        if row.starts_with('#')
            || row.starts_with("__VAULT_HEALTH_")
            || row.starts_with("root@")
            || row.contains(" SSM /")
        {
            break;
        }
        body.push(row);
    }
    // Joined with a space, and with nothing: a break that fell mid-word needs
    // the second, one that fell on a (trimmed) space needs the first.
    for sep in [" ", ""] {
        let joined = body.join(sep);
        let mut row: Vec<&str> = Vec::new();
        let mut rows: Vec<Vec<&str>> = Vec::new();
        for tok in joined.split_whitespace() {
            if tok.len() == 12 && tok.bytes().all(|b| b.is_ascii_hexdigit()) {
                rows.push(std::mem::take(&mut row));
            }
            row.push(tok);
        }
        rows.push(row);
        if let Some(r) = rows
            .iter()
            .find(|r| r.last() == Some(&"vault") && r.contains(&"Up"))
        {
            return Some(Some(r.iter().map(|t| t.to_string()).collect()));
        }
    }
    Some(None)
}

/// Memory and disk from the resource section of this run's output. The
/// headings are matched as whole lines, so the echoed command never counts;
/// only well-formed lines are taken, so a wrapped or garbled one is skipped
/// rather than guessed at.
fn parse_resources(screen: &str) -> (Option<u8>, Vec<(String, u8)>) {
    let lines: Vec<&str> = screen.lines().map(str::trim).collect();
    let Some(mem_at) = lines.iter().rposition(|l| *l == "MEMORY") else {
        return (None, Vec::new());
    };
    let pct = |t: &str| t.trim_end_matches('%').parse::<u8>().ok().filter(|p| *p <= 100);
    let memory = lines.get(mem_at + 1).and_then(|l| {
        if l.ends_with('%') { None } else { pct(l) }
    });
    let mut disks = Vec::new();
    if let Some(d) = lines.iter().skip(mem_at).position(|l| *l == "DISK") {
        for l in lines.iter().skip(mem_at + d + 1) {
            if *l == "END RESOURCES" {
                break;
            }
            if let Some((p, mount)) = l.split_once(' ') {
                if p.ends_with('%') && mount.starts_with('/') {
                    if let Some(p) = pct(p) {
                        disks.push((mount.trim().to_string(), p));
                    }
                }
            }
        }
    }
    (memory, disks)
}

/// Read one box's tab text, from this run's start marker onwards.
pub fn parse_box(screen: &str, nonce: &str) -> BoxReport {
    let Some(screen) = this_run(screen, nonce) else {
        return BoxReport::default();
    };
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
    let (memory_pct, disks) = parse_resources(screen);
    BoxReport {
        memory_pct,
        disks,
        initialized: json_bool(&health, "initialized").or_else(|| json_bool(&seal, "initialized")),
        sealed: json_bool(&health, "sealed").or_else(|| json_bool(&seal, "sealed")),
        standby: json_bool(&health, "standby"),
        is_self: json_bool(&leader, "is_self"),
        version: json_str(&health, "version").or_else(|| json_str(&seal, "version")),
        overall_healthy,
        container_up: vault_container_up(screen),
        uptime: vault_uptime(screen),
        done: is_done(screen, nonce),
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
        match r.memory_pct {
            None => problems.push(format!("{label}: memory use could not be read")),
            Some(p) if level(p) == Level::Red => {
                problems.push(format!("{label}: memory is at {p}% (90% or more)"))
            }
            Some(_) => {}
        }
        if r.disks.is_empty() {
            problems.push(format!("{label}: disk use could not be read"));
        }
        for (mount, p) in &r.disks {
            if level(*p) == Level::Red {
                problems.push(format!("{label}: disk {mount} is at {p}% (90% or more)"));
            }
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

/// What the load balancer says about the two boxes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetGroupResult {
    /// The target group holding both boxes, and each box's state in it
    /// (`healthy`, `unhealthy`, `draining`... as the API words it), in the
    /// same order as the two Vault boxes.
    Found { name: String, states: [String; 2] },
    /// Why it could not be checked.
    Failed(String),
}

/// Most target groups asked about while looking for the Vault one: the API
/// answers one group per call, so an account full of them must not mean a
/// call for each.
pub const MAX_TARGET_GROUP_CHECKS: usize = 60;

/// Find the target group that has both boxes registered and read their state.
/// Matched by instance id, not by name; groups with `vault` in the name are
/// asked first only to keep the number of calls down. `health` reads one
/// group's targets.
pub fn find_vault_target_group(
    ids: [&str; 2],
    mut groups: Vec<crate::elb::TargetGroup>,
    mut health: impl FnMut(&crate::elb::TargetGroup) -> Result<Vec<crate::elb::Target>, String>,
) -> TargetGroupResult {
    groups.retain(|g| g.target_type == "instance");
    groups.sort_by_key(|g| !g.name.to_ascii_lowercase().contains("vault"));
    let mut first_error: Option<String> = None;
    let mut checked = 0;
    for g in groups.iter().take(MAX_TARGET_GROUP_CHECKS) {
        checked += 1;
        match health(g) {
            Ok(targets) => {
                let state_of = |id: &str| {
                    targets.iter().find(|t| t.id == id).map(|t| t.state.clone())
                };
                if let (Some(a), Some(b)) = (state_of(ids[0]), state_of(ids[1])) {
                    return TargetGroupResult::Found { name: g.name.clone(), states: [a, b] };
                }
            }
            Err(e) => {
                first_error.get_or_insert(e);
            }
        }
    }
    let mut msg = format!(
        "no target group has both {} and {} registered (looked at {checked} group(s))",
        ids[0], ids[1]
    );
    if let Some(e) = first_error {
        msg.push_str(&format!("; the first error was: {e}"));
    }
    TargetGroupResult::Failed(msg)
}

/// The target-group rules: the leader is healthy there and the follower is
/// not. When the roles are not a clean leader + follower the minimum still
/// holds: one box healthy and one not. Returns the problems and a one-line
/// description for the report.
pub fn judge_target_group(
    tg: &TargetGroupResult,
    roles: [Role; 2],
    labels: [&str; 2],
) -> (Vec<String>, String) {
    let (name, states) = match tg {
        TargetGroupResult::Failed(e) => {
            return (
                vec![format!("target group could not be checked: {e}")],
                format!("Target group: not checked ({e})."),
            );
        }
        TargetGroupResult::Found { name, states } => (name, states),
    };
    let mut problems = Vec::new();
    let healthy = |i: usize| states[i] == "healthy";
    let clean = matches!(roles, [Role::Leader, Role::Follower] | [Role::Follower, Role::Leader]);
    if clean {
        for i in 0..2 {
            match roles[i] {
                Role::Leader if !healthy(i) => problems.push(format!(
                    "{}: it is the leader but is {} in target group {name} (the leader should be healthy)",
                    labels[i], states[i]
                )),
                Role::Follower if healthy(i) => problems.push(format!(
                    "{}: it is the follower but is healthy in target group {name} (the follower should not be)",
                    labels[i]
                )),
                _ => {}
            }
        }
    } else if healthy(0) == healthy(1) {
        problems.push(format!(
            "target group {name} should have one healthy and one unhealthy box, but {} is {} and {} is {}",
            labels[0], states[0], labels[1], states[1]
        ));
    }
    // The one-healthy-one-unhealthy split is what a good pair looks like, so
    // when the check passes the unhealthy box is marked as expected. Both
    // unhealthy is a problem above and is left unmarked.
    let shown = |i: usize| {
        if problems.is_empty() && states[i] == "unhealthy" {
            format!("{} (expected)", states[i])
        } else {
            states[i].clone()
        }
    };
    let line = format!(
        "Target group {name}: {} {}, {} {}.",
        labels[0], shown(0), labels[1], shown(1)
    );
    (problems, line)
}

/// How a report line is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// The healthy headline (drawn in a darker green).
    Good,
    Plain,
    /// 80% or more.
    Warn,
    /// A problem, or 90% or more.
    Bad,
}

/// Everything the result popup needs.
#[derive(Debug, Clone)]
pub struct Report {
    pub verdict: Verdict,
    /// The popup's message: the verdict in words.
    pub summary: String,
    /// The same text, line by line, each with the tone it is drawn in.
    pub lines: Vec<(String, Tone)>,
    /// The popup's details pane: the verdict, then each box's full output.
    pub details: String,
    /// A vault container has been Up for two minutes or less: the popup
    /// shows its message red even when the pair is healthy.
    pub fresh: bool,
}

/// One box's memory and disk, in words, and the tone of the worst of them.
fn resource_line(label: &str, r: &BoxReport) -> (String, Tone) {
    let mem = r
        .memory_pct
        .map(|p| format!("memory {p}%"))
        .unwrap_or_else(|| "memory not read".to_string());
    let disks = if r.disks.is_empty() {
        "disk not read".to_string()
    } else {
        let d: Vec<String> = r.disks.iter().map(|(m, p)| format!("{m} {p}%")).collect();
        format!("disk {}", d.join(", "))
    };
    let (tone, note) = match r.worst_level() {
        Some(Level::Red) => (Tone::Bad, " (CRITICAL: 90% or more)"),
        Some(Level::Yellow) => (Tone::Warn, " (warning: 80% or more)"),
        Some(Level::Ok) => (Tone::Plain, ""),
        None => (Tone::Bad, ""),
    };
    (format!("{label}: {mem}; {disks}{note}"), tone)
}

/// Judge the two tabs' text and render the report. `None` is a tab that never
/// opened or was closed; it is reported as a box with no result, never
/// skipped.
pub fn build_report(labels: [&str; 2], texts: [Option<&str>; 2], nonce: &str) -> Report {
    build_report_with_target_group(labels, texts, nonce, None)
}

/// As `build_report`, plus the load balancer's view when one was read (`None`
/// skips the check, for callers with no AWS access).
pub fn build_report_with_target_group(
    labels: [&str; 2],
    texts: [Option<&str>; 2],
    nonce: &str,
    tg: Option<&TargetGroupResult>,
) -> Report {
    let reports = [
        parse_box(texts[0].unwrap_or_default(), nonce),
        parse_box(texts[1].unwrap_or_default(), nonce),
    ];
    let mut verdict = judge(&reports[0], &reports[1], labels[0], labels[1]);
    let mut tg_problem = false;
    let tg_line = tg.map(|tg| {
        let (problems, line) =
            judge_target_group(tg, [reports[0].role(), reports[1].role()], labels);
        tg_problem = !problems.is_empty();
        verdict.problems.extend(problems);
        verdict.healthy = verdict.problems.is_empty();
        line
    });
    let tg_tone = if tg_problem { Tone::Bad } else { Tone::Plain };

    let fresh = reports
        .iter()
        .any(|r| r.uptime.as_ref().is_some_and(|(secs, _)| *secs <= FRESH_UPTIME_SECS));
    let mut lines: Vec<(String, Tone)> = Vec::new();
    if verdict.healthy {
        let (leader, follower) = if reports[0].role() == Role::Leader {
            (labels[0], labels[1])
        } else {
            (labels[1], labels[0])
        };
        // A container that has only just started turns the whole message red.
        let headline = if fresh { Tone::Bad } else { Tone::Good };
        lines.push((
            format!(
                "Vault is healthy: {leader} is the leader and {follower} is the follower, \
                 both on version {}, with the vault container up on both.",
                reports[0].version.as_deref().unwrap_or("?")
            ),
            headline,
        ));
        let ups: Vec<String> = labels
            .iter()
            .zip(reports.iter())
            .filter_map(|(l, r)| r.uptime.as_ref().map(|(_, t)| format!("{l} {t}")))
            .collect();
        if !ups.is_empty() {
            lines.push((format!("Docker uptime: {}.", ups.join(", ")), Tone::Plain));
        }
        for (l, r) in labels.iter().zip(reports.iter()) {
            lines.push(resource_line(l, r));
        }
        if let Some(line) = &tg_line {
            lines.push((line.clone(), tg_tone));
        }
        if fresh {
            lines.push(("A vault container started two minutes ago or less.".to_string(), Tone::Bad));
        }
    } else {
        lines.push(("Vault is NOT healthy:".to_string(), Tone::Bad));
        for p in &verdict.problems {
            lines.push((format!(" - {p}"), Tone::Bad));
        }
        for (l, r) in labels.iter().zip(reports.iter()) {
            lines.push(resource_line(l, r));
        }
        if let Some(line) = &tg_line {
            lines.push((line.clone(), tg_tone));
        }
    }
    let summary = lines.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>().join("\n");

    let mut details = summary.clone();
    for (label, text) in labels.iter().zip(texts.iter()) {
        details.push_str(&format!("\n\n===== {label} =====\n"));
        match text.map(str::trim_end).filter(|t| !t.trim().is_empty()) {
            Some(t) => details.push_str(t),
            None => details.push_str("(no output)"),
        }
    }
    Report { verdict, summary, lines, details, fresh }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONCE: &str = "c0ffee01";

    const PS_UP: &str = "CONTAINER ID   IMAGE                                                COMMAND                  CREATED        STATUS         PORTS     NAMES\nc4fb581a84a7   docker-proxy-hub.example.net/vault:1.13.3   \"docker-entrypoint.s…\"   15 hours ago   Up 15 hours              vault";
    const PS_DOWN: &str = "CONTAINER ID   IMAGE                                                COMMAND                  CREATED        STATUS                     PORTS     NAMES\nc4fb581a84a7   docker-proxy-hub.example.net/vault:1.13.3   \"docker-entrypoint.s…\"   15 hours ago   Exited (1) 2 minutes ago             vault";

    /// What a tab's text looks like after the steps ran: the echoed command
    /// lines (which contain the result words), then the real output.
    fn tab(standby: bool, is_self: bool, version: &str, sealed: bool, overall: &str, ps: &str, done: bool) -> String {
        let (mem, disk) = ("40", "30% /\n55% /data");
        let mut s = String::new();
        s.push_str("# sudo su\n# ");
        s.push_str(&format!("# n={NONCE}; echo \"__VAULT_HEALTH_START_${{n}}__\"\n__VAULT_HEALTH_START_{NONCE}__\n# "));
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
        s.push_str(&format!("\n# {RESOURCE_COMMAND}\nMEMORY\n{mem}\nDISK\n{disk}\nEND RESOURCES\n"));
        s.push_str("# docker ps\n");
        s.push_str(ps);
        s.push_str("\n# m=DONE; echo \"__VAULT_HEALTH_${m}__\"\n");
        if done {
            s.push_str(&format!("__VAULT_HEALTH_DONE_{NONCE}__\n"));
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
        judge(&parse_box(a, NONCE), &parse_box(b, NONCE), "Vault 1", "Vault 2")
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
        assert_eq!(parse_box(&leader(), NONCE).role(), Role::Leader);
        assert_eq!(parse_box(&follower(), NONCE).role(), Role::Follower);
    }

    #[test]
    fn command_echo_is_not_a_result() {
        // Only the echoed command lines: they contain OVERALL: HEALTHY and
        // the marker text, and neither may count.
        let echo = format!("# sudo su\n# {HEALTH_COMMAND}\n# docker ps\n# m=DONE; echo \"__VAULT_HEALTH_${{m}}__\"\n");
        assert!(!is_done(&echo, NONCE));
        let b = parse_box(&echo, NONCE);
        assert_eq!(b.overall_healthy, None);
        assert!(!b.done);
        let v = judge(&b, &parse_box(&follower(), NONCE), "Vault 1", "Vault 2");
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
        assert_eq!(parse_box(&down, NONCE).container_up, Some(false));
        let v = judged(&down, &follower());
        assert!(!v.healthy);
        assert!(v.problems.iter().any(|p| p.contains("Vault 1") && p.contains("container")), "{:?}", v.problems);

        let empty = tab(false, true, "1.13.3", false, "OVERALL: HEALTHY", "CONTAINER ID   IMAGE   COMMAND   CREATED   STATUS   PORTS   NAMES", true);
        assert_eq!(parse_box(&empty, NONCE).container_up, Some(false));
        let no_table = tab(false, true, "1.13.3", false, "OVERALL: HEALTHY", "", true);
        assert_eq!(parse_box(&no_table, NONCE).container_up, None);
        assert!(!judged(&no_table, &follower()).healthy);
    }

    #[test]
    fn a_container_merely_containing_vault_in_its_name_is_not_vault() {
        let other = "CONTAINER ID   IMAGE   COMMAND   CREATED   STATUS   PORTS   NAMES\nabc   img:1   \"x\"   1 hour ago   Up 1 hour       vault-agent";
        let t = tab(false, true, "1.13.3", false, "OVERALL: HEALTHY", other, true);
        assert_eq!(parse_box(&t, NONCE).container_up, Some(false));
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
        let b = parse_box(junk, NONCE);
        assert_eq!(b.sealed, None);
        let _ = judge(&b, &b, "Vault 1", "Vault 2");
        let _ = parse_box("", NONCE);
        let _ = parse_box("HEALTH\n{", NONCE);
    }

    #[test]
    fn the_steps_never_contain_the_literal_marker() {
        for s in steps(NONCE) {
            assert!(!s.contains("__VAULT_HEALTH_DONE_") || s.contains("${n}"), "{s}");
            assert!(!s.contains(&format!("__VAULT_HEALTH_DONE_{NONCE}__")), "{s}");
            assert!(!s.contains(&format!("__VAULT_HEALTH_START_{NONCE}__")), "{s}");
        }
        assert!(steps(NONCE).last().unwrap().contains("__VAULT_HEALTH_DONE_${"));
    }

    #[test]
    fn the_steps_are_read_only() {
        let all = steps(NONCE).join("\n");
        for bad in [
            "restart", "unseal", "step-down", "docker rm", "docker stop", "docker kill",
            "-X POST", "-X PUT", "-X DELETE", "rm ", "tee",
        ] {
            assert!(!all.contains(bad), "step mentions {bad}");
        }
        assert!(!all.replace(">/dev/null 2>&1", "").contains('>'));
        assert_eq!(steps(NONCE)[3], "docker ps");
        assert_eq!(steps(NONCE)[2], RESOURCE_COMMAND);
        assert_eq!(steps(NONCE)[1], HEALTH_COMMAND);
    }

    #[test]
    fn report_for_a_healthy_pair() {
        let (l, f) = (leader(), follower());
        let r = build_report(["Vault 1 (vault-a)", "Vault 2 (vault-b)"], [Some(&l), Some(&f)], NONCE);
        assert!(r.verdict.healthy, "{:?}", r.verdict.problems);
        assert!(r.summary.contains("healthy"));
        assert!(r.summary.contains("Vault 1 (vault-a) is the leader"), "{}", r.summary);
        assert!(r.summary.contains("Vault 2 (vault-b) is the follower"), "{}", r.summary);
        assert!(r.summary.contains("1.13.3"));
    }

    #[test]
    fn report_details_carry_both_raw_outputs_labelled() {
        let (l, f) = (leader(), follower());
        let r = build_report(["Vault 1", "Vault 2"], [Some(&l), Some(&f)], NONCE);
        let at_1 = r.details.find("Vault 1").expect("first label");
        let at_2 = r.details.rfind("Vault 2").expect("second label");
        assert!(at_1 < at_2);
        assert!(r.details.contains("\"is_self\": true"));
        assert!(r.details.contains("\"is_self\": false"));
        assert!(r.details.contains("OVERALL: HEALTHY"));
    }

    #[test]
    fn a_missing_tab_is_reported_not_skipped() {
        let l = leader();
        let r = build_report(["Vault 1", "Vault 2"], [Some(&l), None], NONCE);
        assert!(!r.verdict.healthy);
        assert!(r.summary.contains("Vault 2"), "{}", r.summary);
        assert!(r.details.contains("(no output)"), "{}", r.details);
    }

    #[test]
    fn an_unhealthy_summary_lists_every_problem() {
        let (a, b) = (leader(), leader());
        let r = build_report(["Vault 1", "Vault 2"], [Some(&a), Some(&b)], NONCE);
        assert!(!r.verdict.healthy);
        assert!(r.summary.contains("both boxes claim to be the leader"), "{}", r.summary);
    }

    #[test]
    fn an_earlier_run_on_a_reused_tab_is_never_read() {
        // Run 1 was healthy. Run 2's steps were typed into `less`, so all it
        // left on the tab is its own start marker (a different nonce) and
        // nothing else. The old results must not count for it.
        let old = leader();
        let second = "# n=beef0002; echo \"__VAULT_HEALTH_START_${n}__\"\n__VAULT_HEALTH_START_beef0002__\n";
        let screen = format!("{old}{second}");
        let b = parse_box(&screen, "beef0002");
        assert_eq!(b, BoxReport::default());
        assert!(!judge(&b, &parse_box(&follower(), NONCE), "Vault 1", "Vault 2").healthy);
        // And the old run's own nonce still reads it.
        assert!(parse_box(&screen, NONCE).done);
    }

    #[test]
    fn a_box_that_never_printed_the_start_marker_is_unread() {
        let screen = leader().replace(&format!("__VAULT_HEALTH_START_{NONCE}__\n"), "");
        assert_eq!(parse_box(&screen, NONCE), BoxReport::default());
    }

    #[test]
    fn the_done_marker_must_carry_this_runs_nonce() {
        let screen = leader().replace(NONCE, "0ther");
        // Start marker and done marker both carry another nonce.
        assert!(!is_done(&screen, NONCE));
    }

    #[test]
    fn docker_uptime_words_become_seconds() {
        let p = |s: &str| parse_docker_uptime(&s.split_whitespace().collect::<Vec<_>>());
        assert_eq!(p("Less than a second"), Some(0));
        assert_eq!(p("5 seconds"), Some(5));
        assert_eq!(p("About a minute"), Some(60));
        assert_eq!(p("2 minutes"), Some(120));
        assert_eq!(p("About an hour"), Some(3600));
        assert_eq!(p("33 hours"), Some(33 * 3600));
        assert_eq!(p("3 days"), Some(3 * 86_400));
        assert_eq!(p("what"), None);
    }

    fn table(status: &str) -> String {
        format!("# docker ps\nCONTAINER ID   IMAGE   COMMAND   CREATED   STATUS   PORTS   NAMES\n6de5e73f2538   img:1.13.3   \"x\"   5 days ago   Up {status}              vault\n\nroot@ip SSM /home\n")
    }

    #[test]
    fn uptime_is_read_from_the_vault_row() {
        assert_eq!(vault_uptime(&table("33 hours")), Some((33 * 3600, "33 hours".to_string())));
        assert_eq!(vault_uptime(&table("About a minute (healthy)")).map(|u| u.0), Some(60));
        assert_eq!(vault_uptime("no table"), None);
    }

    #[test]
    fn a_healthy_report_shows_each_boxs_uptime_and_flags_a_fresh_start() {
        let (l, f) = (leader(), follower());
        let r = build_report(["Vault 1", "Vault 2"], [Some(&l), Some(&f)], NONCE);
        assert!(r.summary.contains("Docker uptime: Vault 1 "), "{}", r.summary);
        assert!(r.summary.contains(", Vault 2 "), "{}", r.summary);
        assert!(!r.fresh, "{}", r.summary);
        // Same pair, but Vault 2's container came up 90 seconds ago.
        let young = f.replace("Up 15 hours", "Up About a minute");
        assert_ne!(young, f, "fixture should carry 'Up 15 hours'");
        let r = build_report(["Vault 1", "Vault 2"], [Some(&l), Some(&young)], NONCE);
        assert!(r.verdict.healthy);
        assert!(r.fresh, "{}", r.summary);
        // Exactly two minutes counts as fresh; a little over does not.
        let two = f.replace("Up 15 hours", "Up 2 minutes");
        assert!(build_report(["a", "b"], [Some(&l), Some(&two)], NONCE).fresh);
        let three = f.replace("Up 15 hours", "Up 3 minutes");
        assert!(!build_report(["a", "b"], [Some(&l), Some(&three)], NONCE).fresh);
    }

    fn tg(name: &str, ty: &str) -> crate::elb::TargetGroup {
        crate::elb::TargetGroup { name: name.to_string(), arn: format!("arn:{name}"), target_type: ty.to_string(), ..Default::default() }
    }
    fn tgt(id: &str, state: &str) -> crate::elb::Target {
        crate::elb::Target { id: id.to_string(), state: state.to_string(), ..Default::default() }
    }

    #[test]
    fn the_target_group_is_found_by_instance_id_not_by_name() {
        let groups = vec![tg("web", "instance"), tg("odd-name", "instance"), tg("lam", "lambda")];
        let r = find_vault_target_group(["i-a", "i-b"], groups, |g| match g.name.as_str() {
            "web" => Ok(vec![tgt("i-x", "healthy")]),
            "odd-name" => Ok(vec![tgt("i-a", "healthy"), tgt("i-b", "unhealthy")]),
            other => panic!("asked about {other}"),
        });
        assert_eq!(r, TargetGroupResult::Found { name: "odd-name".into(), states: ["healthy".into(), "unhealthy".into()] });
    }

    #[test]
    fn vault_named_groups_are_asked_first_and_a_miss_is_a_failure_with_the_reason() {
        let mut asked = Vec::new();
        let r = find_vault_target_group(["i-a", "i-b"], vec![tg("web", "instance"), tg("prod-Vault-tg", "instance")], |g| {
            asked.push(g.name.clone());
            Err("AccessDenied".to_string())
        });
        assert_eq!(asked[0], "prod-Vault-tg");
        match r {
            TargetGroupResult::Failed(m) => assert!(m.contains("i-a") && m.contains("AccessDenied"), "{m}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_group_with_only_one_of_the_boxes_is_not_the_vault_group() {
        let r = find_vault_target_group(["i-a", "i-b"], vec![tg("g", "instance")], |_| Ok(vec![tgt("i-a", "healthy")]));
        assert!(matches!(r, TargetGroupResult::Failed(_)));
    }

    fn found(a: &str, b: &str) -> TargetGroupResult {
        TargetGroupResult::Found { name: "vault-tg".into(), states: [a.into(), b.into()] }
    }

    #[test]
    fn the_leader_must_be_healthy_and_the_follower_not() {
        let l = ["Vault 1", "Vault 2"];
        let (p, _) = judge_target_group(&found("healthy", "unhealthy"), [Role::Leader, Role::Follower], l);
        assert!(p.is_empty(), "{p:?}");
        // Roles swapped: still fine when the states follow the roles.
        let (p, _) = judge_target_group(&found("unhealthy", "healthy"), [Role::Follower, Role::Leader], l);
        assert!(p.is_empty(), "{p:?}");
        // Both healthy: the follower should not be.
        let (p, _) = judge_target_group(&found("healthy", "healthy"), [Role::Leader, Role::Follower], l);
        assert_eq!(p.len(), 1);
        assert!(p[0].contains("Vault 2") && p[0].contains("follower"), "{p:?}");
        // Leader unhealthy: wrong.
        let (p, _) = judge_target_group(&found("unhealthy", "unhealthy"), [Role::Leader, Role::Follower], l);
        assert!(p.iter().any(|x| x.contains("Vault 1") && x.contains("leader")), "{p:?}");
        // The LB pointing at the follower instead of the leader: both wrong.
        let (p, _) = judge_target_group(&found("unhealthy", "healthy"), [Role::Leader, Role::Follower], l);
        assert_eq!(p.len(), 2, "{p:?}");
    }

    #[test]
    fn without_a_clean_leader_pair_one_healthy_one_not_is_the_minimum() {
        let l = ["Vault 1", "Vault 2"];
        let (p, _) = judge_target_group(&found("healthy", "unhealthy"), [Role::Unknown, Role::Unknown], l);
        assert!(p.is_empty(), "{p:?}");
        let (p, _) = judge_target_group(&found("healthy", "healthy"), [Role::Unknown, Role::Unknown], l);
        assert_eq!(p.len(), 1);
        let (p, _) = judge_target_group(&found("unhealthy", "draining"), [Role::Leader, Role::Leader], l);
        assert_eq!(p.len(), 1);
    }

    #[test]
    fn an_unreadable_target_group_is_a_problem_not_a_pass() {
        let (p, _) = judge_target_group(&TargetGroupResult::Failed("denied".into()), [Role::Leader, Role::Follower], ["a", "b"]);
        assert!(p[0].contains("denied"));
    }

    #[test]
    fn the_report_fails_a_healthy_pair_whose_target_group_is_wrong_and_names_it() {
        let (l, f) = (leader(), follower());
        let ok = build_report_with_target_group(["Vault 1", "Vault 2"], [Some(&l), Some(&f)], NONCE, Some(&found("healthy", "unhealthy")));
        assert!(ok.verdict.healthy, "{:?}", ok.verdict.problems);
        assert!(ok.summary.contains("Target group vault-tg"), "{}", ok.summary);
        let bad = build_report_with_target_group(["Vault 1", "Vault 2"], [Some(&l), Some(&f)], NONCE, Some(&found("healthy", "healthy")));
        assert!(!bad.verdict.healthy);
        assert!(bad.summary.contains("the follower should not be"), "{}", bad.summary);
        assert!(bad.summary.contains("Target group vault-tg"), "{}", bad.summary);
        // Not asked (no AWS access): the Vault checks decide alone.
        assert!(build_report(["Vault 1", "Vault 2"], [Some(&l), Some(&f)], NONCE).verdict.healthy);
    }

    /// A healthy tab with its memory and disk numbers swapped.
    fn with_resources(tab: &str, mem: &str, disk: &str) -> String {
        tab.replace("MEMORY\n40\nDISK\n30% /\n55% /data\n", &format!("MEMORY\n{mem}\nDISK\n{disk}\n"))
    }

    #[test]
    fn levels_use_90_red_and_80_yellow() {
        assert_eq!(level(79), Level::Ok);
        assert_eq!(level(80), Level::Yellow);
        assert_eq!(level(89), Level::Yellow);
        assert_eq!(level(90), Level::Red);
        assert_eq!(level(100), Level::Red);
    }

    #[test]
    fn memory_and_every_mounted_disk_are_read() {
        let b = parse_box(&with_resources(&leader(), "37", "12% /\n81% /data\n5% /efs"), NONCE);
        assert_eq!(b.memory_pct, Some(37));
        assert_eq!(
            b.disks,
            vec![("/".to_string(), 12), ("/data".to_string(), 81), ("/efs".to_string(), 5)]
        );
        assert_eq!(b.worst_level(), Some(Level::Yellow));
    }

    #[test]
    fn the_echoed_resource_command_is_not_a_reading() {
        let echo = format!("__VAULT_HEALTH_START_{NONCE}__\n# {RESOURCE_COMMAND}\n");
        let b = parse_box(&echo, NONCE);
        assert_eq!(b.memory_pct, None);
        assert!(b.disks.is_empty());
    }

    #[test]
    fn red_memory_or_disk_makes_the_pair_unhealthy_and_yellow_does_not() {
        let ok = judged(&leader(), &follower());
        assert!(ok.healthy, "{:?}", ok.problems);
        let yellow = judged(&with_resources(&leader(), "85", "30% /\n85% /data"), &follower());
        assert!(yellow.healthy, "{:?}", yellow.problems);
        let red_mem = judged(&with_resources(&leader(), "90", "30% /"), &follower());
        assert!(!red_mem.healthy);
        assert!(red_mem.problems.iter().any(|p| p.contains("memory is at 90%")));
        let red_disk = judged(&leader(), &with_resources(&follower(), "40", "30% /\n95% /data"));
        assert!(red_disk.problems.iter().any(|p| p.contains("Vault 2") && p.contains("/data") && p.contains("95%")));
    }

    #[test]
    fn an_unread_memory_or_disk_is_a_problem() {
        let none = leader().replace("MEMORY\n40\nDISK\n30% /\n55% /data\n", "");
        let v = judged(&none, &follower());
        assert!(v.problems.iter().any(|p| p.contains("memory use could not be read")));
        assert!(v.problems.iter().any(|p| p.contains("disk use could not be read")));
    }

    fn tg_found(a: &str, b: &str) -> TargetGroupResult {
        TargetGroupResult::Found { name: "vault-tg".into(), states: [a.into(), b.into()] }
    }

    #[test]
    fn the_unhealthy_box_in_a_good_pair_is_marked_expected() {
        let (problems, line) = judge_target_group(
            &tg_found("healthy", "unhealthy"),
            [Role::Leader, Role::Follower],
            ["Vault 1", "Vault 2"],
        );
        assert!(problems.is_empty());
        assert!(line.contains("Vault 1 healthy"), "{line}");
        assert!(line.contains("Vault 2 unhealthy (expected)"), "{line}");
    }

    #[test]
    fn both_unhealthy_is_a_problem_and_is_not_marked_expected() {
        let (problems, line) = judge_target_group(
            &tg_found("unhealthy", "unhealthy"),
            [Role::Leader, Role::Follower],
            ["Vault 1", "Vault 2"],
        );
        assert!(!problems.is_empty());
        assert!(!line.contains("expected"), "{line}");
    }

    #[test]
    fn the_report_tones_the_headline_and_the_resource_lines() {
        let labels = ["Vault 1", "Vault 2"];
        let good = leader();
        let fol = with_resources(&follower(), "85", "30% /");
        let r = build_report_with_target_group(
            labels,
            [Some(&good), Some(&fol)],
            NONCE,
            Some(&tg_found("healthy", "unhealthy")),
        );
        assert!(r.verdict.healthy, "{:?}", r.verdict.problems);
        assert_eq!(r.lines[0].1, Tone::Good);
        assert!(r.lines.iter().any(|(t, tone)| t.starts_with("Vault 2: memory 85%") && *tone == Tone::Warn));
        assert!(r.lines.iter().any(|(t, _)| t.contains("Vault 2 unhealthy (expected)")));
        let red = with_resources(&follower(), "95", "30% /");
        let r = build_report_with_target_group(labels, [Some(&good), Some(&red)], NONCE, None);
        assert!(!r.verdict.healthy);
        assert_eq!(r.lines[0].1, Tone::Bad);
        assert!(r.lines.iter().any(|(t, tone)| t.starts_with("Vault 2: memory 95%") && *tone == Tone::Bad));
    }
}

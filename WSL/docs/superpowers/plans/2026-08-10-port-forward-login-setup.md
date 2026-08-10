# Port Forwards login setup and test — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the Port Forwards window set an environment's bastion, login user and pem, and test that login by spawning the identical `ssh -N` session the tunnel itself spawns.

**Architecture:** The branch of `start_port_tunnel` that resolves a bastion and writes the managed ssh block is extracted into `resolve_tunnel_launch`, so the per-row `Login…` dialog's Test button and the real tunnel start share one definition of "can this connect". The test spawns via `Tunnel::spawn` with the real `-L` forwards and is watched frame-by-frame with the existing non-blocking `try_wait`; a session that survives its deadline becomes the live tunnel rather than being discarded.

**Tech Stack:** Rust 2021, egui/eframe (immediate mode), `rfd` for file picking, `portable-pty` (unrelated to this work). All GUI code is behind `--features gui` in `src/bin/ec2_manager_gui.rs`.

**Spec:** `docs/superpowers/specs/2026-08-10-port-forward-login-setup-design.md`

## Global Constraints

- Every build and test command uses `--features gui`. The GUI binary does not compile without it.
- Build to the Linux filesystem to avoid the `D:` disk-space failure documented in `CLAUDE.md`: prefix commands with `CARGO_TARGET_DIR=/tmp/ec2m-test`. A full `D:` surfaces as `error: failed to build archive … Input/output error (os error 5)`, not a disk message.
- The five failure strings produced by `start_port_tunnel` must not be reworded. `render_port_forwards_window` matches on `why.contains("needs authorizing")`, and `AppConfig::tunnel_error_dismissed` keys on the exact string — a reworded message silently un-dismisses every error a user has cleared.
- `ScriptState` has exactly two variants, `Running` and `Failed` (`src/bin/ec2_manager_gui.rs:805-810`). Do not add a third; Task 7 changes wording only.
- New GUI code must be warning-free. `cargo clippy --features gui` currently emits 21 pre-existing style warnings; do not add to them.
- Test names in this file are descriptive sentences, matching the existing convention (`pem_row_labels_expand_only_ambiguous_names`).
- Commit after every task. Branch is `brandons_changes`; do not commit to `master`.

## File Structure

| File | Responsibility | Change |
|------|----------------|--------|
| `src/config.rs` | Persistence of the three login values under the keys VS Code and the Scripts dialogs already share | Modify — add `set_port_forward_login` + tests |
| `src/bin/ec2_manager_gui.rs` | Everything else: the extracted `resolve_tunnel_launch`, the dialog, the watched test, the toolbar wording | Modify |
| `CLAUDE.md` (repo root and `WSL/`) | Architecture notes under "Background port-forward tunnels" | Modify |

No new files. This codebase keeps the whole GUI in one binary file and puts its testable helpers in free functions near the bottom (`pem_row_labels`, `retain_available_bastion`); follow that rather than introducing a module.

---

### Task 1: `classify_tunnel_failure`

A pure function turning an ssh stderr blob into a one-line hint. Independent of everything else, so it goes first.

**Files:**
- Modify: `src/bin/ec2_manager_gui.rs` — add the function beside `retain_available_bastion` (~line 19281), tests in the `mod tests` block at ~line 20460

**Interfaces:**
- Consumes: nothing
- Produces: `fn classify_tunnel_failure(stderr: &str) -> Option<&'static str>`

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/bin/ec2_manager_gui.rs`:

```rust
/// The three failures a user actually hits, each with a hint that names
/// the field they need to change. The raw stderr is always shown too, so
/// the hint only has to point — it does not have to explain.
#[test]
fn classify_tunnel_failure_names_the_usual_ssh_rejections() {
    assert_eq!(
        classify_tunnel_failure("bconrad@10.1.2.3: Permission denied (publickey)."),
        Some("the key or the login user is wrong"),
    );
    assert_eq!(
        classify_tunnel_failure(
            "bind [127.0.0.1]:5432: Address already in use\r\n\
             channel_setup_fwd_listener_tcpip: cannot listen to port: 5432"
        ),
        Some("another process is already holding that local port"),
    );
    assert_eq!(
        classify_tunnel_failure("ssh: Could not resolve hostname i-0785b0: Name or service not known"),
        Some("the bastion could not be reached"),
    );
}

/// An unrecognised failure returns None rather than a guess. The caller
/// shows stderr verbatim either way, and a confident wrong hint about an
/// unfamiliar error is worse than no hint at all.
#[test]
fn classify_tunnel_failure_returns_none_for_an_unknown_error() {
    assert_eq!(classify_tunnel_failure("kex_exchange_identification: read: Connection reset"), None);
    assert_eq!(classify_tunnel_failure(""), None);
}

/// Matching is case-insensitive: the wording of these messages varies
/// between OpenSSH builds and the Windows client.
#[test]
fn classify_tunnel_failure_ignores_case() {
    assert_eq!(
        classify_tunnel_failure("PERMISSION DENIED (PUBLICKEY)"),
        Some("the key or the login user is wrong"),
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_TARGET_DIR=/tmp/ec2m-test cargo test --features gui classify_tunnel_failure`
Expected: FAIL — `cannot find function 'classify_tunnel_failure' in this scope`

- [ ] **Step 3: Write the implementation**

Add beside `retain_available_bastion` in `src/bin/ec2_manager_gui.rs`:

```rust
/// A one-line hint for an ssh failure, or `None` when the message is not
/// one we recognise.
///
/// The caller always prints stderr verbatim underneath this; the hint
/// annotates it and never replaces it. That is why an unfamiliar error
/// returns `None` instead of a best guess — a wrong hint sends the user to
/// change the wrong field, which is worse than reading raw ssh output.
fn classify_tunnel_failure(stderr: &str) -> Option<&'static str> {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("permission denied") {
        Some("the key or the login user is wrong")
    } else if lower.contains("address already in use")
        || lower.contains("cannot listen to port")
    {
        Some("another process is already holding that local port")
    } else if lower.contains("could not resolve")
        || lower.contains("connection refused")
        || lower.contains("targetnotconnected")
        || lower.contains("no such instance")
    {
        Some("the bastion could not be reached")
    } else {
        None
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_TARGET_DIR=/tmp/ec2m-test cargo test --features gui classify_tunnel_failure`
Expected: PASS, 3 tests

- [ ] **Step 5: Commit**

```bash
git add src/bin/ec2_manager_gui.rs
git commit -m "Add classify_tunnel_failure for ssh stderr hints"
```

---

### Task 2: Extract `resolve_tunnel_launch` from `start_port_tunnel`

A refactor with two deliberate ordering changes, both stated below. No new behaviour — the point is that Task 6's test cannot pass on a configuration the real start would reject.

**Files:**
- Modify: `src/bin/ec2_manager_gui.rs:6456-6570` (`start_port_tunnel`)

**Interfaces:**
- Consumes: `PortForwardRow` (`src/bin/ec2_manager_gui.rs:509`), `ec2_manager::tunnel::Tunnel::spawn`
- Produces:
  - `struct TunnelLaunch { alias: String, forwards: Vec<ec2_manager::forwards::ResolvedForward>, signature: String }`
  - `fn resolve_tunnel_launch(&self, row: &PortForwardRow) -> std::result::Result<TunnelLaunch, String>`

**Two ordering changes, both intentional — call them out in the commit message:**

1. The "already running with a matching signature" early return now happens *before* the empty-forwards and auth guards. A healthy tunnel no longer has an error string written against it because its account's credentials lapsed.
2. A stale tunnel is removed *after* a successful resolve, not before. Previously a resolve failure killed the running session and started nothing; now a working tunnel survives a failed reconfigure.

- [ ] **Step 1: Add the `TunnelLaunch` struct**

Add next to `PortForwardRow` (~line 523) in `src/bin/ec2_manager_gui.rs`:

```rust
/// Everything needed to spawn one environment's tunnel, once the bastion,
/// login and key have been resolved and the managed ssh block written.
///
/// Produced by `resolve_tunnel_launch` and consumed by both the real start
/// and the Login dialog's test, so the test cannot succeed on a
/// configuration the real start would refuse.
struct TunnelLaunch {
    /// Host alias in the managed `config.d/ec2-manager` block.
    alias: String,
    forwards: Vec<ec2_manager::forwards::ResolvedForward>,
    signature: String,
}
```

- [ ] **Step 2: Add `resolve_tunnel_launch`**

Insert immediately above `start_port_tunnel` in `src/bin/ec2_manager_gui.rs`. The body is the existing code from `start_port_tunnel`, with each `self.tunnel_errors.insert(...); return;` replaced by `return Err(...)`:

```rust
/// Resolve everything one environment's tunnel needs and write its
/// managed ssh block, without spawning anything.
///
/// Shared by `start_port_tunnel` and the Login dialog's Test button. The
/// error strings are the ones the Port Forwards window has always shown
/// and are matched on elsewhere — see the note on `tunnel_error_dismissed`.
fn resolve_tunnel_launch(
    &self,
    row: &PortForwardRow,
) -> std::result::Result<TunnelLaunch, String> {
    if row.forwards.is_empty() {
        return Err("no forwards for this environment".into());
    }
    // An unauthenticated account cannot open a session, and trying
    // produces a hidden process that dies with a credentials error
    // nobody sees. Wait for the account to be authorized instead —
    // `poll_port_tunnels` starts it the moment it is.
    if self.script_env_auth(&row.account_id) != AuthStatus::Ok {
        let name = self.profile_display_name(&row.account_id);
        return Err(format!("{name} needs authorizing — forwards start once it is"));
    }
    let Some(bastion) = row.bastion.clone() else {
        return Err("no bastion saved for this environment — pick one in a \
                    Scripts dialog first"
            .into());
    };
    let Some((instance_name, profile, region)) =
        self.bastion_connection_details(&row.account_id, &bastion)
    else {
        return Err(format!("{bastion} is not in the loaded inventory — refresh"));
    };
    let user = self.config.resolve_ssh_user(&row.account_id, &row.env);
    let Some(pem) = self.config.resolve_pem(&row.account_id, &row.env, &bastion) else {
        return Err("no pem saved for this environment — open a box in VS Code \
                    once to choose one"
            .into());
    };

    // The block carries no LocalForward: the tunnel owns the forwards, so
    // VS Code connecting to the same alias cannot fight it for the ports.
    ec2_manager::ssh_config::ensure_include_directive()?;
    let alias = ec2_manager::ssh_config::write_managed_block(
        &ec2_manager::ssh_config::ManagedHost {
            instance_id: &bastion,
            name: &instance_name,
            user: &user,
            pem: &pem,
            profile: &profile,
            region: &region,
            forwards: &[],
        },
    )?;

    Ok(TunnelLaunch {
        alias,
        forwards: row.forwards.clone(),
        signature: ec2_manager::forwards::tunnel_signature(&row.forwards),
    })
}
```

- [ ] **Step 3: Rewrite `start_port_tunnel` to use it**

Replace the whole body of `start_port_tunnel` with:

```rust
/// Start the tunnel for one environment, replacing any running one whose
/// forward set no longer matches.
fn start_port_tunnel(&mut self, row: &PortForwardRow) {
    let signature = ec2_manager::forwards::tunnel_signature(&row.forwards);
    // A session already carrying exactly these forwards is left alone,
    // checked before anything else: a healthy tunnel must not collect an
    // error string because its account's credentials lapsed.
    if let Some(existing) = self.port_tunnels.get_mut(&row.key) {
        if existing.signature == signature && existing.is_running() {
            return;
        }
    }

    let launch = match self.resolve_tunnel_launch(row) {
        Ok(launch) => launch,
        Err(err) => {
            self.tunnel_errors.insert(row.key.clone(), err);
            return;
        }
    };

    // Only now drop the stale session. Resolving can fail, and killing a
    // working tunnel to then start nothing is worse than leaving it up.
    self.port_tunnels.remove(&row.key);

    match ec2_manager::tunnel::Tunnel::spawn(
        &launch.alias,
        &launch.forwards,
        launch.signature,
    ) {
        Ok(tunnel) => {
            self.tunnel_errors.remove(&row.key);
            // Forget any cleared failure so the next one speaks up.
            if self.config.clear_tunnel_dismissal(&row.account_id, &row.env) {
                let _ = self.config.save();
            }
            self.log_info(format!(
                "tunnel {}: started — ssh -N {}, {} forward(s): {}",
                row.label,
                launch.alias,
                launch.forwards.len(),
                describe_forwards(&launch.forwards)
            ));
            self.port_tunnels.insert(row.key.clone(), tunnel);
        }
        Err(err) => {
            self.log_error(format!("tunnel {}: {err}", row.label));
            self.tunnel_errors.insert(row.key.clone(), err);
        }
    }
}
```

- [ ] **Step 4: Add the `describe_forwards` helper used above**

The forward-rendering expression appears in the start log and will appear again in Task 6's test log. Extract it once, beside `classify_tunnel_failure`:

```rust
/// `ip:local->host:remote` for each forward, space separated — the form
/// the tunnel logs use, so a start line and a test line read alike.
fn describe_forwards(forwards: &[ec2_manager::forwards::ResolvedForward]) -> String {
    forwards
        .iter()
        .map(|f| format!("{}:{}->{}:{}", f.ip, f.local_port, f.host, f.remote_port))
        .collect::<Vec<_>>()
        .join(" ")
}
```

- [ ] **Step 5: Build and run the full suite**

Run: `CARGO_TARGET_DIR=/tmp/ec2m-test cargo build --features gui`
Expected: compiles, zero warnings

Run: `CARGO_TARGET_DIR=/tmp/ec2m-test cargo test --features gui`
Expected: PASS, no fewer tests than before the task (the 3 from Task 1 are additions)

- [ ] **Step 6: Commit**

```bash
git add src/bin/ec2_manager_gui.rs
git commit -m "Extract resolve_tunnel_launch from start_port_tunnel

Shared by the real tunnel start and (next) the Login dialog's test, so a
test cannot pass on a configuration the start would reject.

Two deliberate ordering changes: a running tunnel whose forwards still
match is now left alone before the auth guard runs, so a lapsed
credential no longer writes an error against a healthy session; and a
stale tunnel is dropped after a successful resolve rather than before, so
a failed reconfigure no longer kills a working one."
```

---

### Task 3: `set_port_forward_login` in the config

Persistence for the three values, written to the keys that already hold them. Lives in `src/config.rs` because that is where the keys and their tests already are.

**Files:**
- Modify: `src/config.rs` — method near `set_vscode_defaults` (~line 320), tests in the existing `mod tests` (~line 1250)

**Interfaces:**
- Consumes: `AppConfig::set_vscode_defaults`, `AppConfig::bastion_selection`, `AppConfig::set_bastion_selection`
- Produces: `pub fn set_port_forward_login(&mut self, account_id: &str, env: &str, bastion: &str, user: &str, pem: &str)`

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/config.rs`:

```rust
/// All three values land under the keys the rest of the app already
/// reads, so fixing a login here fixes Open in VS Code and the Scripts
/// dialogs too — that shared storage is the point of the feature.
#[test]
fn set_port_forward_login_writes_pem_user_and_bastion() {
    let mut cfg = AppConfig::default();
    cfg.set_port_forward_login("123456789012", "AUCT", "i-0abc", "bconrad", "/keys/auct.pem");

    assert_eq!(
        cfg.resolve_pem("123456789012", "AUCT", "i-0abc").as_deref(),
        Some("/keys/auct.pem")
    );
    assert_eq!(cfg.resolve_ssh_user("123456789012", "AUCT"), "bconrad");
    assert_eq!(
        cfg.bastion_selection("123456789012", "AUCT"),
        Some(("i-0abc".to_string(), String::new()))
    );
}

/// The bastion pair is shared with the Scripts dialogs and this dialog
/// only edits the primary. Blanking the secondary would silently break
/// the secondary-mirror step of Bastion New User.
#[test]
fn set_port_forward_login_keeps_the_secondary_bastion() {
    let mut cfg = AppConfig::default();
    cfg.set_bastion_selection("123456789012", "AUCT", "i-0old", "i-0second");
    cfg.set_port_forward_login("123456789012", "AUCT", "i-0new", "bconrad", "/keys/auct.pem");

    assert_eq!(
        cfg.bastion_selection("123456789012", "AUCT"),
        Some(("i-0new".to_string(), "i-0second".to_string()))
    );
}

/// The pem is added to the shared library, so a key chosen here shows up
/// in the Open in VS Code dropdown rather than only in this dialog.
#[test]
fn set_port_forward_login_adds_the_pem_to_the_library() {
    let mut cfg = AppConfig::default();
    cfg.set_port_forward_login("123456789012", "AUCT", "i-0abc", "bconrad", "/keys/auct.pem");
    assert!(cfg.sorted_pem_library().iter().any(|p| p == "/keys/auct.pem"));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_TARGET_DIR=/tmp/ec2m-test cargo test set_port_forward_login`
Expected: FAIL — `no method named 'set_port_forward_login' found`

- [ ] **Step 3: Write the implementation**

Add to `impl AppConfig` in `src/config.rs`, immediately after `set_vscode_defaults`:

```rust
/// Save the bastion, login user and key chosen in the Port Forwards
/// window's Login dialog.
///
/// Deliberately writes the keys the rest of the app already reads rather
/// than a private set: the pem and user are what Open in VS Code
/// resolves, and the bastion is the primary of the pair the Scripts
/// dialogs share. One place to fix a terminated bastion is the point.
///
/// The **secondary bastion is preserved** — this dialog only edits the
/// primary, and Bastion New User mirrors its run onto the secondary.
pub fn set_port_forward_login(
    &mut self,
    account_id: &str,
    env: &str,
    bastion: &str,
    user: &str,
    pem: &str,
) {
    self.set_vscode_defaults(account_id, env, pem, user);
    self.add_pem_to_library(pem);
    let secondary = self
        .bastion_selection(account_id, env)
        .map(|(_, secondary)| secondary)
        .unwrap_or_default();
    self.set_bastion_selection(account_id, env, bastion, &secondary);
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_TARGET_DIR=/tmp/ec2m-test cargo test set_port_forward_login`
Expected: PASS, 3 tests

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "Add AppConfig::set_port_forward_login

Writes the pem, login and primary bastion to the keys Open in VS Code and
the Scripts dialogs already read, preserving the secondary bastion."
```

---

### Task 4: Dialog state and the stale-bastion prefill

The dialog's data and the one piece of its logic that is worth testing: what the bastion field shows when the saved instance is gone.

**Files:**
- Modify: `src/bin/ec2_manager_gui.rs` — struct/enum near `PemDialog` (~line 525), prefill helper beside `retain_available_bastion` (~line 19281), field on the app struct near `port_tunnels` (~line 4362), tests in `mod tests`

**Interfaces:**
- Consumes: `retain_available_bastion`, `env_instances`, `bastion_label`
- Produces:
  - `struct PortForwardLoginDialog { key, account_id, env, label, bastion_id, bastion_label, bastion_query, user, pem, test }`
  - `enum TestState { Idle, Running { tunnel, deadline, started }, Passed { forwards, elapsed }, Failed { stderr, hint, elapsed } }`
  - `fn port_forward_login_bastion(saved: &str, available: &[(String, String)]) -> (String, String, bool)` — returns `(id, label, is_stale)`
  - App field `port_forward_login: Option<PortForwardLoginDialog>`

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/bin/ec2_manager_gui.rs`:

```rust
/// A saved bastion that is no longer in the inventory stays selected and
/// says so. `retain_available_bastion` blanks it, which is right for the
/// Scripts dialogs — an unavailable box there means the dialog is aimed
/// at nothing. Here the terminated instance is the user's whole
/// diagnosis, and emptying the field turns "this broke" into "you never
/// configured this".
#[test]
fn port_forward_login_bastion_flags_one_that_is_gone() {
    let available = vec![("i-0new".to_string(), "bastion-auct".to_string())];
    let (id, label, stale) = port_forward_login_bastion("i-0gone", &available);
    assert_eq!(id, "i-0gone");
    assert_eq!(label, "i-0gone — no longer in inventory");
    assert!(stale);
}

/// A bastion that is still there selects normally, labelled the way the
/// Scripts dropdowns label it.
#[test]
fn port_forward_login_bastion_selects_one_still_present() {
    let available = vec![("i-0new".to_string(), "bastion-auct".to_string())];
    let (id, label, stale) = port_forward_login_bastion("i-0new", &available);
    assert_eq!(id, "i-0new");
    assert_eq!(label, "bastion-auct  i-0new");
    assert!(!stale);
}

/// Nothing saved is not a stale value — the dialog opens with an empty
/// combo reading "choose", not a warning about an instance that never
/// existed.
#[test]
fn port_forward_login_bastion_treats_nothing_saved_as_not_stale() {
    let available = vec![("i-0new".to_string(), "bastion-auct".to_string())];
    let (id, label, stale) = port_forward_login_bastion("", &available);
    assert!(id.is_empty());
    assert!(label.is_empty());
    assert!(!stale);
}

/// An instance with no Name tag falls back to the bare id, as the Scripts
/// dropdowns do — and is not mistaken for a missing one.
#[test]
fn port_forward_login_bastion_handles_an_unnamed_instance() {
    let available = vec![("i-0new".to_string(), String::new())];
    let (id, label, stale) = port_forward_login_bastion("i-0new", &available);
    assert_eq!(id, "i-0new");
    assert_eq!(label, "i-0new");
    assert!(!stale);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `CARGO_TARGET_DIR=/tmp/ec2m-test cargo test --features gui port_forward_login_bastion`
Expected: FAIL — `cannot find function 'port_forward_login_bastion' in this scope`

- [ ] **Step 3: Write the prefill helper**

Add beside `retain_available_bastion` in `src/bin/ec2_manager_gui.rs`:

```rust
/// What the Login dialog's bastion combo shows for a saved instance id:
/// `(id, label, is_stale)`.
///
/// Unlike `retain_available_bastion`, a saved id missing from the
/// environment is **kept and flagged** rather than cleared. A terminated
/// bastion is exactly the failure this dialog exists to fix, and blanking
/// the field hides the cause.
fn port_forward_login_bastion(
    saved: &str,
    available: &[(String, String)],
) -> (String, String, bool) {
    let saved = saved.trim();
    if saved.is_empty() {
        return (String::new(), String::new(), false);
    }
    match available.iter().find(|(id, _)| id == saved) {
        Some((id, name)) if name.trim().is_empty() => (id.clone(), id.clone(), false),
        Some((id, name)) => (id.clone(), format!("{name}  {id}"), false),
        None => (
            saved.to_string(),
            format!("{saved} — no longer in inventory"),
            true,
        ),
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `CARGO_TARGET_DIR=/tmp/ec2m-test cargo test --features gui port_forward_login_bastion`
Expected: PASS, 4 tests

- [ ] **Step 5: Add the dialog state types**

Add next to `PemDialog` (~line 525) in `src/bin/ec2_manager_gui.rs`:

```rust
/// How the Login dialog's Test button is getting on.
///
/// `Running` owns the `Tunnel` itself rather than a handle: dropping the
/// dialog therefore kills the child (see `Tunnel`'s `Drop`), and a run
/// that survives its deadline can be handed straight to `port_tunnels`
/// without a second session ever being spawned.
enum TestState {
    Idle,
    Running {
        tunnel: ec2_manager::tunnel::Tunnel,
        deadline: Instant,
        started: Instant,
    },
    Passed {
        forwards: usize,
        elapsed: Duration,
    },
    Failed {
        stderr: Vec<String>,
        hint: Option<&'static str>,
        elapsed: Duration,
    },
}

/// Modal state for the Port Forwards window's per-row "Login…" button.
///
/// Present on every row, not only failing ones: the case it exists for is
/// a setup that used to work and stopped — a terminated bastion, a
/// deleted login, a rotated key.
struct PortForwardLoginDialog {
    /// `vscode_key(account_id, env)` — the row this belongs to.
    key: String,
    account_id: String,
    env: String,
    /// Environment name as the window shows it, for logs and the title.
    label: String,
    bastion_id: String,
    bastion_label: String,
    /// Substring narrowing the bastion dropdown, seeded from
    /// `primary_bastion_filter`.
    bastion_query: String,
    /// True when `bastion_id` names an instance not in the inventory.
    bastion_stale: bool,
    user: String,
    pem: String,
    test: TestState,
}
```

- [ ] **Step 6: Add the app field**

Add to the app struct beside `show_port_forwards` (~line 4372) in `src/bin/ec2_manager_gui.rs`:

```rust
/// Open "Login…" dialog for one Port Forwards row, if any.
port_forward_login: Option<PortForwardLoginDialog>,
```

and to the initialiser beside `port_tunnels: HashMap::new(),` (~line 4737):

```rust
port_forward_login: None,
```

- [ ] **Step 7: Build**

Run: `CARGO_TARGET_DIR=/tmp/ec2m-test cargo build --features gui`
Expected: compiles. `TestState`'s non-`Idle` variants are not constructed yet, so expect dead-code warnings — that is fine within this task and they go away in Task 6. If you would rather keep the build clean between commits, combine this task's commit with Task 5's.

- [ ] **Step 8: Commit**

```bash
git add src/bin/ec2_manager_gui.rs
git commit -m "Add Port Forwards login dialog state and stale-bastion prefill

A saved bastion missing from the inventory is kept and flagged rather
than cleared the way retain_available_bastion clears it — the terminated
instance is the diagnosis the dialog exists to show."
```

---

### Task 5: The `Login…` column and the dialog

Rendering. No unit tests are possible — egui cannot be driven headless here — so this task ends with manual verification steps.

**Files:**
- Modify: `src/bin/ec2_manager_gui.rs:7071-7174` (the grid in `render_port_forwards_window`), new `render_port_forward_login_dialog` beside it, call site at `:16694`

**Interfaces:**
- Consumes: `PortForwardLoginDialog`, `TestState`, `port_forward_login_bastion`, `AppConfig::set_port_forward_login`, `bastion_combo_ui`, `pem_library_combo_items`, `PEM_COMBO_POPUP_H`, `pem_basename`, `env_instances`
- Produces: `fn open_port_forward_login(&mut self, row: &PortForwardRow)`, `fn render_port_forward_login_dialog(&mut self, ctx: &egui::Context)`

- [ ] **Step 1: Widen the grid to five columns**

In `render_port_forwards_window`, change `.num_columns(4)` to `.num_columns(5)` and add a header cell after the Bastion header:

```rust
ui.label(egui::RichText::new("Bastion").strong());
ui.label("");
ui.end_row();
```

- [ ] **Step 2: Add the button to each row**

Immediately before `ui.end_row();` in the per-row loop (after the bastion cell, ~line 7171), add:

```rust
if ui
    .small_button("Login…")
    .on_hover_text(
        "Set the bastion, login user and key this environment \
         connects with, and test them.",
    )
    .clicked()
{
    open_login = Some(row.clone());
}
```

and declare `let mut open_login: Option<PortForwardRow> = None;` beside the other deferred actions (`let mut toggle: …`, ~line 6990), then act on it after the window closure, beside the `if let Some((row, enabled)) = toggle` block:

```rust
if let Some(row) = open_login {
    self.open_port_forward_login(&row);
}
```

- [ ] **Step 3: Add `open_port_forward_login`**

Add after `render_port_forwards_window`:

```rust
/// Open the Login dialog for one environment, prefilled from what is
/// saved. Available on every row, whatever its status — the case this
/// exists for is a setup that used to work and stopped.
fn open_port_forward_login(&mut self, row: &PortForwardRow) {
    let available = self.env_instances(&row.account_id, &row.env);
    let saved = row.bastion.clone().unwrap_or_default();
    let (bastion_id, bastion_label, bastion_stale) =
        port_forward_login_bastion(&saved, &available);
    let user = self.config.resolve_ssh_user(&row.account_id, &row.env);
    let pem = self
        .config
        .resolve_pem(&row.account_id, &row.env, &bastion_id)
        .unwrap_or_default();

    self.log_info(format!(
        "tunnel {}: login dialog opened — bastion={} user={} pem={}",
        row.label,
        if bastion_id.is_empty() { "none" } else { &bastion_id },
        user,
        if pem.is_empty() { "none".to_string() } else { pem_basename(&pem) },
    ));

    self.port_forward_login = Some(PortForwardLoginDialog {
        key: row.key.clone(),
        account_id: row.account_id.clone(),
        env: row.env.clone(),
        label: row.label.clone(),
        bastion_id,
        bastion_label,
        bastion_query: self.primary_bastion_filter.clone(),
        bastion_stale,
        user,
        pem,
        test: TestState::Idle,
    });
}
```

- [ ] **Step 4: Render the dialog**

Add after `open_port_forward_login`. The `take()` / put-back pattern matches `render_pem_dialog` at `:7509`:

```rust
/// Render the per-environment Login dialog.
fn render_port_forward_login_dialog(&mut self, ctx: &egui::Context) {
    let Some(mut dlg) = self.port_forward_login.take() else {
        return;
    };
    let mut window_open = true;
    let mut do_save = false;
    let mut do_test = false;
    let mut do_close = false;
    let available = self.env_instances(&dlg.account_id, &dlg.env);
    let library = self.config.sorted_pem_library();

    egui::Window::new(format!("{} — login", dlg.label))
        .collapsible(false)
        .resizable(false)
        .open(&mut window_open)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            // `bastion_combo_ui` is an associated function on the app
            // struct (no `self`), so it is called as `Self::…` — unlike
            // `pem_library_combo_items` and `retain_available_bastion`,
            // which are module-level free functions.
            Self::bastion_combo_ui(
                ui,
                "pf_login_bastion",
                "Bastion:",
                &dlg.bastion_query.clone(),
                &mut dlg.bastion_id,
                &mut dlg.bastion_label,
                &available,
            );
            if dlg.bastion_stale {
                ui.colored_label(
                    egui::Color32::from_rgb(220, 150, 60),
                    egui::RichText::new(
                        "The saved bastion is not in this environment's \
                         inventory — it may have been terminated. Pick \
                         another.",
                    )
                    .small(),
                );
            }

            ui.horizontal(|ui| {
                ui.label("Login user:");
                ui.add(
                    egui::TextEdit::singleline(&mut dlg.user)
                        .desired_width(160.0),
                );
            });

            ui.label("Key (pem):");
            ui.horizontal(|ui| {
                let selected_text = if dlg.pem.is_empty() {
                    "(choose a key)".to_string()
                } else {
                    pem_basename(&dlg.pem)
                };
                egui::ComboBox::from_id_salt("pf_login_pem")
                    .selected_text(selected_text)
                    .width(260.0)
                    .height(PEM_COMBO_POPUP_H)
                    .show_ui(ui, |ui| {
                        if let Some(picked) =
                            pem_library_combo_items(ui, &library, &dlg.pem)
                        {
                            dlg.pem = picked;
                        }
                    });
                if ui.button("+ Add pem...").clicked() {
                    let mut dialog = rfd::FileDialog::new()
                        .add_filter("SSH key", &["pem", "key"]);
                    if let Some(home) = ec2_manager::util::home_dir() {
                        dialog = dialog.set_directory(home.join(".ssh"));
                    }
                    if let Some(picked) = dialog.pick_file() {
                        dlg.pem = picked.to_string_lossy().to_string();
                    }
                }
            });

            ui.label(
                egui::RichText::new(
                    "These are the same key and login Open in VS Code uses, \
                     and the same bastion the Scripts dialogs run against.",
                )
                .small()
                .weak(),
            );

            ui.separator();
            ui.horizontal(|ui| {
                if ui
                    .button("Test login")
                    .on_hover_text(
                        "Saves, then opens the identical hidden ssh session \
                         the tunnel uses, forwards and all.",
                    )
                    .clicked()
                {
                    do_test = true;
                }
                render_test_state(ui, &mut dlg.test);
            });

            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    do_save = true;
                    do_close = true;
                }
                if ui.button("Cancel").clicked() {
                    do_close = true;
                }
            });
        });

    if do_test {
        self.save_port_forward_login(&dlg);
        self.start_port_forward_login_test(&mut dlg);
    } else if do_save {
        self.save_port_forward_login(&dlg);
    }
    if window_open && !do_close {
        self.port_forward_login = Some(dlg);
    }
}
```

- [ ] **Step 5: Add the save helper and the test-state renderer**

`save_port_forward_login` as a method beside the above:

```rust
/// Persist the dialog's three values and clear the row's stale error.
fn save_port_forward_login(&mut self, dlg: &PortForwardLoginDialog) {
    let previous = self
        .config
        .bastion_selection(&dlg.account_id, &dlg.env)
        .map(|(primary, _)| primary)
        .unwrap_or_default();
    if !previous.is_empty() && previous != dlg.bastion_id {
        // The pair is shared with Bastion New User, Bastion User Delete
        // and Vault IAM. Re-aiming those silently would be nasty.
        self.log_warn(format!(
            "tunnel {}: bastion changed {previous} → {} (also used by the \
             Scripts dialogs)",
            dlg.label, dlg.bastion_id
        ));
    }
    self.config.set_port_forward_login(
        &dlg.account_id,
        &dlg.env,
        &dlg.bastion_id,
        &dlg.user,
        &dlg.pem,
    );
    let _ = self.config.save();
    self.tunnel_errors.remove(&dlg.key);
    self.config.clear_tunnel_dismissal(&dlg.account_id, &dlg.env);
    self.log_info(format!(
        "tunnel {}: login saved — bastion={} user={} pem={}",
        dlg.label, dlg.bastion_id, dlg.user, dlg.pem
    ));
}
```

`render_test_state` as a free function beside `classify_tunnel_failure` (Task 6 fills in the `Running` arm's live counter; this is the whole thing):

```rust
/// The Test button's result line.
fn render_test_state(ui: &mut egui::Ui, state: &mut TestState) {
    match state {
        TestState::Idle => {}
        TestState::Running { started, .. } => {
            ui.spinner();
            ui.label(format!("testing… {:.1}s", started.elapsed().as_secs_f32()));
        }
        TestState::Passed { forwards, elapsed } => {
            ui.colored_label(
                egui::Color32::from_rgb(120, 180, 120),
                format!(
                    "connected in {:.1}s — {forwards} forward(s) bound",
                    elapsed.as_secs_f32()
                ),
            );
        }
        TestState::Failed { stderr, hint, elapsed } => {
            ui.vertical(|ui| {
                let headline = match hint {
                    Some(hint) => format!("failed after {:.1}s — {hint}", elapsed.as_secs_f32()),
                    None => format!("failed after {:.1}s", elapsed.as_secs_f32()),
                };
                ui.colored_label(egui::Color32::from_rgb(220, 80, 80), headline);
                // Verbatim, always. The hint points; this explains.
                for line in stderr {
                    ui.label(egui::RichText::new(line.as_str()).small().weak());
                }
            });
        }
    }
}
```

- [ ] **Step 6: Add a temporary stub for the test spawner**

Task 6 implements it. Add beside `save_port_forward_login` so this task compiles:

```rust
/// Filled in by the next task.
fn start_port_forward_login_test(&mut self, dlg: &mut PortForwardLoginDialog) {
    let _ = dlg;
}
```

- [ ] **Step 7: Call the renderer**

At `src/bin/ec2_manager_gui.rs:16694`, after `self.render_port_forwards_window(ctx);`:

```rust
self.render_port_forward_login_dialog(ctx);
```

- [ ] **Step 8: Build and check for warnings**

Run: `CARGO_TARGET_DIR=/tmp/ec2m-test cargo build --features gui`
Expected: compiles, zero new warnings

Run: `CARGO_TARGET_DIR=/tmp/ec2m-test cargo test --features gui`
Expected: PASS, all tests

- [ ] **Step 9: Verify by hand**

Run: `CARGO_TARGET_DIR=/tmp/ec2m-test cargo run --features gui --bin ec2_manager_gui`

Confirm, and note the result in the commit message:
1. Port Forwards opens and every row has a `Login…` button, including rows that are running normally.
2. Clicking it opens a dialog titled `<ENV> — login` with the bastion, user and pem prefilled from what is saved.
3. The pem dropdown scrolls with a visible bar (this is what `PEM_COMBO_POPUP_H` and `ScrollStyle::solid()` are for — a regression here shows as an empty-looking list).
4. Save closes the dialog, and a row that read `no pem saved for this environment` stops saying so.
5. The app log records the open and the save.

- [ ] **Step 10: Commit**

```bash
git add src/bin/ec2_manager_gui.rs
git commit -m "Add the per-row Login… dialog to the Port Forwards window

Bastion, login user and key for one environment, on every row rather than
only broken ones. Test login is stubbed; the next commit spawns it."
```

---

### Task 6: Test login — spawn, watch, adopt

The real work. Spawns via `Tunnel::spawn`, watches it frame by frame, and hands a passing session to `port_tunnels`.

**Files:**
- Modify: `src/bin/ec2_manager_gui.rs` — replace the Task 5 stub, and poll from `render_port_forward_login_dialog`

**Interfaces:**
- Consumes: `resolve_tunnel_launch`, `TunnelLaunch`, `Tunnel::spawn`, `Tunnel::is_running`, `Tunnel::errors`, `classify_tunnel_failure`, `describe_forwards`, `port_forward_rows`
- Produces: `fn start_port_forward_login_test(&mut self, dlg: &mut PortForwardLoginDialog)`, `fn poll_port_forward_login_test(&mut self, dlg: &mut PortForwardLoginDialog, ctx: &egui::Context)`

- [ ] **Step 1: Replace the stub with the spawner**

```rust
/// Spawn the identical session the tunnel would, and start watching it.
///
/// Not a cheap `ssh … true` probe: this carries the real `-L` forwards
/// under `ExitOnForwardFailure=yes`, which is where tunnels actually die.
/// The environment's own tunnel is stopped first — a byte-identical
/// session binds the same local ports, so leaving it up would fail the
/// test with `Address already in use` because the thing works.
fn start_port_forward_login_test(&mut self, dlg: &mut PortForwardLoginDialog) {
    let Some(row) = self
        .port_forward_rows()
        .into_iter()
        .find(|r| r.key == dlg.key)
    else {
        dlg.test = TestState::Failed {
            stderr: vec!["this environment is no longer listed — refresh".into()],
            hint: None,
            elapsed: Duration::ZERO,
        };
        return;
    };

    self.stop_port_tunnel(&dlg.key.clone(), &dlg.label.clone());

    let launch = match self.resolve_tunnel_launch(&row) {
        Ok(launch) => launch,
        Err(err) => {
            self.log_warn(format!(
                "tunnel {}: test login not started — {err}",
                dlg.label
            ));
            dlg.test = TestState::Failed {
                stderr: vec![err],
                hint: None,
                elapsed: Duration::ZERO,
            };
            return;
        }
    };

    self.log_info(format!(
        "tunnel {}: test login — ssh -N {}, {} forward(s): {}",
        dlg.label,
        launch.alias,
        launch.forwards.len(),
        describe_forwards(&launch.forwards)
    ));

    match ec2_manager::tunnel::Tunnel::spawn(
        &launch.alias,
        &launch.forwards,
        launch.signature,
    ) {
        Ok(tunnel) => {
            let now = Instant::now();
            dlg.test = TestState::Running {
                tunnel,
                // Long enough for an SSM-proxied handshake, short enough
                // that a user waits it out rather than wondering.
                deadline: now + Duration::from_secs(5),
                started: now,
            };
        }
        Err(err) => {
            self.log_error(format!("tunnel {}: test login — {err}", dlg.label));
            dlg.test = TestState::Failed {
                stderr: vec![err],
                hint: None,
                elapsed: Duration::ZERO,
            };
        }
    }
}
```

- [ ] **Step 2: Add the frame poll**

```rust
/// Advance a running test. Called every frame while the dialog is open.
///
/// `is_running` is `try_wait` underneath, so this never blocks — the same
/// idiom `poll_port_tunnels` uses. A session that outlives its deadline
/// **becomes** the environment's tunnel when the row is on, so the thing
/// that was tested is the thing that keeps running; otherwise it is
/// dropped, and `Tunnel`'s `Drop` kills the child.
fn poll_port_forward_login_test(
    &mut self,
    dlg: &mut PortForwardLoginDialog,
    ctx: &egui::Context,
) {
    let TestState::Running { tunnel, deadline, started } = &mut dlg.test else {
        return;
    };
    // Keep the live counter ticking without an input event.
    ctx.request_repaint_after(Duration::from_millis(100));

    if !tunnel.is_running() {
        let elapsed = started.elapsed();
        let stderr = tunnel.errors();
        let hint = classify_tunnel_failure(&stderr.join("\n"));
        self.log_error(format!(
            "tunnel {}: test login failed after {:.1}s",
            dlg.label,
            elapsed.as_secs_f32()
        ));
        for line in &stderr {
            self.log_error(format!("tunnel {}: test login — {line}", dlg.label));
        }
        dlg.test = TestState::Failed { stderr, hint, elapsed };
        return;
    }

    if Instant::now() < *deadline {
        return;
    }

    let elapsed = started.elapsed();
    let TestState::Running { tunnel, .. } =
        std::mem::replace(&mut dlg.test, TestState::Idle)
    else {
        return;
    };
    let forwards = self
        .port_forward_rows()
        .into_iter()
        .find(|r| r.key == dlg.key)
        .map(|r| r.forwards.len())
        .unwrap_or(0);
    let adopt = self.config.tunnel_enabled(&dlg.account_id, &dlg.env);
    self.log_info(format!(
        "tunnel {}: test login passed in {:.1}s — {forwards} forward(s) bound; {}",
        dlg.label,
        elapsed.as_secs_f32(),
        if adopt {
            "adopted as the live tunnel"
        } else {
            "discarded (environment is off)"
        }
    ));
    if adopt {
        self.port_tunnels.insert(dlg.key.clone(), tunnel);
        self.tunnel_errors.remove(&dlg.key);
    }
    dlg.test = TestState::Passed { forwards, elapsed };
}
```

- [ ] **Step 3: Call the poll from the renderer**

In `render_port_forward_login_dialog`, immediately after `let Some(mut dlg) = self.port_forward_login.take() else { return; };`:

```rust
self.poll_port_forward_login_test(&mut dlg, ctx);
```

- [ ] **Step 4: Build and run the suite**

Run: `CARGO_TARGET_DIR=/tmp/ec2m-test cargo build --features gui`
Expected: compiles, zero warnings — the `TestState` dead-code warnings from Task 4 are gone now that every variant is constructed

Run: `CARGO_TARGET_DIR=/tmp/ec2m-test cargo test --features gui`
Expected: PASS, all tests

Run: `CARGO_TARGET_DIR=/tmp/ec2m-test cargo clippy --features gui 2>&1 | grep -c warning`
Expected: no more than the 21 pre-existing warnings

- [ ] **Step 5: Verify by hand**

Run the GUI against a real environment and confirm, recording results in the commit message:
1. **Pass** — a correct bastion/user/pem shows `testing… 2.1s` counting up, then `connected in 5.0s — N forward(s) bound`, and the Port Forwards row goes to `running` without a second session appearing.
2. **Wrong key** — point the pem at another key. Expect `failed after …s — the key or the login user is wrong` with the `Permission denied (publickey)` line printed underneath verbatim.
3. **Unauthorized account** — expect the refusal (`… needs authorizing …`) with no ssh process spawned; check the log says `test login not started`.
4. **Row off** — untick the environment, test, and confirm the log says `discarded (environment is off)` and no tunnel appears in the window.
5. **Close mid-test** — press Test and close the dialog immediately; confirm no orphan `ssh` survives (`ps aux | grep 'ssh -N'` on Linux, Task Manager on Windows).

- [ ] **Step 6: Commit**

```bash
git add src/bin/ec2_manager_gui.rs
git commit -m "Test login spawns the real tunnel and adopts it on success

Not a probe: the identical ssh -N with the real -L forwards under
ExitOnForwardFailure=yes, watched frame by frame with try_wait so the UI
never blocks. A session that survives its deadline becomes the live
tunnel rather than being thrown away and respawned."
```

---

### Task 7: Toolbar wording

`ScriptState::Running` on a healthy tunnel reads as a connect in progress. There is no connecting state in this code at all — `alive` is binary off `try_wait` — so the message only appears once the session is up.

**Files:**
- Modify: `src/bin/ec2_manager_gui.rs:6786-6793` (`refresh_tunnel_status`)

**Interfaces:**
- Consumes: nothing new
- Produces: nothing new

- [ ] **Step 1: Change the wording**

Replace the healthy branch of `refresh_tunnel_status`:

```rust
self.tunnel_status = if blocked.is_empty() {
    Some((
        format!(
            "Forwarding ports for {} ({} tunnel{} up)",
            name_list(&running, enabled.len()),
            running.len(),
            if running.len() == 1 { "" } else { "s" }
        ),
        // ScriptState has only Running and Failed, and Running is what
        // colours this line. The wording carries the distinction
        // instead: this is a steady state, not a connect in progress —
        // there is no connecting state, since `alive` comes straight off
        // `try_wait`.
        ScriptState::Running,
    ))
} else {
```

- [ ] **Step 2: Build and run the suite**

Run: `CARGO_TARGET_DIR=/tmp/ec2m-test cargo test --features gui`
Expected: PASS

- [ ] **Step 3: Verify by hand**

Start the GUI with at least one working tunnel; the toolbar should read `Forwarding ports for all environments (1 tunnel up)` and the count should change when you tick another environment on.

- [ ] **Step 4: Commit**

```bash
git add src/bin/ec2_manager_gui.rs
git commit -m "Say how many tunnels are up in the forwarding status

'Forwarding ports for DEV1' with ScriptState::Running reads as a connect
in progress, so a tunnel that came up minutes ago looks like one that has
been hanging for minutes. There is no connecting state in this code —
the message only appears once the session is up and carrying forwards."
```

---

### Task 8: Document it in CLAUDE.md

Both copies carry the architecture notes; `WSL/CLAUDE.md` is the one that ships (the root copy is stale per the project's own memory, but the "Background port-forward tunnels" section exists in both and should not diverge further).

**Files:**
- Modify: `WSL/CLAUDE.md` — the "Background port-forward tunnels" section

**Interfaces:** none

- [ ] **Step 1: Add to the "Background port-forward tunnels" section**

Append these bullets, matching the existing voice (each states a decision and why it would break if reversed):

```markdown
- **The Login… dialog is on every row, not only broken ones.** The case it
  exists for is a setup that worked and stopped — a terminated bastion, a
  deleted login, a rotated key — which looks identical to a working row
  until the tunnel dies.
- **It writes the keys everything else reads**, via
  `AppConfig::set_port_forward_login`: the pem and login are what Open in
  VS Code resolves, and the bastion is the *primary* of the pair the
  Scripts dialogs share, so the **secondary is preserved**. Changing the
  bastion here re-aims Bastion New User, Bastion User Delete and Vault
  IAM, which is why that change is logged at warn and stated in the
  dialog.
- **A saved bastion missing from the inventory is kept and flagged**
  (`port_forward_login_bastion`), not cleared the way
  `retain_available_bastion` clears it for the Scripts dialogs. The
  terminated instance is the diagnosis; blanking the field turns "this
  broke" into "you never configured this".
- **Test login is the real tunnel, not a probe.** It spawns through
  `Tunnel::spawn` with the actual `-L` forwards under
  `ExitOnForwardFailure=yes` — port binding is where these sessions
  actually die, so a connection-only test would pass on a setup that
  cannot forward. `resolve_tunnel_launch` is shared with
  `start_port_tunnel` so there is one definition of "can this connect".
- **The environment's tunnel is stopped before a test and the passing
  session is adopted as the replacement.** A byte-identical session binds
  the same ports, so leaving the old one up fails the test with
  `Address already in use` *because* the thing works; and respawning
  after a pass would throw away the session that was just proven.
- **The watch is frame-polled, never blocking.** `is_running` is
  `try_wait`, the same idiom as `poll_port_tunnels`; egui is immediate
  mode and a 5s wait would freeze the app. `TestState::Running` owns the
  `Tunnel`, so closing the dialog mid-test kills the child via `Drop`.
- **Failure hints annotate stderr, never replace it**
  (`classify_tunnel_failure` returns `None` for anything unfamiliar). A
  confident wrong hint sends the user to change the wrong field.
- **Elapsed time is in both result log lines** because "should this be
  quick?" is otherwise unanswerable from the log, and the dialog counts
  up live so a slow connect looks slow rather than hung.
```

- [ ] **Step 2: Verify the surrounding section still reads as one list**

Read the whole "Background port-forward tunnels" section top to bottom. The new bullets go after the existing `tunnel_failures` bullet.

- [ ] **Step 3: Commit and push**

```bash
git add WSL/CLAUDE.md
git commit -m "Document the Port Forwards login dialog and test"
git push origin brandons_changes
```

---

## Self-Review

**Spec coverage**

| Spec section | Task |
|---|---|
| Problem / dead-end rows | 5 (button on every row) |
| `resolve_tunnel_launch` | 2 |
| The dialog (three fields) | 4 (state), 5 (render) |
| Saved bastion that is gone | 4 (`port_forward_login_bastion`), 5 (warning label) |
| Test login: saves first, stops tunnel, spawns | 6 |
| Frame-polled, not threaded | 6 |
| Passing session becomes the tunnel | 6 |
| Failure hints | 1 (`classify_tunnel_failure`), 5 (`render_test_state`) |
| Refusals | 6 (via `resolve_tunnel_launch`'s auth guard) |
| Storage / preserved secondary | 3 |
| Two cross-effects | 3 (doc comment), 5 (warn log + dialog line) |
| Logging | 5 (open, save, bastion change), 6 (test start/refused/pass/fail) |
| Toolbar wording | 7 |
| Testing | 1, 3, 4 |
| Out of scope: editing forwards | not implemented, correctly |

**Type consistency** — `TunnelLaunch { alias, forwards, signature }` is produced in Task 2 and consumed in Task 6 with those field names. `TestState`'s four variants are defined in Task 4, rendered in Task 5, and constructed in Task 6 with matching fields (`stderr: Vec<String>`, `hint: Option<&'static str>`, `elapsed: Duration`). `port_forward_login_bastion` returns `(String, String, bool)` in Task 4 and is destructured that way in Task 5. `set_port_forward_login(account_id, env, bastion, user, pem)` is defined in Task 3 and called with that order in Task 5.

**Known wrinkle** — Task 4 leaves dead-code warnings between its commit and Task 6's, because `TestState`'s non-`Idle` variants are not constructed until then. Task 4 Step 7 says so and offers squashing 4 and 5 as the alternative.

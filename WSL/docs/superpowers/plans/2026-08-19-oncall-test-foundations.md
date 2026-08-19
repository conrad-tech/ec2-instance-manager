# On-Call Test — Foundations Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build every piece the on-call test button needs except the GUI itself — the recipient lookup, the feature gate, the timestamped subject, the PowerShell send script, and its packaging — each independently testable.

**Architecture:** Four small, separately-testable additions following patterns this codebase already uses: a credential lookup mirroring `opsgenie_api_key`, a gate mirroring `UserSyncFeature`, a pure subject builder in `reaper.rs`, and a PowerShell script beside the exe mirroring `send_access_email.ps1`. No GUI code.

**Tech Stack:** Rust (existing crate, no new dependencies), PowerShell 5.1 (Outlook COM), bash for packaging.

**Specs:**
- `docs/superpowers/specs/2026-08-19-oncall-test-send-design.md`
- `docs/superpowers/specs/2026-08-19-alert-timestamp-in-payload-design.md`

## Global Constraints

- **No new crate dependencies.** This app deliberately shells out to `aws` and `curl` rather than linking SDKs.
- **The subject is the entire payload; the body is empty.** No instance id, AWS account number, environment or product name ever appears.
- **The code comes from `OutcomeCode::as_str()`, never a literal.** A drifted literal sends mail the daemon does not recognise, which it then escalates as an unknown code — a failure that looks like success.
- **Blank-but-set is absent**, matching `jsm_auth`'s existing rule: `set ESCALATION_MAILBOX=` must not shadow a stored credential.
- **Fail closed.** No recipient configured means the feature is unavailable, never a default destination.
- **Cross-compile before trusting GUI-adjacent changes.** The existing notes record that `launch_access_email` fails to compile only on the Windows target, because the crate's own `Result<T>` alias is in scope and takes one parameter.
- **Run tests with:** `cargo test --features gui`

## What this plan deliberately does NOT do

The GUI: the dropdown, the button, its placement, the spawn call and the result display. `ec2_manager_gui.rs` is ~1.2 MB and specifying that wiring precisely needs reading this plan's author has not done. Writing plausible-looking GUI code into a plan is how a plan starts lying.

Everything here is consumed by that work, and all of it is testable without a GUI — so the GUI plan starts against proven pieces rather than building foundations and wiring simultaneously.

---

## File Structure

```
src/jsm_auth.rs        # MODIFIED: escalation_mailbox() + its target/env constants
src/features.rs        # MODIFIED: OnCallTestFeature { allowed_users }
assets/features.json   # MODIFIED: "on_call_test": { "allowed_users": [] }
src/reaper.rs          # MODIFIED: Target.created_at; escalation_subject()
assets/scripts/send_escalation.ps1   # NEW
scripts/build_binaries.sh            # MODIFIED: copy the new script beside the exe
scripts/test_build_binaries.sh       # MODIFIED: assert that copy happens
```

`src/bin/ec2_manager_gui.rs` is touched in **exactly one place**: the test
helper `test_reaper_target()` at around line 23894 constructs a `Target`, so
Task 3's new field breaks it until that literal gains `created_at`. No
production GUI code changes.

---

### Task 1: The recipient lookup

**Files:**
- Modify: `src/jsm_auth.rs`

**Interfaces:**
- Consumes: existing `resolve_id`, `some_unless_blank`
- Produces: `pub const ESCALATION_MAILBOX_TARGET: &str`, `pub const ESCALATION_MAILBOX_ENV: &str`, and `pub fn escalation_mailbox() -> Option<String>`

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block at the bottom of `src/jsm_auth.rs`:

```rust
    #[test]
    fn the_escalation_mailbox_target_and_env_match_what_is_documented() {
        // These two strings are what the user typed into Credential Manager
        // with `cmdkey /generic:...` and may have exported. Renaming either
        // silently disables the feature on a machine that is correctly set
        // up, so they are pinned here rather than left to a refactor.
        assert_eq!(ESCALATION_MAILBOX_TARGET, "ec2_manager/escalation_mailbox");
        assert_eq!(ESCALATION_MAILBOX_ENV, "ESCALATION_MAILBOX");
    }

    #[test]
    fn a_blank_mailbox_value_is_absent_not_configured() {
        // Mirrors the rule the rest of this module uses: `set ESCALATION_MAILBOX=`
        // must not read as "configured with an empty address", which would
        // otherwise send escalation mail to nobody and report success.
        assert!(some_unless_blank(String::new()).is_none());
        assert!(some_unless_blank("   ".to_string()).is_none());
        assert_eq!(
            some_unless_blank("a@b.com".to_string()),
            Some("a@b.com".to_string())
        );
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --features gui jsm_auth 2>&1 | tail -20`
Expected: FAIL — `cannot find value ESCALATION_MAILBOX_TARGET in this scope`

- [ ] **Step 3: Write the implementation**

Add beside the other target constants in `src/jsm_auth.rs` (after `OPSGENIE_API_KEY_ENV`):

```rust
/// Credential Manager target for the escalation mailbox address (password
/// field only). Not a secret, but it must not be committed: it is a personal
/// address, and it is the point at which data leaves the org.
pub const ESCALATION_MAILBOX_TARGET: &str = "ec2_manager/escalation_mailbox";
/// Environment variable that overrides the stored escalation mailbox address.
pub const ESCALATION_MAILBOX_ENV: &str = "ESCALATION_MAILBOX";
```

And beside `opsgenie_api_key`:

```rust
/// The address escalation mail is sent to, if one is configured.
///
/// Deliberately has no fallback and no default. An address nobody configured
/// must never become an address the app invents — this is the destination for
/// mail that crosses the org boundary, so "unset" has to mean the feature is
/// unavailable rather than "send it somewhere". Same fail-closed posture as
/// `allow_delete_user` and the Alerts button.
///
/// Stored in Credential Manager rather than `features.json` (committed, so a
/// personal address would enter the corporate repo) or `config.ini` (plain
/// text, and it would let any user aim the app at any external address).
pub fn escalation_mailbox() -> Option<String> {
    some_unless_blank(resolve_id(
        ESCALATION_MAILBOX_ENV,
        ESCALATION_MAILBOX_TARGET,
        "",
    ))
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test --features gui jsm_auth 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/jsm_auth.rs
git commit -m "Resolve the escalation mailbox from Credential Manager"
```

---

### Task 2: The feature gate

**Files:**
- Modify: `src/features.rs`, `assets/features.json`

**Interfaces:**
- Consumes: existing `user_in_list`
- Produces: `pub struct OnCallTestFeature { pub allowed_users: Vec<String> }` with `pub fn is_allowed_user(&self, user: &str) -> bool`, reachable as a field on the root features struct

- [ ] **Step 1: Write the failing test**

Add to `src/features.rs`'s test module:

```rust
    #[test]
    fn the_on_call_test_gate_follows_the_shared_allow_list_rules() {
        let nobody = OnCallTestFeature { allowed_users: vec![] };
        assert!(!nobody.is_allowed_user("brandon"));

        let everyone = OnCallTestFeature { allowed_users: vec!["*".to_string()] };
        assert!(everyone.is_allowed_user("anyone"));

        let named = OnCallTestFeature {
            allowed_users: vec!["Brandon".to_string()],
        };
        assert!(named.is_allowed_user("brandon"), "match is case-insensitive");
        assert!(!named.is_allowed_user("someone_else"));
    }

    #[test]
    fn the_shipped_on_call_test_gate_is_closed() {
        // It sends mail out of the org. It ships to nobody, and handing it
        // out is a deliberate edit plus a rebuild -- same posture as
        // user_sync and alerts.
        let shipped = shipped_features();
        assert!(
            shipped.on_call_test.allowed_users.is_empty(),
            "on_call_test must ship with an empty allow-list"
        );
    }
```

If the test module has no `shipped_features()` helper, use whichever function the neighbouring `alerts`/`user_sync` shipped-config tests already use to parse `assets/features.json`, and follow their exact style.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --features gui features 2>&1 | tail -20`
Expected: FAIL — `cannot find type OnCallTestFeature in this scope`

- [ ] **Step 3: Write the implementation**

In `src/features.rs`, beside `UserSyncFeature`:

```rust
/// The "Send test escalation" action, which mails a content-free coded
/// subject to the configured escalation mailbox.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct OnCallTestFeature {
    /// OS usernames allowed to see the action (case-insensitive).
    /// `["*"]` for everyone, an empty list for nobody. Shipped empty.
    pub allowed_users: Vec<String>,
}

impl OnCallTestFeature {
    /// True when `user` may see the test action.
    ///
    /// The action itself is also hidden when no recipient is configured — a
    /// visible control that cannot work invites a click and then explains
    /// nothing. That check belongs to the caller, which has the credential
    /// lookup; this gate answers only "is this user allowed".
    pub fn is_allowed_user(&self, user: &str) -> bool {
        user_in_list(&self.allowed_users, user)
    }
}
```

Add the field to the root features struct beside `user_sync`, matching its
attribute style exactly:

```rust
    pub on_call_test: OnCallTestFeature,
```

- [ ] **Step 4: Add it to the shipped config**

In `assets/features.json`, beside the `user_sync` block:

```json
  "_on_call_test_comment": "\"Send test escalation\" action. Sends one content-free email to the escalation mailbox so the whole notification chain -- Outlook, the mail path, the mailbox, the Pi daemon, Telegram, the phone -- can be exercised on demand. The subject carries an outcome code and the alert timestamp and NOTHING else: no instance id, no account number, no environment, no product name. The body is empty. The recipient is NOT configured here: it is read from Windows Credential Manager (ec2_manager/escalation_mailbox) or the ESCALATION_MAILBOX environment variable, so a personal address never enters this committed file, and an unconfigured address hides the action rather than defaulting to one. allowed_users lists the OS usernames that see it: [\"*\"] = everyone, [] = nobody (the shipped default). It sends mail out of the org, so it ships closed -- add usernames and rebuild to hand it out.",
  "on_call_test": {
    "allowed_users": []
  },
```

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test --features gui features 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/features.rs assets/features.json
git commit -m "Gate the on-call test action; ship it closed"
```

---

### Task 3: The timestamped subject

**Files:**
- Modify: `src/reaper.rs`

**Interfaces:**
- Consumes: existing `OutcomeCode`, `Target`, `Alert`
- Produces: `Target.created_at: String` (new field), and `pub fn escalation_subject(code: OutcomeCode, created_at: &str) -> String`

- [ ] **Step 1: Write the failing tests**

Add to `src/reaper.rs`'s test module:

```rust
    #[test]
    fn the_subject_is_the_code_then_the_timestamp() {
        assert_eq!(
            escalation_subject(OutcomeCode::Failure, "2026-08-19T20:12:09Z"),
            "RE-F 2026-08-19T20:12:09Z"
        );
    }

    #[test]
    fn the_subject_takes_the_code_from_the_enum_not_a_literal() {
        // A literal here could drift from the vocabulary the daemon matches
        // on. The mail would still send, the daemon would not recognise the
        // code, and it would escalate it as UNKNOWN -- a failure that looks
        // exactly like success.
        for code in [
            OutcomeCode::Failure,
            OutcomeCode::FailureQuiet,
            OutcomeCode::Ok,
            OutcomeCode::Canary,
        ] {
            let subject = escalation_subject(code, "2026-08-19T20:12:09Z");
            assert!(
                subject.starts_with(code.as_str()),
                "{subject} must start with {}",
                code.as_str()
            );
        }
    }

    #[test]
    fn a_blank_timestamp_yields_the_bare_code_with_no_trailing_space() {
        // The feed has been observed serving unrendered templates and absent
        // tags, so a missing timestamp must degrade to the old behaviour --
        // never a trailing space, and never a blocked send. An escalation
        // without a timestamp is worth more than one that does not arrive.
        assert_eq!(escalation_subject(OutcomeCode::Failure, ""), "RE-F");
        assert_eq!(escalation_subject(OutcomeCode::Failure, "   "), "RE-F");
    }

    #[test]
    fn the_subject_carries_nothing_but_the_code_and_the_time() {
        let subject = escalation_subject(OutcomeCode::Failure, "2026-08-19T20:12:09Z");
        for leak in ["i-", "prod", "dev", "1234", "account", "env"] {
            assert!(
                !subject.to_ascii_lowercase().contains(leak),
                "subject {subject} must not contain {leak}"
            );
        }
    }

    #[test]
    fn a_matched_target_carries_the_alerts_created_at() {
        let mut alert = alert_with_alertname("reaper fired");
        alert.created_at = "2026-08-19T20:12:09Z".to_string();
        alert.message = "i-0123456789abcdef0 is unhealthy".to_string();
        let target = match_alert(&alert, &cfg()).expect("should match");
        assert_eq!(target.created_at, "2026-08-19T20:12:09Z");
    }
```

Use whatever helper the neighbouring `match_alert` tests already use to build
an `Alert` — the name `alert_with_alertname` above is a placeholder for that
existing helper, and the surrounding tests show its real name and signature.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --features gui reaper 2>&1 | tail -20`
Expected: FAIL — `cannot find function escalation_subject`, and `no field created_at on type Target`

- [ ] **Step 3: Write the implementation**

Add the field to `Target`:

```rust
pub struct Target {
    pub alert_id: String,
    pub instance_id: String,
    pub account_id: String,
    pub environment: String,
    /// `createdAt` from the alert, verbatim as the API returned it (RFC 3339,
    /// UTC). Carried so a remediation can be tied back to the alert that
    /// caused it when several fire close together.
    pub created_at: String,
}
```

Populate it wherever `match_alert` constructs the `Target`, from
`alert.created_at.clone()`.

Then add, near `OutcomeCode`:

```rust
/// The subject line of an escalation email. This is the entire payload.
///
/// `<code> <createdAt>` — the code first, so it stays the head of the
/// message, and the timestamp exactly as the API returned it (RFC 3339, UTC).
/// The Pi-side daemon renders it in local time; sending UTC keeps the wire
/// format unambiguous and machine-parseable.
///
/// The body is empty and stays empty. What leaves the org is a code and a
/// time — no instance id, account number, environment or product name.
///
/// A blank timestamp yields the bare code. The feed has been observed serving
/// unrendered templates and absent tags, and an escalation that arrives
/// without a timestamp is worth far more than one that does not arrive.
pub fn escalation_subject(code: OutcomeCode, created_at: &str) -> String {
    let stamp = created_at.trim();
    if stamp.is_empty() {
        code.as_str().to_string()
    } else {
        format!("{} {}", code.as_str(), stamp)
    }
}
```

- [ ] **Step 4: Fix the one construction site outside this file**

Adding a field to `Target` breaks every struct literal. There is exactly one
outside `reaper.rs`: the test helper `test_reaper_target()` at around
`src/bin/ec2_manager_gui.rs:23894`. Give it a `created_at` — any plausible
RFC 3339 value; it is a fixture, not an assertion.

This is the only edit this plan makes to the GUI file.

- [ ] **Step 5: Run to verify they pass**

Run: `cargo test --features gui reaper 2>&1 | tail -20`
Expected: PASS.

Then confirm the whole crate still builds, since the field change reaches
beyond `reaper.rs`:

Run: `cargo build --features gui 2>&1 | tail -5`
Expected: no errors. If the compiler names another literal, fix it the same
way and note it in your report — it means this survey missed one.

- [ ] **Step 6: Commit**

```bash
git add src/reaper.rs src/bin/ec2_manager_gui.rs
git commit -m "Carry the alert timestamp into the escalation subject"
```

---

### Task 4: The send script and its packaging

**Files:**
- Create: `assets/scripts/send_escalation.ps1`
- Modify: `scripts/build_binaries.sh`, `scripts/test_build_binaries.sh`

**Interfaces:**
- Consumes: nothing from Tasks 1–3 at build time; the GUI plan passes `-To` and `-Subject`
- Produces: a script that prints exactly one marker line, `SENT address='…'` or `FAILED reason='…'`

- [ ] **Step 1: Write the script**

```powershell
# send_escalation.ps1 -- send one content-free escalation email via Outlook.
#
# Deliberately NOT part of send_access_email.ps1. That script's recipient
# gates (directory user, domain allow-list, local-format match) exist because
# it attaches a PRIVATE KEY. A fixed configured address with no attachment
# must not inherit them, and those gates must not be relaxed to fit this.
#
# Prints exactly one marker line the GUI parses:
#   SENT address='someone@example.com'
#   FAILED reason='Outlook is not available'
param(
    [Parameter(Mandatory = $true)][string]$To,
    [Parameter(Mandatory = $true)][string]$Subject
)

$ErrorActionPreference = 'Stop'

function Write-Marker {
    param([string]$Line)
    Write-Output $Line
}

if ([string]::IsNullOrWhiteSpace($To)) {
    Write-Marker "FAILED reason='no recipient configured'"
    exit 1
}

try {
    $outlook = New-Object -ComObject Outlook.Application
} catch {
    Write-Marker "FAILED reason='Outlook is not available'"
    exit 1
}

try {
    $mail = $outlook.CreateItem(0)
    $mail.To = $To
    $mail.Subject = $Subject
    # The body stays empty. The subject is the entire payload -- see
    # 2026-08-14-escalation-notifier-design.md.
    $mail.Body = ''
    $mail.Send()
    Write-Marker "SENT address='$To'"
    exit 0
} catch {
    $reason = $_.Exception.Message -replace "'", '' -replace "`r?`n", ' '
    Write-Marker "FAILED reason='$reason'"
    exit 1
}
```

- [ ] **Step 2: Copy it beside the exe at package time**

In `scripts/build_binaries.sh`, in `package_windows_zip`, directly after the
existing `send_access_email.ps1` copy, add the same shape for the new script.
Find the existing block (around line 347) and mirror it exactly — including
whatever guard and destination the existing one uses — for
`assets/scripts/send_escalation.ps1`.

It is copied beside the exe rather than embedded for the same EDR reason the
existing one is: a script written to `%TEMP%` and run from there is a pattern
EDRs quarantine on sight, and this app has a CrowdStrike quarantine in its
history.

- [ ] **Step 3: Assert the copy happens**

**There is no existing assertion for `send_access_email.ps1` to copy** — the
Windows script copy is untested today, which is why this step writes a real
test rather than mirroring one. Follow the shape of
`test_package_linux_zip_creates_archive_with_artifacts`, which already saves
and restores a dist-dir global around a call to the function under test.

Add to `scripts/test_build_binaries.sh`, and register it wherever the file
invokes its `test_*` functions:

```bash
test_package_windows_zip_ships_both_powershell_scripts() {
  if ! command -v zip >/dev/null 2>&1 || ! command -v unzip >/dev/null 2>&1; then
    echo "skipping windows zip packaging test (zip/unzip not installed)"
    return 0
  fi

  local tmpdir
  tmpdir="$(mktemp -d)"
  local original_windows_dist_dir="$WINDOWS_DIST_DIR"
  WINDOWS_DIST_DIR="$tmpdir"

  # Same versioned names copy_artifact writes.
  touch "$WINDOWS_DIST_DIR/${CLI_APP_NAME}_${APP_VERSION}.exe"
  touch "$WINDOWS_DIST_DIR/${GUI_APP_NAME}_${APP_VERSION}.exe"

  SKIP_ICON_VERIFY=1 package_windows_zip

  # Both scripts must land beside the exe rather than inside the archive
  # only: they are run from the file next to the executable, never written
  # to %TEMP% and run from there, because that is a pattern EDRs quarantine
  # on sight and this app has a CrowdStrike quarantine in its history.
  local missing=""
  [[ -f "$WINDOWS_DIST_DIR/send_access_email.ps1" ]] || missing="$missing send_access_email.ps1"
  [[ -f "$WINDOWS_DIST_DIR/send_escalation.ps1" ]] || missing="$missing send_escalation.ps1"
  if [[ -n "$missing" ]]; then
    echo "assertion failed: not copied beside the exe:$missing" >&2
    exit 1
  fi

  rm -rf "$tmpdir"
  WINDOWS_DIST_DIR="$original_windows_dist_dir"
}
```

If `package_windows_zip` turns out to need more setup than this — an icon
file, a different global name — adapt to what the function actually reads
rather than forcing the test to match this sketch, and say what you changed
in your report. `SKIP_ICON_VERIFY=1` is set because `verify_windows_icon`
re-reads the exe it copied and `exit 1`s unless it has a real `.rsrc`
section, which a `touch`ed empty file does not.

**If the test proves impractical to write in a reasonable time, stop and
report it rather than deleting the assertion.** An untested copy step is how
`config.example.ini` survived in `deploy.sh` after being deleted.

- [ ] **Step 4: Verify**

Run: `bash -n assets/scripts/send_escalation.ps1 2>/dev/null; bash -n scripts/build_binaries.sh && bash -n scripts/test_build_binaries.sh && echo "shell syntax OK"`
Expected: `shell syntax OK`. (The first command is expected to be a no-op or complain — PowerShell is not bash; it is there only so a stray bash-ism is noticed.)

Run: `./scripts/test_build_binaries.sh 2>&1 | tail -20`
Expected: the suite passes, including the new assertion.

- [ ] **Step 5: Commit**

```bash
git add assets/scripts/send_escalation.ps1 scripts/build_binaries.sh scripts/test_build_binaries.sh
git commit -m "Add the escalation send script and ship it beside the exe"
```

---

### Task 5: Prove both targets still build

**Files:** none

- [ ] **Step 1: Full test suite**

Run: `cargo test --features gui 2>&1 | tail -15`
Expected: all tests pass. Record the count; it must be higher than before this plan and no test may have been removed.

- [ ] **Step 2: Clippy**

Run: `cargo clippy --features gui 2>&1 | grep -E "^(error|warning)" | head -20`
Expected: no NEW warnings. The repo has known pre-existing ones (`derivable_impls` on Mode, `too_many_arguments` on `sim::make_instance`, `collapsible_if`, `let_and_return`, `manual_is_multiple_of`).

- [ ] **Step 3: Windows cross-compile**

Run: `CARGO_TARGET_DIR=/tmp/ec2m cargo build --release --target x86_64-pc-windows-gnu --features gui 2>&1 | tail -15`
Expected: exit 0.

`CARGO_TARGET_DIR` is set to a space-free path deliberately: mingw's `dlltool`
does not quote the paths it passes to the assembler, so it fails outright when
the build directory contains a space — as this repo's does.

This step is not optional. The existing notes record that `launch_access_email`
compiles on Linux and fails on the Windows target because the crate's own
`Result<T>` alias is in scope and takes one parameter. Nothing in this plan adds
a Windows-only function, but the next plan does, and this establishes the
baseline is clean before it lands.

- [ ] **Step 4: Commit if anything needed fixing**

```bash
git add -A
git commit -m "Fix build issues found by the cross-compile check"
```

Skip if nothing changed.

---

## Self-Review

**Spec coverage:**

| Requirement | Spec | Task |
|---|---|---|
| Recipient from Credential Manager, env override | send | 1 |
| Blank-but-set treated as absent | send | 1 |
| No default destination; unset means unavailable | send | 1 |
| `allowed_users` gate, shipped empty | send | 2 |
| Subject `<code> <timestamp>`, empty body | timestamp | 3 |
| Code from `OutcomeCode::as_str()`, never a literal | send | 3 |
| Blank timestamp yields the bare code | timestamp | 3 |
| No instance id / account / environment / product name | both | 3 |
| `Target` carries `created_at` | timestamp | 3 |
| Its own script, not `send_access_email.ps1` | send | 4 |
| Copied beside the exe, not embedded | send | 4 |
| Marker line the GUI parses | send | 4 |
| Cross-compile before trusting it | send | 5 |

**Deliberately not covered:** the GUI — dropdown, button, placement, spawn, result display — and the `CREATE_NO_WINDOW` / no-`-WindowStyle Hidden` spawn constraints, which are properties of the spawn call and therefore belong with it. Also not covered: the Pi-side rendering of the timestamp in the Telegram message, which lives in the other repo and gets its own plan.

**Placeholder scan:** none. Two tasks say "mirror the existing block exactly" for `build_binaries.sh` and the `features.rs` test helper — that is a deliberate instruction to match a local convention the implementer can read, not a gap.

**Type consistency:** `escalation_subject` takes `OutcomeCode` by value (it is `Copy`) and `&str`, returning `String`. `Target.created_at` is `String`, matching the other four fields. `escalation_mailbox` returns `Option<String>`, matching `opsgenie_api_key`.

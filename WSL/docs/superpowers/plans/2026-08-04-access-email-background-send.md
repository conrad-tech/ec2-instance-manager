# Access Email Background Send Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Run the access email automatically after a create, send it unattended only when the recipient resolves to one person in the organization's own mail domain, report the outcome in the GUI, and correct the subject, body, and failure behavior.

**Architecture:** Four layers, in order. `AccessEmailConfig` (Rust lib) gains `email_domain` and `auto_run`; `build_email_command` (Rust GUI) passes the domain through as `-Domain`; `send_access_email.ps1` (PowerShell) does the domain check, clears the To field on every failure, encrypts before deciding, emits the new subject and body, and gains a `-Quiet` switch; the GUI spawns that script itself on Windows and renders the outcome as a status line in the result popup. The Rust layers are unit-tested; the PowerShell is verified by hand on Windows because it drives live Outlook COM.

**Revision note:** Tasks 4 and 5 (auto-run and status reporting) were added after live testing showed the copy-a-command-only flow read as a broken feature. The ✉ Send Email Command menu is retained as a manual fallback.

**Tech Stack:** Rust (serde `Deserialize` with `#[serde(default)]`), Windows PowerShell 5.1 + Outlook COM object model, `cargo test --features gui`.

## Global Constraints

- **Design spec:** `docs/superpowers/plans/../specs/2026-08-04-access-email-background-send-design.md`. Read it before starting.
- **EDR hygiene when spawning (Task 4).** Run the `send_access_email.ps1` that sits **next to the exe**; never write a script to `%TEMP%` and run that. Never pass `-WindowStyle Hidden`. Both are patterns EDRs quarantine on sight. If the file is missing, log and skip — do not fall back to temp.
- **The GUI still must not talk to Outlook directly.** No COM, no MAPI, no mail library in Rust. The only Outlook contact is the spawned PowerShell.
- **`.ps1` files must be pure ASCII.** Windows PowerShell reads scripts as ANSI; a curly quote or em dash breaks parsing with "string is missing the terminator". No `—`, `’`, `“`, `”` anywhere in a `.ps1`.
- **`Alt+6` is a toggle.** `$EncryptSendKeys` must only ever be sent when headless encryption was **not** confirmed. Sending it on an already-encrypted item strips the encryption.
- **Never send without confirmed encryption.** The PEM is a private key. The send path requires resolved AND domain-matched AND encryption-confirmed — all three.
- **Blank `email_domain` disables the domain check** and preserves current behavior. It is not an error.
- Build/test commands (from repo root `WSL/`, with `export PATH="$HOME/.cargo/bin:$PATH"`):
  - `cargo build --features gui`
  - `cargo test --features gui`
  - `cargo clippy --features gui`
- If `D:` is low on space, build with `CARGO_TARGET_DIR=/tmp/ec2m-test`. A full disk shows up as `error: failed to build archive ... Input/output error (os error 5)`, not a "no space" message.
- Baseline before this work: 305 tests pass (168 lib + 3 CLI + 134 GUI), zero build warnings, 21 pre-existing clippy warnings.

---

### Task 1: Add `email_domain` and `auto_run` to the features config

**Files:**
- Modify: `src/features.rs:213-243` (`AccessEmailConfig` struct and its `Default` impl)
- Modify: `assets/features.json` (the `access_email` block and the `_access_email_comment` above it)
- Test: `src/features.rs` (the existing `#[cfg(test)] mod tests` at the end of the file)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `ec2_manager::features::AccessEmailConfig.email_domain: String`, defaulting to `String::new()`. Task 2 reads this field.

- [ ] **Step 1: Write the failing test**

Append to the `mod tests` block at the end of `src/features.rs`:

```rust
    #[test]
    fn access_email_domain_defaults_to_blank() {
        let cfg = AccessEmailConfig::default();
        assert_eq!(cfg.email_domain, "");
    }

    #[test]
    fn access_email_domain_is_read_from_json() {
        let cfg: AccessEmailConfig =
            serde_json::from_str(r#"{"email_domain":"xyz.com"}"#).expect("parses");
        assert_eq!(cfg.email_domain, "xyz.com");
        // Unlisted fields still fall back to the Default impl.
        assert!(cfg.enabled);
    }

    #[test]
    fn access_email_auto_run_defaults_on_and_can_be_disabled() {
        assert!(AccessEmailConfig::default().auto_run);
        let cfg: AccessEmailConfig =
            serde_json::from_str(r#"{"auto_run":false}"#).expect("parses");
        assert!(!cfg.auto_run);
    }
```

If `serde_json` is not already imported in that test module, use the fully
qualified `serde_json::from_str` as written above — the crate is already a
dependency of the lib.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --features gui access_email_domain`
Expected: FAIL — `no field 'email_domain' on type 'AccessEmailConfig'`

- [ ] **Step 3: Add the field**

In `src/features.rs`, inside `pub struct AccessEmailConfig`, after the
`pub enabled: bool` field:

```rust
    /// The organization's own mail domain, e.g. "xyz.com". A resolved
    /// recipient's address must sit in this domain before anything is sent
    /// unattended - `Resolve()` also matches the local Contacts folder and
    /// the autocomplete cache, so a stale personal entry would otherwise
    /// receive a private key. Blank disables the check.
    pub email_domain: String,
```

And in the `Default` impl, after `enabled: true,`:

```rust
            email_domain: String::new(),
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --features gui access_email_domain`
Expected: PASS, 2 tests

- [ ] **Step 5: Update features.json**

In `assets/features.json`, add `email_domain` as the second key of the
`access_email` block:

```json
  "access_email": {
    "enabled": true,
    "email_domain": "",
    "encrypt_template_guid": "{00000000-0000-0000-0000-000000000000}",
    "encrypt_permission": 3,
    "encrypt_permission_service": 1,
    "encrypt_smime_flag": 0,
    "encrypt_sendkeys": "%6"
  }
```

Ships blank so a build without the value configured behaves exactly as it does
today. In the `_access_email_comment` string immediately above the block, append
this sentence before the closing quote:

```
 email_domain is your organization's mail domain (e.g. xyz.com): a recipient that resolves to an address outside it is never sent to unattended, which stops a stale local Contacts entry from receiving the PEM. Leave it empty to skip the check.
```

- [ ] **Step 6: Verify the JSON still parses and the whole suite passes**

Run: `python3 -m json.tool assets/features.json > /dev/null && echo VALID`
Expected: `VALID`

Run: `cargo test --features gui`
Expected: PASS, 307 tests (2 more than the 305 baseline)

- [ ] **Step 7: Commit**

```bash
git add src/features.rs assets/features.json
git commit -m "Add access_email.email_domain to the features config"
```

---

### Task 2: Pass `-Domain` through to the helper script

**Files:**
- Modify: `src/bin/ec2_manager_gui.rs:1911-1923` (the `args` vector in `build_email_command`)
- Test: `src/bin/ec2_manager_gui.rs` — the existing `gui::tests` module, around `build_email_command_passes_every_arg_to_the_helper_script` at line 16871

**Interfaces:**
- Consumes: `AccessEmailConfig.email_domain` from Task 1.
- Produces: the generated command string now contains `-Domain '<value>'` in both the bash and PowerShell forms. Task 3 reads it as `[string]$Domain`.

- [ ] **Step 1: Write the failing test**

In `src/bin/ec2_manager_gui.rs`, update the `access_email_cfg` helper (added
alongside the existing tests) to set the new field, so every test builds a
config with a domain:

```rust
        fn access_email_cfg(enabled: bool) -> ec2_manager::features::AccessEmailConfig {
            ec2_manager::features::AccessEmailConfig {
                enabled,
                email_domain: "xyz.com".to_string(),
                encrypt_template_guid: "{abc}".to_string(),
                encrypt_permission: 3,
                encrypt_permission_service: 1,
                encrypt_smime_flag: 0,
                encrypt_sendkeys: "%6".to_string(),
            }
        }
```

Add `"-Domain 'xyz.com'",` to the `for expected in [...]` array inside
`build_email_command_passes_every_arg_to_the_helper_script`, directly after the
`"-Pem '/p.pem'",` entry.

Then add this new test immediately after that function:

```rust
        #[test]
        fn build_email_command_still_passes_domain_when_blank() {
            // A blank domain must still emit the flag, so argument positions
            // never shift and the script's default stays a plain empty string.
            let mut cfg = access_email_cfg(true);
            cfg.email_domain = String::new();
            let cmd = build_email_command(&cfg, "jdoe", "DEV1", "i-1", "i-2", "/p.pem")
                .expect("enabled config builds a command");
            assert!(cmd.wsl.contains("-Domain ''"), "{}", cmd.wsl);
            assert!(cmd.powershell.contains("-Domain ''"), "{}", cmd.powershell);
        }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --features gui build_email_command`
Expected: FAIL — `build_email_command_passes_every_arg_to_the_helper_script`
panics with `bash cmd missing -Domain 'xyz.com'`, and
`build_email_command_still_passes_domain_when_blank` fails the same way.

- [ ] **Step 3: Add the argument**

In `build_email_command`, insert one entry into the `args` vector, directly
after the `("-Pem", pem_path.to_string()),` line:

```rust
            ("-Domain", cfg.email_domain.clone()),
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --features gui build_email_command`
Expected: PASS, 4 tests

- [ ] **Step 5: Verify nothing else regressed**

Run: `cargo test --features gui`
Expected: PASS, 308 tests

Run: `cargo build --features gui 2>&1 | grep -c '^warning'`
Expected: `0`

- [ ] **Step 6: Commit**

```bash
git add src/bin/ec2_manager_gui.rs
git commit -m "Pass the configured mail domain to send_access_email.ps1"
```

---

### Task 3: Rewrite the recipient, encryption and send logic in the script

**Files:**
- Modify: `assets/scripts/send_access_email.ps1` — the `param()` block at lines 20-31, the subject/body at lines 78-96, the recipient block at lines 98-102, the encryption block at lines 104-134, and the decision block at lines 136-173

**Interfaces:**
- Consumes: `-Domain` from Task 2, plus the existing `-Username -EnvTag -Primary -Secondary -Pem -TemplateGuid -Permission -PermissionService -SmimeFlag -EncryptSendKeys`.
- Produces: stdout lines `SENT recipient='<name>' address='<smtp>'` on the send path and `OPEN recipient='<name>' resolved=<bool> domain_ok=<bool> encrypted=<bool>` on every open path. Task 4 documents these.

There is no automated test for this task — the script drives live Outlook COM
and cannot run on Linux. Step 5 is a manual checklist, and it is not optional.

- [ ] **Step 1: Add the `-Domain` parameter**

In the `param()` block, after the `[string]$Pem = "",` line:

```powershell
    [string]$Domain          = "",   # org mail domain; resolved address must match
```

- [ ] **Step 2: Replace the subject and body**

Replace the `$mail.Subject = ...` line and the `$mail.Body = @" ... "@` here-string
(lines 79-92) with:

```powershell
$envUpper = "$EnvTag".ToUpper()
$mail.Subject = "Bastion Access for $envUpper"
$mail.Body = @"
Hello $firstName,

See below for your login credentials and attached is your PEM file.

Username: $Username

Primary Bastion: $Primary

Secondary Bastion: $Secondary

Thanks,
$senderFirst
"@
```

- [ ] **Step 3: Replace the recipient block with resolve + domain check**

Replace lines 98-102 (the `# To: add the display name and resolve.` comment
through the `try { $resolved = ... }` line) with:

```powershell
# To: resolve the display name. Resolve() is TRUE only for ONE unambiguous
# match - two people with the same name return FALSE, as does no match at all.
# We deliberately do not try to tell those two failures apart: the user does the
# same thing either way (pick the right person in an empty To field), and
# counting would need a full GAL enumeration or an LDAP query.
$recip = $mail.Recipients.Add($displayName)
$resolved = $false
try { $resolved = [bool]$recip.Resolve() } catch { $resolved = $false }

# Resolve() also matches the local Contacts folder and the autocomplete cache,
# not just the GAL. A stale personal entry for the same name would otherwise be
# mailed a private key, so a resolved address must sit in the configured domain.
$smtp      = ""
$domainOk  = $false
if ($resolved) {
    try { $smtp = "$($recip.AddressEntry.GetExchangeUser().PrimarySmtpAddress)" } catch { $smtp = "" }
    if (-not $smtp) { try { $smtp = "$($recip.Address)" } catch { $smtp = "" } }

    if (-not $Domain) {
        # No domain configured - check disabled, preserving older behavior.
        $domainOk = $true
    } elseif ($smtp -like "*@*") {
        $addrDomain = ($smtp -split '@')[-1]
        $domainOk = $addrDomain.Trim().ToLower() -eq $Domain.Trim().ToLower()
    }
}
```

- [ ] **Step 4: Encrypt unconditionally, then decide**

Replace lines 104-173 — everything from the
`# --- Apply encryption headless (single-recipient path only) ---` comment to
the end of the file — with:

```powershell
# --- Apply encryption headless ------------------------------------------
# Encryption is applied whether or not the recipient resolved, so a draft that
# opens is already encrypted rather than depending on the Alt+6 keystroke
# landing. Alt+6 is a TOGGLE and is only sent below when this did NOT confirm.
# Preference: RMS/IRM template -> S/MIME flag -> bare Permission value.
$encConfirmed = $false
if ($TemplateGuid) {
    # Proven headless sequence: Permission first, then PermissionService, then
    # the GUID last. Setting the GUID throws "The operation failed" but the value
    # sticks and the template is applied at send time - so we confirm by reading
    # BOTH Permission and PermissionTemplateGuid back, not by the setter's error.
    try {
        if ($Permission -ne 0) { $mail.Permission = $Permission } else { $mail.Permission = 4 }
    } catch {}
    if ($PermissionService -ne 0) { try { $mail.PermissionService = $PermissionService } catch {} }
    try { $mail.PermissionTemplateGuid = $TemplateGuid } catch {}
    if ([int]$mail.Permission -ne 0 -and "$($mail.PermissionTemplateGuid)") { $encConfirmed = $true }
} elseif ($SmimeFlag -ne 0) {
    try {
        $mail.PropertyAccessor.SetProperty($SEC_PROP, $SmimeFlag)
        if ([int]($mail.PropertyAccessor.GetProperty($SEC_PROP)) -ne 0) { $encConfirmed = $true }
    } catch {}
} elseif ($Permission -ne 0) {
    try {
        $mail.Permission = $Permission
        if ([int]$mail.Permission -ne 0) { $encConfirmed = $true }
    } catch {}
}

# --- Decide: send headless, or open for the user -------------------------
# All three must hold. The PEM is a private key; it never leaves unattended
# without a confirmed single recipient in our own domain and confirmed
# encryption.
$sent = $false
if ($resolved -and $domainOk -and $encConfirmed) {
    try { $mail.Send(); $sent = $true } catch { $sent = $false }
}

if ($sent) {
    Show-Box "Email sent successfully to $displayName ($smtp)." "Information"
    Write-Output "SENT recipient='$displayName' address='$smtp'"
    return
}

# Not sent. Clear the To field so nobody is pre-selected, explain why, and open.
try { while ($mail.Recipients.Count -gt 0) { $mail.Recipients.Remove(1) } } catch {}

$reason =
    if (-not $resolved) {
        "Could not identify a single recipient for '$displayName' - either nobody matches or more than one person does.`n`n" +
        "The email is ready below with the To field empty. Enter the correct recipient, confirm it still shows encrypted, then click Send."
    } elseif (-not $domainOk) {
        "'$displayName' resolved to $smtp, which is not in $Domain.`n`n" +
        "The email is ready below with the To field empty. Enter the correct recipient, confirm it still shows encrypted, then click Send."
    } else {
        "The email is ready but encryption could not be confirmed automatically.`n`n" +
        "Applying your Encrypt shortcut now - verify the email shows as encrypted, then enter the recipient and click Send."
    }

$inspector = $mail.GetInspector
$inspector.Display($false)
Start-Sleep -Milliseconds 700
try { $inspector.Activate() } catch {}
Start-Sleep -Milliseconds 250
# Best-effort: apply the QAT Encrypt shortcut on the now-visible window. Only
# when headless encryption did NOT confirm - Alt+6 toggles, so pressing it on an
# already-encrypted item would strip the encryption.
if (-not $encConfirmed -and $EncryptSendKeys) {
    try { [System.Windows.Forms.SendKeys]::SendWait($EncryptSendKeys); Start-Sleep -Milliseconds 400 } catch {}
}

Show-Box $reason "Warning"
Write-Output "OPEN recipient='$displayName' resolved=$resolved domain_ok=$domainOk encrypted=$encConfirmed"
```

- [ ] **Step 5: Update the header comment**

Replace the `# Flow:` block at lines 8-18 with:

```powershell
# Flow:
#   * Compose headless (To/Subject/Body/PEM) - nothing is shown yet.
#   * Resolve the recipient. Resolve() is TRUE only for a single unambiguous
#     match; FALSE for 2+ same-named people AND for no match (we do not try to
#     tell those apart - the user does the same thing either way).
#   * Verify the resolved address is in -Domain. Resolve() also matches local
#     Contacts and the autocomplete cache, so this stops a stale personal entry
#     from being mailed a private key. Blank -Domain skips the check.
#   * Encrypt via the object model using the values features.json supplies
#     (discovered with outlook_verification.ps1). Done whether or not the
#     recipient resolved, so an opened draft is already encrypted.
#   * Send headless only when resolved AND in-domain AND encryption confirmed.
#   * Otherwise clear the To field, open the draft, apply the QAT Encrypt
#     shortcut if encryption did not confirm, and explain why.
```

- [ ] **Step 6: Verify the file is still pure ASCII and balanced**

Run:

```bash
LC_ALL=C grep -n '[^ -~\t]' assets/scripts/send_access_email.ps1 && echo "NON-ASCII - FIX IT" || echo "ASCII clean"
```

Expected: `ASCII clean`

Run:

```bash
python3 - <<'EOF'
s=open('assets/scripts/send_access_email.ps1',encoding='ascii').read()
b=[l for l in s.split('\n') if not l.strip().startswith('#')]
body='\n'.join(b)
print("braces", body.count('{'), body.count('}'), "parens", body.count('('), body.count(')'))
bad=[n for n,l in enumerate(s.split('\n'),1)
     if not l.strip().startswith('#') and l.replace('`"','').count('"')%2]
print("unbalanced quotes:", bad or "none")
EOF
```

Expected: matching brace and paren counts, `unbalanced quotes: none`

- [ ] **Step 7: Manual verification on Windows — REQUIRED**

This cannot be automated. On a Windows box with classic Outlook signed in, with
a real `encrypt_template_guid` and `email_domain` in `features.json`, rebuild
and run a create for each case:

1. A username resolving to exactly one person in the domain — expect **no
   window**, a "sent successfully" box, `SENT recipient=... address=...` on
   stdout. **Open the recipient's mailbox and confirm the encryption banner** —
   arrival alone is not proof.
2. A username whose name is shared by two people — expect the draft to open,
   **To field empty**, "could not identify a single recipient" box.
3. A username matching nobody — expect the same open path and the same message.
4. Temporarily set `email_domain` to a domain the resolved address is not in —
   expect the draft to open, To empty, the "resolved to ... which is not in ..."
   message naming the real address.

In cases 2-4 confirm the opened draft still shows as **encrypted** and that
`Alt+6` did not toggle it off.

- [ ] **Step 8: Commit**

```bash
git add assets/scripts/send_access_email.ps1
git commit -m "Send the access email unattended only to a single in-domain recipient"
```

---

### Task 4: Run the script automatically after a create (Windows)

**Files:**
- Modify: `src/bin/ec2_manager_gui.rs` — add `access_email_args()` next to `build_email_command`, add `launch_access_email()`, call it where `popup.email_cmd` is assigned
- Test: `src/bin/ec2_manager_gui.rs` — `gui::tests`

**Interfaces:**
- Consumes: `AccessEmailConfig.auto_run` and `.email_domain` (Task 1), `-Quiet` (Task 3).
- Produces: `access_email_args(cfg, username, env, primary, secondary, pem) -> Vec<(&'static str, String)>` — the shared ordered flag/value list, consumed by both `build_email_command` and `launch_access_email`. `launch_access_email` returns `Result<std::process::Child, String>` on Windows.

- [ ] **Step 1:** Extract the `args` vector from `build_email_command` into `access_email_args`, returning `Vec<(&'static str, String)>`. `build_email_command` calls it. Test that both still contain every flag (existing tests cover this).
- [ ] **Step 2:** Add `launch_access_email`, gated `#[cfg(target_os = "windows")]`, resolving `send_access_email.ps1` beside `current_exe()`, erroring if absent (no `%TEMP%` fallback), spawning `powershell.exe -NoProfile -ExecutionPolicy Bypass -File <path> <args> -Quiet` with `.stdout(Stdio::piped())` and no `-WindowStyle Hidden`. Non-Windows arm logs and returns `Err`.
- [ ] **Step 3:** Call it where `popup.email_cmd` is set, only when `cfg.enabled && cfg.auto_run`.
- [ ] **Step 4:** `cargo test --features gui`, `cargo build --features gui` (0 warnings), commit.

---

### Task 5: Report the outcome in the result popup

**Files:**
- Modify: `src/bin/ec2_manager_gui.rs` — `EmailStatus` enum, `ScriptResultPopup.email_status`, an `email_tx`/`email_rx` channel pair on the app, the popup rendering, and a `parse_email_marker` helper
- Test: `src/bin/ec2_manager_gui.rs` — `gui::tests`

**Interfaces:**
- Consumes: `launch_access_email` (Task 4) and the script's stdout markers (Task 3).
- Produces: `parse_email_marker(line: &str) -> Option<EmailStatus>`.

- [ ] **Step 1: Write the failing tests** for `parse_email_marker`, one per row of the spec's Part 6 table plus a malformed line returning `None`.
- [ ] **Step 2:** Run them; expect FAIL (function not defined).
- [ ] **Step 3:** Add `enum EmailStatus { Sending, Sent { address: String }, Opened { reason: String }, Failed { error: String } }` and `parse_email_marker`, reading `SENT ... address='...'` and `OPEN ... resolved=/domain_ok=/encrypted=` (PowerShell prints `True`/`False`).
- [ ] **Step 4:** Run them; expect PASS.
- [ ] **Step 5:** Wire it up — worker thread reads the child's stdout to EOF, waits, sends the parsed status (or `Failed`) over `email_tx`; the update loop drains `email_rx` into `popup.email_status`; the popup renders it coloured (green sent, yellow opened/sending, red failed) above the button row.
- [ ] **Step 6:** `cargo test --features gui`, `cargo build --features gui` (0 warnings), commit.

---

### Task 6: Update the documentation

**Files:**
- Modify: `README.md` — the `access_email` JSON sample and the feature-flag table in the "Feature flags (features.json)" section, and the "Access email (post-create)" section
- Modify: `ACCESS_EMAIL_WALKTHROUGH.md` — the "What happens after a user is created" section and its outcome table
- Modify: `CLAUDE.md` — the "Access email (post-create) — copy a command, never auto-send" section

**Interfaces:**
- Consumes: the `email_domain` field from Task 1 and the stdout markers from Task 3.
- Produces: nothing consumed by later tasks.

- [ ] **Step 1: Add `email_domain` to the README feature-flag sample and table**

In `README.md`, in the `access_email` JSON block, add `"email_domain": "xyz.com",`
directly after `"enabled": true,`.

In the feature-flag table, insert this row directly after the
`access_email.enabled` row:

```markdown
| `access_email.email_domain` | `""`      | Your organization's mail domain. A recipient that resolves outside it is never mailed unattended — this is what stops a stale local Contacts entry from receiving the PEM. Empty skips the check. |
```

- [ ] **Step 2: Replace the README behavior description**

In the "Access email (post-create)" section of `README.md`, replace the
paragraph beginning "After a Bastion New User run finishes" with:

```markdown
After a Bastion New User run finishes *and the PEM was saved*, the result popup
grows a **✉ Send Email Command** menu with one entry per terminal (WSL, Git
Bash, PowerShell). Picking one **copies a command to your clipboard** — the app
does not run it. You paste it into your own terminal and run it.

That command invokes `send_access_email.ps1`, which composes the email
(subject `Bastion Access for <ENV>`, the login details in the body, the PEM
attached), then:

| Outcome | What happens |
|---|---|
| The name resolves to exactly one person **in `email_domain`** | Encrypted headless and **sent in the background** — no window |
| Resolves, but the address is outside `email_domain` | Opens the draft, **To field empty**, naming the address it found |
| Does not resolve — nobody matches, or two people share the name | Opens the draft, **To field empty** |
| Encryption could not be confirmed | Opens the draft and applies your `Alt+6` shortcut |

Nothing is ever sent unattended without a confirmed single in-domain recipient
**and** confirmed encryption — the attachment is a private key.
```

- [ ] **Step 3: Update the walkthrough outcome table**

In `ACCESS_EMAIL_WALKTHROUGH.md`, replace the three-row table under
"Running the command:" with:

```markdown
| Case | What happens |
|---|---|
| **One match, address in your `email_domain`** | Encrypts headless and **sends silently** - no compose window. The script pops up "Email sent successfully" and prints `SENT recipient='...' address='...'`. |
| **One match, address outside `email_domain`** | Opens the draft with the **To field empty** and names the address it found. Guards against a stale Contacts entry receiving the PEM. |
| **Two or more people share the name, or nobody matches** | Opens the draft with the **To field empty** so nobody is pre-selected. One message covers both - Outlook cannot tell them apart without a full directory scan. |
| **Encryption could not be confirmed** | Opens the draft and presses your Alt+6 shortcut. Only ever pressed when headless encryption failed, since Alt+6 is a toggle. |
```

Then replace the sentence beginning "The private key is only sent
automatically" with:

```markdown
The private key is only sent automatically when the recipient resolves to one
person, that person's address is in your configured `email_domain`, **and**
encryption is confirmed on the item. Alt+6 is only ever pressed on a
not-yet-encrypted draft (it's a toggle), so it never accidentally un-encrypts.
```

- [ ] **Step 4: Update CLAUDE.md**

In `CLAUDE.md`, in the "Access email (post-create)" section, add these two
bullets to the existing list:

```markdown
- **The domain check is a safety control, not a filter.** `Resolve()` matches
  the local Contacts folder and the autocomplete cache as well as the GAL, so a
  stale personal entry for the same name would otherwise be mailed a private
  key. `access_email.email_domain` requires the resolved SMTP address to be in
  the org's own domain before anything sends unattended. Blank skips the check
  (preserving pre-existing behavior) — that is deliberate, not an oversight.
- **0 matches and 2+ matches share one message on purpose.** `Resolve()` returns
  false for both and cannot distinguish them; telling them apart would need a
  full GAL enumeration or an LDAP query, and the user does the same thing either
  way. Do not add a directory scan to produce a nicer error string.
```

- [ ] **Step 5: Verify the docs are consistent with the code**

Run:

```bash
grep -n "email_domain" README.md CLAUDE.md ACCESS_EMAIL_WALKTHROUGH.md assets/features.json src/features.rs src/bin/ec2_manager_gui.rs
```

Expected: hits in all six files. Confirm the default shown in the README table
(`""`) matches `AccessEmailConfig::default()` from Task 1.

- [ ] **Step 6: Commit**

```bash
git add README.md CLAUDE.md ACCESS_EMAIL_WALKTHROUGH.md
git commit -m "Document the mail-domain check and the new send/open outcomes"
```

---

### Task 7: Full verification pass

**Files:**
- Modify: `CLAUDE.md` — the "Build status" bullet list only

**Interfaces:**
- Consumes: everything from Tasks 1-4.
- Produces: nothing.

- [ ] **Step 1: Run the full suite**

Run: `cargo test --features gui 2>&1 | grep -E '^test result'`
Expected: 308 tests pass, 0 fail (168 lib + 2 new = 170 lib, 3 CLI, 135 GUI).
If the split differs, count them from the output rather than assuming.

- [ ] **Step 2: Confirm the build is warning-free**

Run: `cargo build --features gui 2>&1 | grep -c '^warning'`
Expected: `0`

- [ ] **Step 3: Confirm clippy gained nothing**

Run: `cargo clippy --features gui 2>&1 | grep -E 'generated [0-9]+ warning'`
Expected: `(lib) generated 6 warnings` and `(bin "ec2_manager_gui") generated 15 warnings` — the pre-existing 21. Any increase must be fixed before committing.

- [ ] **Step 4: Confirm the Windows target still cross-compiles**

Run: `CARGO_TARGET_DIR=/tmp/ec2m cargo build --release --target x86_64-pc-windows-gnu --features gui 2>&1 | tail -3`
Expected: `Finished` and exit 0.

- [ ] **Step 5: Update the build status in CLAUDE.md**

Replace the test-count line under "## Build status" with the real number from
Step 1, keeping the rest of the bullet list intact:

```markdown
- `cargo test --features gui` — 308 tests pass, 0 fail (170 lib + 3 CLI + 135 GUI)
```

- [ ] **Step 6: Commit**

```bash
git add CLAUDE.md
git commit -m "Refresh the verified build status"
```

---

## Manual verification still outstanding after this plan

Every Rust-side change is covered by tests. **The PowerShell is not, and cannot
be** — it drives live Outlook COM, which does not exist on this repo's Linux
build host, and a mock would only test the mock.

Task 3 Step 7 is the real acceptance test. Until it has been run on Windows
against a live Outlook with a real template GUID, the send path is **written but
unproven**. Do not describe this feature as working before that.

# CLAUDE.md

## Project overview

Rust-only EC2 + SSM instance explorer with CLI (`ec2_manager`) and desktop GUI (`ec2_manager_gui`, egui/eframe). The GUI embeds a terminal for interactive SSM sessions.

## Working with this repo

When the user asks to revert, restore, or reference a prior change (e.g. "change it back", "like it was before"), always check recent git history first (`git log`, `git show <sha>`, `git diff <sha>~..<sha>`) to see what was actually changed before making edits. Also check uncommitted/unpushed changes (`git status`, `git diff`, `git diff --cached`) since the relevant change may not yet be committed.

## Build commands

```bash
# Development build (library + CLI)
cargo build

# Development build with GUI
cargo build --features gui

# Run tests
cargo test                  # lib + CLI tests
cargo test --features gui   # all tests including GUI (46 GUI tests)

# Clippy
cargo clippy --features gui

# Full release build (Linux + Windows cross-compile)
./scripts/build_binaries.sh         # or: ./scripts/build_binaries.sh all
./scripts/build_binaries.sh linux   # Linux only
./scripts/build_binaries.sh windows # Windows cross-compile only
```

## Build status

As of 2026-08-04 (rustc 1.94.0), with the access-email integration restored and
made automatic, the full build pipeline passes cleanly:
- `cargo build --features gui` — zero warnings (Linux)
- `cargo test --features gui` — 315 tests pass, 0 fail (171 lib + 3 CLI + 141 GUI)
- `cargo clippy --features gui` — no errors; 21 pre-existing style warnings
  (derivable_impls on Mode, too_many_arguments on sim::make_instance,
  collapsible_if / let_and_return / manual_is_multiple_of in the GUI)
- Release targets — zero warnings on both Linux (x86_64-unknown-linux-gnu, via
  `build_binaries.sh`) and Windows (x86_64-pc-windows-gnu, built directly since
  the script aborts without `zip` — see below)
- **Disk space on `D:`.** A full `D:` surfaces as an opaque Cargo failure, not a
  "no space" message: `error: failed to build archive at .../libec2_manager-*.rlib:
  Input/output error (os error 5)`. Check with `df -h /mnt/d` — `WSL/target/`
  alone is ~10 GB. Either `cargo clean`, or build to the Linux filesystem with
  `CARGO_TARGET_DIR=/tmp/ec2m-test cargo test --features gui` (same trick the
  Windows cross-compile already uses for the spaces-in-path problem).
- Packaging the release zips needs `zip`/`unzip` on PATH (`sudo apt install zip`).
  **Without them `build_binaries.sh` aborts, it does not skip the step:**
  `package_linux_zip` calls `require_cmd zip`, which `exit 1`s — so the Linux
  binaries land in `dist/linux/` and the run dies there, *before* the Windows
  target is ever built. If you only need to check that both targets compile,
  build them directly (see the cross-compile note below) rather than running the
  packaging script.

### Known-good rollback point: `pre-email-readd-58a9b9a`

Tag `pre-email-readd-58a9b9a` (annotated) marks the last state verified working
**before** the Outlook access-email integration was re-added — i.e. the state to
return to if the email code turns out to be the CrowdStrike trigger after all.
It sits on the commit that added this note; the code is identical to `58a9b9a`,
which is where the verification below was run. Roll back with:

```bash
git reset --hard pre-email-readd-58a9b9a
```

Everything in the Build status section above was run against that exact commit
with a clean working tree, plus the Windows release cross-compile
(`CARGO_TARGET_DIR=/tmp/ec2m cargo build --release --target x86_64-pc-windows-gnu
--features gui`, exit 0). The tree is confirmed email-free — no hits for
`send_access_email` / `outlook` / `access_email` / `smtp` / `MailItem` outside
`target/`.

**The email code came back via `git revert 0360342` ("Removed email code.").**
That restored `ACCESS_EMAIL_WALKTHROUGH.md`, four PowerShell assets
(`send_access_email.ps1`, `outlook_verification.ps1`, `test_access_email.ps1`,
`test_headless_encrypt.ps1`), the `access_email` block in `features.json`, the
`AccessEmailConfig` struct in `features.rs`, the GUI wiring, and the
`build_binaries.sh` hunk that copies `send_access_email.ps1` next to the GUI exe.

It conflicted in three files — `assets/features.json`, `src/features.rs`,
`src/bin/ec2_manager_gui.rs` — purely because each had grown new sections under
the alerts / Vault IAM / personal-scripts work. Every conflict was additive
(keep both sides); nothing from either side was dropped. The PowerShell assets,
the walkthrough, and the `build_binaries.sh` hunk applied clean.

Earlier email history, if it ever needs untangling again: `5e03e93` (initial),
`3dc603d` (logic), `050a4b9` (switched to the manual copy-a-command button —
this is the version that came back), `ccb3104` (the last pre-removal commit that
*has* the code).

**Why it was removed:** CrowdStrike quarantined the app, so `2c9a8e6` stripped
email as a test. It still quarantined afterward, so email was ruled out as the
cause; `2c31e8c` then reset the branch to the pre-email baseline `c51a397` for a
clean test target. Tag `pre-rollback-2c9a8e6` preserves the old tip.

### Access email (post-create)

Restored on 2026-08-04 by reverting `0360342`, then made automatic again. After
a Bastion New User run *where the PEM was saved*, the GUI spawns
`send_access_email.ps1` itself (`start_access_email`, Windows only), and the
result popup shows a status line fed by the script's stdout marker. The **✉ Send
Email Command** menu remains as a manual fallback that copies a ready-to-run
command per terminal.

- **The send path has three conditions, all required:** the recipient resolves
  to exactly one person, that address is in `access_email.email_domain`, and
  encryption reads back confirmed. Anything else opens the draft. The
  attachment is a private key — do not relax this.
- **The domain check is a safety control, not a filter.** Outlook's `Resolve()`
  matches the local Contacts folder and the autocomplete cache as well as the
  GAL, so a stale personal entry for the same name would otherwise be mailed
  the PEM. Blank `email_domain` skips the check (preserving older behavior);
  that is deliberate, not an oversight.
- **0 matches and 2+ matches share one message on purpose.** `Resolve()` returns
  false for both and cannot distinguish them; telling them apart needs a full
  GAL enumeration or an LDAP query, and the user does the same thing either way
  (pick a recipient in an empty To field). Do not add a directory scan to
  produce a nicer error string.
- **Two EDR constraints in the spawn are load-bearing.** The script is run from
  the file **next to the exe** — never written to `%TEMP%` and run from there —
  and `-WindowStyle Hidden` is not used. Both are patterns EDRs quarantine on
  sight. `build_binaries.sh`'s `package_windows_zip` copies the script beside
  the exe rather than embedding it, for the same reason.
  (History worth knowing: `050a4b9` removed auto-run believing it triggered
  CrowdStrike; `2c31e8c` later found the app was still quarantined with *all*
  email code gone, so email was never the trigger.)
- **`-Quiet` is only for the auto-run path.** It suppresses the script's own
  message boxes because the GUI renders the outcome; the copied manual command
  omits it, since nothing is watching a command pasted into a terminal.
  `the_copied_command_never_carries_quiet` guards this.
- **`access_email_args` is shared** by `build_email_command` and
  `launch_access_email` so the copied command and the spawned one cannot drift.
- Values are interpolated into single-quoted arguments, and the two shells
  escape an embedded quote differently (`'a'\''b'` for bash, `'a''b'` for
  PowerShell). `build_email_command_quotes_apostrophes_per_shell` covers it.
- `EmailStatus` and `parse_email_marker` carry
  `#[cfg_attr(not(target_os = "windows"), allow(dead_code))]` — only the
  Windows build constructs the non-`Sending` variants, and the Linux dev build
  must stay warning-free without weakening the check where it ships.
- `launch_access_email` returns `std::result::Result` spelled out, because the
  crate's own `Result<T>` alias (`src/error.rs`) takes one parameter and is in
  scope. This only fails on the Windows target, so **cross-compile before
  trusting a change here.**
- The `encrypt_*` values are tenant-specific and discovered with
  `outlook_verification.ps1` / `test_headless_encrypt.ps1`; the shipped
  `encrypt_template_guid` is an all-zeros placeholder that must be replaced.
  See `ACCESS_EMAIL_WALKTHROUGH.md`.

### Windows cross-compile and spaces in the repo path

The mingw `dlltool` does not quote the paths it passes to the assembler, so it
fails outright when Cargo's build directory contains a space — as it does when
the repo lives under `/mnt/d/Work Projects/...`. It is invoked for any crate
using raw-dylib imports (e.g. `chrono` → `windows-link`), so this breaks the
whole Windows target with an opaque "dlltool could not create import library"
error.

`build_binaries.sh` handles this: when `ROOT_DIR` contains a space it sets
`CARGO_TARGET_DIR` to a space-free scratch dir (`/tmp/ec2-manager-build/target`)
and copies artifacts out to `dist/` as usual. If you cross-compile by hand,
do the same:

```bash
CARGO_TARGET_DIR=/tmp/ec2m cargo build --release --target x86_64-pc-windows-gnu --features gui
```

## Architecture notes

### Windows embedded terminal

The GUI uses ConPTY (via `portable-pty`) exclusively for embedded terminal sessions. Only PowerShell 7, Windows PowerShell, and Command Prompt are supported as embedded shells on Windows. Git Bash/MSYS2/winpty are **not** used for embedded sessions.

Key functions:
- `filter_embedded_terminals()` in `ec2_manager_gui.rs` — restricts to PowerShell7/WindowsPowerShell/Cmd
- `spawn_pty_session_blocking()` — spawns all sessions via `native_pty_system()` (ConPTY on Windows)
- `pty_command_for_context()` — spawns `aws` directly with SSM args in live mode
- `resize_pty_session()` — propagates resize to both vt100 parser and PTY master

### On-call alerts (Alerts button)

`src/alerts.rs` fetches the Jira Service Management Operations alert feed
(`https://api.atlassian.com/jsm/ops/api/<cloud_id>/v1/alerts`). The GUI's
**Alerts** button (left of Close All, Connections toolbar) opens a window that
polls it every 10s and renders the rows in the user's **local** timezone — the
API reports UTC.

Two things that are easy to get wrong:

- **Account / Environment / App are not top-level fields.** They arrive as
  `"Key: value"` strings inside each alert's `tags` array (`"Account: 1234…"`,
  `"Environment: Dev"`). `Alert::from_raw` splits them out, matching the key
  case-insensitively and splitting on the *first* colon only — otherwise a
  sibling tag like `"Metric: Global-Request-Time-(seconds)"` gets misread.
- **HTTP goes through `curl`,** not a linked HTTP stack (consistent with how the
  app shells out to `aws`). Credentials are written to curl's **stdin**
  (`-K -`), never argv, so the API token never appears in the process list.
  `curl.exe` ships with Windows 10 1803+.

Config lives in the `alerts` section of `assets/features.json` (compiled in):
`cloud_id` + `email` identify the site, and `allowed_users` is the list of OS
usernames that see the button (`["*"]` = everyone, `[]` = nobody; the shipped
default is `[]`, so the button is hidden until an admin opts users in). The
button also stays hidden when `cloud_id`/`email` are blank — it fails closed,
like `allow_delete_user`.

**Do not put a real token in features.json** — that file is committed. Leave
`token` empty and have each user export `JIRA_TOKEN`, which always overrides it.

`assets/scripts/alerts_10min.sh` is the standalone bash equivalent (curl + jq,
same tag parsing, same local-time conversion) for terminal use.

### Scripts menu: personal scripts, default scripts, git PAT

**Add Script** in the Scripts menu is available to **everyone**. A personal
script is a name, an optional hotkey, and a shell body; entries render below
the built-in Bastion New User / Bastion User Delete ones (which run
`create_new_user.sh` / `delete_user.sh`), each with ✏ (edit)
and ✖ (delete, with an "Are you sure…" confirm). Picking one — or pressing its
hotkey — pastes the body into the **focused connection tab** via the existing
`paste_to_connection_tab` drip-feed. No bastion dialog, no `sudo su`.

`personal_scripts.allowed_users` in `assets/features.json` gates the **git
integration only**, not Add Script:

- **Default scripts.** `personal_scripts.default_scripts` (name, hotkey, body)
  are hardcoded scripts handed to allow-listed users — e.g. a **Ctrl+1**
  "re-clone the repo" script. They show above the personal scripts, are
  read-only (no ✏/✖), and live in features.json, not config.ini. On a hotkey
  collision they win over a personal binding (`poll_script_hotkeys` checks
  them first; the editor's clash check refuses to bind their keys). Users off
  the allow-list get none, and can bind Ctrl+1 to their own script instead.
- **`{{user}}` placeholder.** `expand_script_placeholders` replaces `{{user}}`
  (and `{{USER}}`) in a body with the local OS username before pasting —
  applied to every script run, default or personal. Needed for per-user paths
  like `/home/efs/{{user}}`: the remote `$USER` is the SSM/root account, not
  the person at the keyboard, so the substitution must happen locally.
- **Git PAT prompt.** Allow-listed users are prompted for a PAT on first
  launch (and via the "Git PAT" ✏ row in the menu). Cached in config.ini.
- **Prep Terminal populates git's credential store.**
  `git_credential_store_command` enables `credential.helper store` and writes
  `~/.git-credentials` with `https://USER:PAT@<git_host>` (chmod 600), so
  `git clone`/`git pull` over HTTPS authenticate **without prompting** and it
  persists across sessions. It is self-healing: each prep drops any stale line
  for that host and re-adds the current PAT (a rotated token replaces the old
  one), preserving other hosts' lines. `git_host` defaults to `github.com`.
  Token hygiene: leading space + `HISTCONTROL=ignorespace` keeps it out of the
  remote history, the trailing `clear` wipes it off screen. The built-in user
  scripts pass `None` — they run as root and don't touch git.

**Storage in `config.ini`:** `personal_script=<b64 name>|<hotkey>|<b64 body>`
and `git_pat=<b64>`. Name/body are base64'd because the file is line-based and
both may contain `|`, `=` or newlines; the PAT is base64 — obfuscation, **not**
encryption.

**Hotkeys** need Ctrl or Alt (or a function key) — `Hotkey::is_bindable`
enforces this, since a bare letter binding would swallow ordinary typing in
the terminal. They only fire while the app owns OS focus (`ctx.input(|i|
i.focused)`), and `poll_script_hotkeys` runs before any panel so
`hotkey_consumed_frame` can suppress the key press for
`forward_terminal_key_input`.

**git auth failures re-prompt.** `looks_like_git_auth_failure` scans PTY
output for the usual rejections ("fatal: Authentication failed", "HTTP Basic:
Access denied", …) and raises the PAT dialog, rate-limited to once per 30s so a
failing `git pull` doesn't reopen it per line. After updating the PAT, click
Prep Terminal to rewrite the stored credential.

### Scripts dialogs select an environment, not an account

Several AWS accounts host **two environments**, told apart by each instance's
`MMODAL_ENV` tag. All three Scripts entries (create_new_user, delete_user,
Vault IAM Access) therefore select an *environment*, and narrow their bastion
dropdowns to instances carrying that tag value.

`src/script_env.rs` builds the dropdown rows as the **union** of the
environments declared in `accounts.json` (`environments: [{name, vault_addr}]`)
and those discovered from the tags in the account's inventory. Three cases that
are easy to break:

- **Declared spelling wins** on a case-insensitive collision, so a `DEV1` in
  accounts.json and a `dev1` tag are one row, labelled as the admin wrote it.
- **Rows are labelled with the environment name alone**, not `Account — ENV`.
  Two accounts sharing an `MMODAL_ENV` value therefore render identically;
  `account_id` still distinguishes them internally. The two Vault dialogs
  uppercase the label via `vault_env_label` (display only — matching and
  `vault_addr` lookup are already case-insensitive); the whole-account row is
  exempt, since it names an account rather than an environment.
- **An account with nothing declared and nothing tagged** yields one row with
  `env: ""` labelled with the *account* name, which applies **no** environment
  filter — the pre-existing whole-account behavior. Do not "fix" that empty
  string away.
- **Exclude Env (`hidden_envs`) filters the list.** An account whose
  environments are *all* excluded contributes **no rows**; it must not collapse
  back to the `env: ""` whole-account row, which would re-expose every bastion
  the user just hid. The untagged row is exempt — it has no name to match.

The environment filter is never relaxed: `bastion_combo_ui`'s fallback to the
`"bastion"` substring applies *within* the selected environment, so an
environment with no matching bastion shows an empty dropdown rather than
another environment's boxes.

**Cache.** `bastion_pair.<account_id>.<env>` in config.ini, shared by all three
scripts. Reads fall back to the legacy `bastion_pair.<account_id>` key so
existing selections survive; an empty `env` normalizes back to that key.
Because an inherited pair can name a box from elsewhere,
`retain_available_bastion` clears any id not present in the selected
environment — otherwise the dialog opens pre-aimed at the wrong hosts.

### Vault IAM Access (Scripts menu)

`src/vault_iam.rs` builds the commands that create a Vault policy and an AWS
auth role bound to an IAM role; the GUI file holds only the dialog and the
run/verify wiring. Gated by `vault_iam.allowed_users` in features.json
(`["*"]` shipped; the `Default` is empty so a malformed file fails closed).

Unlike the user scripts it runs on the **primary bastion only** — Vault is a
shared server, so a second identical write is redundant — and as the
**logged-in SSM user**, with no `sudo su`, since Vault authenticates by token.
The secondary is used only if the primary session won't open.

Three things that will silently break if changed:

- **The verdict sentinel is assembled at runtime** (`then v=OK; else v=FAIL;
  fi; echo "__VAULT_IAM_${v}__"`). The shell echoes every command before
  running it, so a literal `__VAULT_IAM_OK__` in the command line would make
  the screen scan match the echo and report success no matter what Vault did.
  `parse_verdict` checks FAIL first, and a missing marker is `Unknown` —
  never treated as success.
- **The policy body ships base64-encoded**, like create_new_user.sh, so
  multi-line HCL survives the line-at-a-time drip-feed.
- **The token ships base64-encoded too**, with the export sent under a leading
  space + `HISTCONTROL=ignorespace` and a `clear` immediately after — before
  the read-back output, so the verification stays readable. `clear` only wipes
  the visible screen; the encoded value can remain in that tab's scrollback,
  the same tradeoff the git PAT flow accepts.

ARNs are validated rather than escaped: anything containing quotes, `$`,
backticks or whitespace is rejected, since it is interpolated into a
double-quoted shell argument.

`vault_addr` resolves environment-level → account-level → blank, via
`accounts::vault_addr_for`. `ProfileConfig` is deliberately not extended with
it; it flows into config.ini persistence and the tab UI, neither of which needs
Vault settings.

**Vault IAM Delete** is the same modal with a `delete` flag (the ARN and
policy-body boxes are hidden), driven by `VaultIamDeleteRequest`. It runs
`vault delete auth/aws/role/<name>` + `vault policy delete <name>`, lists what's
left, and its verdict test is the **negation**: OK means neither object reads
back. Both requests share `connect_steps` and `verdict_step`, so the token
hygiene and the marker mechanism cannot drift between them — keep it that way.

Its gate is `vault_iam.delete_allowed_users`, which **ships empty and is ANDed
with `allowed_users`** (`vault_iam_delete_enabled_for`): being able to create
must not imply being able to delete. The delete always removes the policy too,
so pointing it at a policy shared by another role takes that role's policy with
it. Deleting something already absent is fine — the verdict checks end state,
not the delete's exit code.

### cfg gates for imports

Test-only imports (`std::process::{Child, Command, Stdio}`) are gated with `#[cfg(test)]` to avoid unused-import warnings on both Linux native and Windows cross-compile targets. Similarly, `shell_plan()` is `#[cfg(test)]` since it's only used in tests.

## Key directories

- `src/` — Rust library and binaries
- `src/bin/ec2_manager_gui.rs` — GUI binary (requires `--features gui`)
- `scripts/` — Build and test helper scripts
- `dist/linux/` and `dist/windows/` — Release artifacts
- `windows-vm/smoke/` — Windows VM smoke test harness

## Smoke tests

```bash
# Shell script assertion tests (run locally without VM)
./scripts/test_run_windows_gui_smoke_test.sh
./scripts/test_windows_gui_smoke_compose.sh
./scripts/test_run_windows_vm_test.sh

# Full Windows VM smoke test (requires Docker + KVM)
./scripts/run_windows_gui_smoke_test.sh
```

The Windows smoke test uses PowerShell (`default_terminal=powershell`) in the guest VM.

## Troubleshooting

### Multi-line paste — only first command runs

Symptom: user pastes N commands from Notepad, all N lines appear echoed in the terminal, but `history` shows only the first (and occasionally 2–3 fast ones); subsequent lines never execute.

Root cause: the drip-feed in `paste_to_connection_tab` previously slept a fixed delay between lines. If the remote shell is still running the previous command when the next `\r` arrives, the TTY line discipline hands the queued bytes to that running command's stdin (where they're discarded) instead of letting bash read them as new commands. Echo happens at the kernel level regardless, so everything looks pasted.

Fix in place (commit on `brandons_changes`): `PtySession.prompt_ready: Arc<(Mutex<u64>, Condvar)>` — a counter that bumps each time the parser sees a fresh shell prompt at the cursor line (bare `$ `/`# ` from the custom PS1, or any `user@host…$/#`). The paste worker snapshots the counter, writes one line, and waits on the condvar (2.5s timeout fallback) before the next line. Prompt-detection lives next to the existing `cursor_is_user_host_prompt` check in the output-processing loop.

If this regresses: verify the PS1 still ends with `$ ` / `# ` (set by `PREP_TERMINAL_COMMAND`), and check the prompt-ready signal site fires by greping debug logs for prompt redraws right after pasted-line output.

### Terminal selection / copy-paste — intermittent

Symptoms (cluster together): highlight appears but right-click copy is empty (next paste returns previous clipboard content), AND double-click word selection silently produces nothing. Keyboard input still works.

Suspect helpers, all in `src/bin/ec2_manager_gui.rs`:
- `pixel_to_grid_cell` — depends on `sel_cell_w/h`, computed from `term_rect` ÷ grid dims. If grid dims are stale, mouse → cell mapping is off and extraction reads wrong row.
- `set_scrollback_for_top_abs_row[_checked]` — vt100 silently clamps if `top_abs_row` exceeds the 2500-line scrollback cap. The `_checked` variant returns `None` on clamp; `extract_selection_text` and the double-click path both use it.
- `find_word_bounds` — returns `(col, col)` on a non-word char, which the double-click handler now logs explicitly.
- `extract_selection_text` — returns `""` on clamp; right-click copy preserves the clipboard on empty extraction (intentional — avoids "right-click ate my clipboard"), which is why a failed copy looks like "paste returned previous content".

Hardening already in place: cell size is computed from `parser.screen().size()` (live grid), not `session.last_size` (which lags during resize).

Diagnostic logs already present — to repro & diagnose, ask the user to capture log lines from the affected tab and look for:
- `primary_pressed tab=… pos=… rect=… cell=…x… live=(RxC) last_size=… cell=(row,col) abs=… scroll_off=…` — TRACE level. Shows whether pointer→cell mapping is sane and whether `live` vs `last_size` disagree.
- `double-click tab=… cell=(row,col) abs_row=… screen_row=… actual_sb=… word=(start..end) row="…"` — DEBUG. The `row=` preview tells you whether the parser returned the row the user actually clicked on.
- `double-click tab=… abs_row=… clamped (past scrollback cap)` — WARN. Selection was past the 2500-line buffer.
- `right-click copy tab=… coords=(abs …) vr=… raw_len=…` — existing DEBUG line; `raw_len=0` means extraction came back empty.
- `extract_selection_text: start.abs_row=… clamped …` — eprintln, shows up on stderr.

When triaging next session, ask the user for the log block around the failed click and walk it top-down: live grid vs last_size → cell mapping → abs_row → screen_row → row preview → word bounds / raw_len.

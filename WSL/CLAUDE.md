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
- `cargo test --features gui` — 356 tests pass, 0 fail (174 lib + 3 CLI + 179 GUI)
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

- **The send path requires all of:** one identified recipient, that entry being
  a real Exchange **directory user**, its address in an
  `access_email.email_domains` entry, the address matching
  `email_local_format`, and encryption reading back confirmed. Anything else
  opens the draft. The attachment is a private key — do not relax this.
- **`Recipient.Resolve()` is not a reliable ambiguity test.** It returns `True`
  for a name several people share, quietly taking the nickname/autocomplete
  entry. Preferred gate is an LDAP **ANR** query (`(anr=<name>)`) — the same
  resolution behind Outlook's suggestion list — requiring exactly one match,
  after which the mail is addressed **by SMTP**, never by display name.
- **But LDAP is unavailable on an Entra-ID-only machine, and that is normal.**
  `DirectorySearcher` cannot even bind with no on-prem AD (`dsregcmd` shows
  `DomainJoined : NO`); Outlook still works because it reaches Exchange Online
  over HTTPS, which says nothing about LDAP. So `matches=-1` **falls back** to
  Outlook resolution rather than disabling the feature — an earlier version
  failed closed here and would have sent nothing at all on such a machine.
  What keeps the fallback safe is `dir_user`: a local Contact or saved one-off
  address is refused outright, which is the specific path by which a stale
  personal entry receives a private key.
- **The Outlook GAL is not a substitute counter.** Measured at ~152,000 entries
  on a real tenant — scanning it per send is not viable.
- **`email_domains` and `email_local_format` are layered, not redundant.** The
  domain check catches an out-of-org address (a stale local Contact); the local
  format catches an *in-domain* address belonging to a different person with a
  similar name — `test.user` must resolve to `tuser@`/`tuser2@`, not
  `testuser@`. Blank disables either check.
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

### Open in VS Code (right-click) — how the wrong login sneaks back in

`src/ssh_config.rs` writes a managed Host block into the quarantined
`config.d/ec2-manager` include and launches `code --remote ssh-remote+<alias>`.
Four things keep the connection on the login the user actually picked; each
was a real "it connects as ec2-user and gets permission denied" bug:

- **The alias carries the login** — `<name>-<user>-<instance-id>`, built by
  `managed_alias`. VS Code keys a remote window on the alias alone (recent
  hosts, its server install, cached connection details), so one alias reused
  for two logins lets the earlier session's user resurface. One alias per
  (instance, user).
- **`compose_managed_file` replaces by (HostName, User), not by alias**, so a
  renamed instance collapses into one entry, a *different* login on the same
  box keeps its own entry, and a block written before the alias carried the
  user is dropped. That last rule is what clears the stale entry left by
  earlier versions — without it the old alias lingers and stays clickable.
- **The `Include` must precede every Host/Match block.** ssh keeps the *first*
  value it obtains for each keyword, so an include sitting under a `Host *`
  that sets `User` never gets to set the login, no matter how correct the
  managed block is. `compose_config_with_include` hoists an existing include
  rather than assuming that "present somewhere" is good enough. It returns
  `None` when nothing needs moving, and is idempotent.
- **The dialog's "Open folder" follows the SSH user** while it is still that
  login's default home (`default_remote_dir`/`remote_dir_owner` in the GUI),
  and warns when it names someone else's. Editing only the user field and
  launching into `/home/ec2-user` is its own permission-denied, distinct from
  the ssh one. Only `ec2-user` has a local `/home/ec2-user`; every account the
  Bastion New User script creates lives at `/efs/home/<user>` on the shared
  mount, so that is the default for any other login. The box stays editable
  and a hand-typed path stops tracking the user field.

**The pem, the login and the "don't ask again" opt-out are cached per
(account, `MMODAL_ENV`)**, not per account — `AppConfig::vscode_key`, the same
`<id>.<ENV>` shape as `bastion_key`, upper-cased because the tag is free text.
Resolution layers instance → environment → account, so an environment with no
override still inherits the account-wide value the Settings dialog writes
(that dialog is deliberately account-level and passes an empty env).

The opt-out is the one thing that does **not** fall back to the account entry
when the instance carries an environment: opting out of the prompt for DEV1
must not silently opt out of DEV2, whose key and login differ. An untagged
instance keys on the bare profile id, which is also what older builds wrote,
so an existing opt-out keeps working there. The Settings dialog's "Ask which
key to use again" checkbox is the only in-app way to undo one —
`clear_vscode_prompt_suppression` drops the account key and every `<id>.<ENV>`
key under it.

**Launch with `code --folder-uri vscode-remote://ssh-remote+<alias><path>`,
never `code --remote ssh-remote+<alias> <path>`.** The positional form leaves
VS Code to guess whether the path is a file or a folder, and it cannot stat a
remote path to find out; the guess lands on "file", the window comes up
connected but empty — the "Open Folder / Clone Repository" buttons — and the
folder is silently dropped. `remote_folder_uri` builds the explicit form and
percent-encodes the path, since Open folder is a free-text field.

The folder in that URI **is** the workspace root: VS Code opens with the
Explorer rooted there. A path that does not exist (or that the login cannot
read) still opens the window, just with an error instead of a tree, which is
why the wrong-owner warning is worth having.

Unrelated but easy to misread as a failure: Remote-SSH asks "Select the
platform of the remote host" once per alias it has never seen, and stores the
answer in the user's `remote.SSH.remotePlatform` setting. Since the alias now
carries the login, that prompt appears once per (instance, user) rather than
once per instance. It does not affect which folder opens.

Prefill precedence also matters: a user saved for the account wins over one
scraped from the ssh config, because `scan()` sees our own managed blocks and
an older session's login would otherwise keep re-suggesting itself.

The pem dropdowns render `config.sorted_pem_library()` (alphabetical by
filename, path as tiebreak; storage order untouched) through
`pem_library_combo_items()`, shared by the launch dialog and Settings so the
two cannot drift. Rows are labelled by `pem_row_labels()`: the filename
alone, or the **full path** when another key in the library shares that
filename (compared case-insensitively) — two rows reading `bastion.pem`
identify neither, and a hover tooltip is not an answer. **`ScrollBarVisibility::AlwaysVisible` on its own shows
nothing** — two egui details defeat it, and both are load-bearing:

- `ComboBox::show_ui` wraps its contents in a `ScrollArea` of its own, capped
  at `spacing.combo_height` (200). A taller scroll area nested inside gets
  clipped by it, and the inner bar is drawn past the bottom edge where you
  have to scroll to find it. Callers pass `.height(PEM_COMBO_POPUP_H)`, kept
  well above `PEM_LIST_H`, so the outer one never has anything to scroll.
- The default `ScrollStyle` is `floating()`, whose `dormant_handle_opacity`
  and `dormant_background_opacity` are both `0.0` — the bar exists but is
  fully transparent until hovered. `ScrollStyle::solid()` is opaque
  (`handle_opacity` is hard-coded to 1.0 for non-floating bars) and reserves
  its own width.

### Port forwards (Open in VS Code)

`src/forwards.rs` turns the compiled-in `assets/forwards.json` plus the
machine's hosts file into the `LocalForward` lines in the managed Host block.
Environments key on the `MMODAL_ENV` tag, like everything else.

- **The hosts file is read, never written.** Verified 2026-08-06: an
  unelevated process is denied write access to
  `C:\Windows\System32\drivers\etc\hosts`, most corporate users are not local
  admins, and programmatic writes to that path are EDR-flagged behaviour —
  expensive for an app with a CrowdStrike quarantine in its history. The path
  is configurable (`forwards_hosts_file` in config.ini) so a user can point at
  whatever copy they keep.
- **Matching is by endpoint, never by comment.** Which endpoints belong to an
  environment comes from forwards.json; the hosts file is searched by DNS name
  wherever that name appears. Plenty of hosts files are a bare list of
  `IP name` lines with no section comments at all, and those users must get
  the same result as the ones who annotate. A section comment is only ever
  *additive*: an entry under `# AUCT` that forwards.json does not declare is
  offered too, since the user has said it belongs there.
- **Where the user's hosts file resolves a name, the forward binds that IP**
  (`ForwardSource::HostsIp`), not the one in forwards.json. That is what lets
  an existing setup work untouched, and it closes a hazard: if forwards.json
  names an IP the user already points a *different* name at, binding it would
  silently hijack theirs.
- A name mapped to two different addresses uses the first, as the file itself
  resolves, and logs a warning — a stale line above the live one would
  otherwise bind an address the machine does not resolve the name to, which
  looks exactly like a broken tunnel.
- `parse_hosts` tolerates `IP:port name:port`, and such a port beats the name
  rules. The system hosts file cannot carry one (Windows' DNS client rejects
  the line), so this only applies to a private endpoint list a user points at
  via `forwards_hosts_file`. In the normal case ports come from `port_rules`.
- **A missing hosts entry is not a failure.** The tunnel resolves its remote
  name on the bastion, so it works addressed by IP; the hosts entry only lets
  the user type the name in a browser. Those rows are flagged and
  `hosts_snippet` offers the lines to paste.
- **`port_rules` are first-match-wins** — documented, not incidental. A name
  like `kafka-postgres-proxy` takes the port of whichever rule is listed
  first. Do not sort that list.
- **A malformed forwards.json yields no forwards, never an error.** Forwards
  are a convenience; a bad config must not block a VS Code launch.
- Section headers in a hosts file are comments whose text is a **single
  word** (`# AUCT`). Real hosts files are full of prose comments and none of
  them should become a phantom environment.
- Unticked forwards persist per environment as the set of **disabled** names
  (`forwards_disabled.<id>.<ENV>`), so a forward added to forwards.json later
  arrives switched on rather than silently absent.
- `ServerAliveInterval 30` is in every managed block: an SSM tunnel carrying
  an idle forward gets dropped without keepalives.

### Background port-forward tunnels

`src/tunnel.rs` runs one hidden `ssh` per environment (`CREATE_NO_WINDOW`),
holding that environment's forwards open independently of VS Code. The **Port
Forwards** button (left of Close All) opens the window that manages them. On
by default per environment; the opt-*out* is stored (`forward_ports_off.<id>.<ENV>`).

- **The tunnel owns the forwards, so the managed block carries none** while it
  is on. Both would bind the same ports and whichever connected second would
  fail on every one, decided by timing. Switching the tunnel off for an
  environment puts the `LocalForward` lines back in that block.
- **Every environment's tunnel runs at once**, so two environments claiming
  the same local `ip:port` is a hard conflict. `forwards::collisions` finds it
  at startup and the clashing forward is dropped from the later environment —
  with `ExitOnForwardFailure=yes` the failed bind would otherwise kill that
  whole tunnel, invisibly.
- **`ExitOnForwardFailure=yes` is deliberate.** The window is hidden, so a
  half-forwarded session that keeps running looks exactly like a working one
  until something fails to connect much later.
- **An unauthorized account is not an error, it is a wait.** `start_port_tunnel`
  refuses to spawn without `AuthStatus::Ok` (a hidden process dying on a
  credentials error is invisible), and `poll_port_tunnels` starts it the moment
  the account is authorized — the user does not have to return to the window.
  The poll only logs when a reason *changes*, so a permanently unauthorized
  environment does not fill the log.
- **stderr is captured** (bounded to 50 lines) and `Drop` kills the child.
  Both are load-bearing for an invisible process: nothing else explains why a
  tunnel died, and an ssh session outliving the app is one the user cannot
  find. The GUI also calls `stop_all_port_tunnels` on close.
- **There is no remote shell** (`ssh -N`). A shell only adds a `TMOUT` that
  logs an idle session out however healthy the connection is, and a stdin
  that anything written lands in. `ServerAliveInterval=30` keeps the
  transport up and `poll_port_tunnels` restarts whatever dies — both work on
  a session with nothing to keep busy. (An earlier version held a shell open
  with a newline every 60s; `-N` removes the thing that defended against.)
- **A tunnel that was up and drops is logged at error level** and recorded in
  `tunnel_failures` with a count. The record survives the restart on purpose:
  a session that drops and recovers inside the 15s poll would otherwise leave
  no trace on screen, and these processes are invisible.

### Instance search

`filter::searchable_text` is the *whole* haystack for the search box —
instance id, Name, private IP, **private DNS**, AMI id, and every tag key and
value. A column visible in the table but missing here reads as "search is
broken": private DNS was absent, so `ip-10-1-2-3` only ever matched the few
boxes with that string in a Name tag. Add the field here when adding a column.

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

### Bastion User Restore (Scripts menu)

Restore issues a new key to a user who **already exists** — someone who lost
their PEM. It is a variant of create, not a third pipeline: `UserScriptMode`
picks between the three at the dialog/`enqueue_user_script` boundary, and
everything downstream (drip-feed, secondary mirror, SSH verification, PEM
pull, access email) is the create path unchanged. Only the run line and the
wording differ. Delete is the genuinely separate path, and the code below
`enqueue_user_script` still keys on `delete: bool` for that reason.

`create_new_user.sh --restore` differs from a plain run in exactly four ways:

- **Refuses a user who does not exist**, before writing anything. Without it a
  typo'd username would quietly create a half-configured account instead of
  saying the name was wrong.
- **Replaces `authorized_keys`** rather than appending. The point of a restore
  is that the previous key is unaccounted for, so leaving it authorized
  defeats the exercise.
- **Implies `--force`**, since the original run left a PEM at the default path
  and refusing it would fail every restore.
- **Never passes `--sudo`**, so sudoers is untouched and an existing grant
  survives.

**Restore refuses the `protected_users` list**, which previously only gated
delete. Restore replaces `authorized_keys`, so pointed at a shared account
like `ec2-user` it would revoke the key everyone uses.

Every Scripts menu row carries hover text saying what it does; the personal
and default script rows show a trimmed preview of the body itself
(`script_hover_text`), since a name like "prep" says nothing about what is
about to be pasted into a live shell.

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
  `account_id` still distinguishes them internally. **All** the Scripts dialogs
  uppercase the label via `script_env_label` — the shared Bastion New User /
  Bastion User Delete dropdown (`cnu_env`) and both Vault dialogs — so one
  environment reads the same everywhere. Display only: matching an instance to
  an environment and looking up its `vault_addr` are already case-insensitive,
  and `row.env` keeps the tag's real casing. The whole-account row is exempt,
  since it names an account rather than an environment.
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

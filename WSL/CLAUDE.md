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
  The console window is still suppressed, by `CREATE_NO_WINDOW` on the spawn —
  an ordinary Win32 process-creation flag, and what every other child process
  here already uses (`run_fed_script`, `open_in_browser`, the tunnels). That is
  the substitution to keep: no console *and* no flagged PowerShell switch.
  Until 2026-08-13 this one spawn lacked the flag, so a PowerShell window sat
  over the result popup for the length of the Outlook run.
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

### Port Forwards: the Login dialog, and bastion failover

**The Setup button is on every row, not only broken ones.** The case it
exists for is a setup that worked and stopped — a terminated bastion, a
deleted login, a rotated key — which looks identical to a working row until
the tunnel dies.

- **It writes the keys everything else reads**, via
  `AppConfig::set_port_forward_login`: the pem and login are what Open in VS
  Code resolves, and the bastions are the pair the Scripts dialogs share.
  Changing the primary re-aims Bastion New User, Bastion User Delete and
  Vault IAM, which is why that change is logged at warn and stated in the
  dialog. An empty secondary is stored as empty, never refused — plenty of
  environments have one box.
- **Under WSL the tunnel runs the Windows ssh client** (`src/wsl.rs`). WSL2
  has its own network namespace, so `ssh -L 127.200.20.4:443 …` started by
  the Linux client binds *WSL's* loopback and no Windows browser can reach
  it. WSL2's localhost forwarding only mirrors `127.0.0.1`, never the
  distinct `127.200.x` addresses the forwards rely on to give each service
  its own name. The failure looks exactly like success from inside the app —
  process alive, binds succeeded, nothing ever arrives, so not even stderr.
  - The Windows client cannot use the managed block: it holds WSL paths and
    lives where that client does not look. So a second copy is written with
    the pem translated to `C:\…` and handed over with **`-F`**, which also
    leaves the user's hand-maintained Windows ssh config alone.
  - **Credentials need no bridging**: `wsl_setup` symlinks the WSL AWS
    directory to the Windows one, so the `aws ssm start-session` in the
    ProxyCommand authenticates either side. `ProxyCommand` names bare `aws`,
    which the Windows client resolves on the Windows PATH.
  - **A key on the WSL filesystem is warned about, not blocked.** Windows
    OpenSSH refuses a private key whose permissions it cannot vouch for, and
    a file reached over `\\wsl.localhost` does not present the ownership it
    wants. `pem_is_windows_native` drives that warning; a key under
    `/mnt/<drive>/…` avoids the question.
- **Each row has a scrollable "session output" pane** showing the hidden
  ssh process's stderr **live**, not only once it dies. This is the one
  place a specific failure mode is visible: every local bind succeeds — so
  the session is up, the status is green, and nothing looks wrong — while
  every *remote* dial is refused, because ssh only connects `host:port` on
  the bastion when a forward is actually used. ssh reports that as
  `channel N: open failed: connect failed: …` on stderr and nowhere else.
  `tunnel_stderr` keeps the last session's output after it dies, since the
  `Tunnel` (and its buffer) is dropped at that point. The pane sets
  `ScrollStyle::solid()` and `AlwaysVisible` for the reason recorded under
  the pem dropdown: the default floating bar has `dormant_*_opacity` of
  0.0, so it is invisible until hovered and the pane reads as unscrollable.
- **`src/probe.rs` verifies a forward end to end.** Surviving
  `TUNNEL_PROVEN_AFTER` proves ssh **bound** the local ports; it proves
  nothing about the far side, because ssh only opens the remote connection
  when a forward is first *used*. So a listener can sit there looking healthy
  and fail the moment a browser hits it. One `curl` through the tunnel
  settles it, and the row then says `verified` or `nothing answering through
  it` rather than implying either.
  - **The endpoint is the environment's `vault_addr`**, and an environment
    without one is `Skipped`, not failed — the check is opt-in by
    configuration. Vault must also be among that environment's forwards, or
    the request would leave via the machine's own network and "verify"
    something this tunnel has nothing to do with.
  - **Reaching the server is the test, not the reply.** Vault answers
    `/v1/sys/health` with 429/472/501/503 when standby, DR, uninitialised or
    sealed — all working forwards. Even a TLS complaint proves bytes crossed.
    Only a connect failure or timeout means `Unreachable`. `-k` is set
    because this is reachability, not authentication.
  - **`--resolve` pins the name to the local bind**, so the probe works with
    no hosts entry (most users have none) while keeping the hostname in the
    URL for SNI and `Host`.
  - **curl failing is `Inconclusive`, never `Unreachable`.** Reporting "not
    forwarded" because curl is missing would send someone to debug a healthy
    tunnel.
  - Runs **once per session**, cleared whenever the tunnel is replaced, so a
    restart or failover re-verifies. It is a `curl` per tunnel lifetime, not
    per poll.
- **A bastion dropdown with exactly one candidate selects it**
  (`sole_bastion_candidate`). It counts candidates *after* the dropdown's own
  filter via the shared `bastion_candidates`, or it would pick something the
  list does not show. It only fills an **empty** field — a saved selection,
  including a stale one, is left alone, since silently replacing a terminated
  bastion hides what the dialog flags. The secondary excludes the primary
  from its pool: an environment with one bastion gets a primary and no
  failover, which is the correct answer.
- **The "all forwarding" toolbar line is green and transient.**
  `ScriptState` gained an `Ok` variant (solid green, no flash) because the
  healthy state was being drawn in the flashing yellow of work in progress,
  so a tunnel up for minutes read as one hanging for minutes. It then hides
  itself after `TUNNEL_OK_BANNER` (60s): a permanent banner announcing that
  things work is noise on a bar whose job is to carry problems. It shows
  again on recovery, which is what `tunnel_ok_since` being cleared on any
  failure buys. Expiry is judged **at render**, not in
  `refresh_tunnel_status` — that only runs on the 15s poll, so the banner
  would overstay by up to that long — and the toolbar requests a repaint at
  the expiry instant rather than waiting for something else to redraw.
  `has_clearable_status` still counts only `Failed`, so the green line has
  no ✖: it is not something to dismiss.
- **The tunnel can never stop to ask a question**
  (`StrictHostKeyChecking=accept-new`, `BatchMode=yes`). It is spawned with
  `CREATE_NO_WINDOW` and a null stdin, so a prompt is a *hang*, not a
  failure. Every instance id is a new host name, so the first connection to
  one hits ssh's default `ask` and stops dead on "The authenticity of host …
  can't be established" with nobody to type yes — sitting alive,
  authenticated to nothing, binding nothing, writing nothing. From outside
  that is indistinguishable from a healthy tunnel, and it is what made a
  whole afternoon's forwarding silently do nothing while the window reported
  `up 2m · 9 forward(s)`. Running the same command by hand works precisely
  because a console can answer. **`accept-new`, never `no`:** an unknown
  host is trusted on first sight, as an interactive user would, but a
  *changed* key is still refused.
- **"Working" is answered by `Tunnel::is_bound`, not by process liveness.**
  A session behind the SSM `ProxyCommand` will sit alive indefinitely
  without ever finishing its connection — binding nothing, writing nothing,
  exiting never — and `is_running`, uptime and `ExitOnForwardFailure` all
  report that as healthy. Only asking the listener distinguishes it, so the
  poll TCP-connects to the first forward's own bind. Refused means not
  bound; accepted (even if ssh then closes it because the *remote* dial
  failed) means ssh is listening, which is the question. A tunnel alive past
  `TUNNEL_PROVEN_AFTER` with nothing bound now reads **"not connected —
  alive 2m but no ports bound"** in red, where it used to claim to be
  forwarding.
- **Tunnels run `ssh -v`.** These processes are invisible and the session
  pane is their only account of themselves, so the handshake belongs in it:
  the failure above is diagnosed exactly by the `ssh -v` a user would run by
  hand, so it is run that way in the first place. `MAX_STDERR_LINES` is 500
  to hold the handshake *and* the channel errors that follow it.
- **The curl probe takes its endpoint from the environment's own forwards**,
  not from `accounts::vault_addr_for`. accounts.json ships as template data,
  so its host matched nothing real and the probe silently skipped every
  time. A declared address still wins where one is configured for real;
  otherwise the environment's own `vault` forward is used. Only an HTTP
  service is worth curling — probing Postgres or Kafka would report a
  working forward as broken — and having nothing to ask is a skip, since
  `is_bound` already covers whether ssh is listening.
- **The Status column shows uptime, not "running".** `is_running` is
  `try_wait` — it only says the ssh *process* exists, and `Tunnel::spawn`
  returns the moment it does, while an auth failure or a refused bind takes
  another second or two to arrive. A bare "running · N forward(s)" therefore
  read identically for a healthy session and one about to die, and with the
  15s restart poll a permanently broken environment claimed to be running for
  part of every cycle. Under `TUNNEL_PROVEN_AFTER` (10s) the row says
  `connecting… 3s`; past it, `up 4m · 3 forward(s)`. Surviving that long means
  ssh authenticated and bound every forward, since `ExitOnForwardFailure=yes`
  would have killed it otherwise. Uptime is also what distinguishes a working
  tunnel from one being restarted every poll — the latter never gets past
  seconds. The window requests a repaint while anything is connecting, or the
  counter would look stuck exactly when it is being watched.
- **Port Forwards reads strictly per environment — no account fallback.**
  `env_pem`, `env_ssh_user` and `env_bastion_selection` look up the exact
  `<account>.<ENV>` key only. The inheriting `resolve_pem` /
  `resolve_ssh_user` / `bastion_selection` still exist and are still right
  for Open in VS Code and the Scripts dialogs, where an account with one
  environment sets a value once in Settings and everything inherits it. They
  are wrong here: several accounts host **two** environments, so inheriting
  means whichever environment was configured first silently becomes the
  default for the other — the tunnel connects with the wrong key, and the
  Setup dialog prefilled that way is one Save from writing those values as
  this environment's own. `env_ssh_user` still defaults to `ec2-user`, since
  that is the AMI's own account rather than a sibling environment's answer.
- **`bastion_key` upper-cases the environment**, as `vscode_key` always has.
  `MMODAL_ENV` is free text, so an account tagged `dev1` on some instances
  and `DEV1` on others previously got one pem entry but *two* bastion
  entries. `bastion_key_legacy` still reads the old tag-cased spelling, so
  an existing selection survives the change without being re-picked.
- **A saved bastion missing from the inventory is kept and flagged**
  (`port_forward_login_bastion`), not cleared the way
  `retain_available_bastion` clears it for the Scripts dialogs. The
  terminated instance is the diagnosis; blanking the field turns "this broke"
  into "you never configured this".

**The secondary bastion is a failover, not just a Scripts setting.**
`start_port_tunnel` walks the pair via `tunnel_attempt_order`.

- **A bastion that will not resolve is skipped immediately** and the next is
  tried in the same call.
- **A session that dies young fails the bastion over.** The test is
  `Tunnel::age` at the moment the death is noticed, not the reason given: a
  session that never connected dies in seconds, while an old one is a good
  tunnel dropping, and moving that to the backup would be an overreaction.
  The 30s threshold has to clear the 15s poll — a session that died at 2s may
  not be noticed for another 15.
- **Sticky, and the rotation keeps every bastion.** A tunnel happily up on
  the secondary is not disturbed to go back; a failure *on* the secondary
  falls back to the primary, or one outage on the backup would leave nothing
  to try.
- **The preference is in memory only.** Which box a tunnel happens to be on
  is a fact about this run. Persisting it would mean an outage during one
  session quietly re-aimed every later one.
- **The Bastion column shows the box actually carrying the session**, flagged
  `(secondary)`. An environment quietly running on its backup otherwise looks
  exactly like a healthy one.

**Test login is the real tunnel, not a probe.** It spawns through
`Tunnel::spawn` with the actual `-L` forwards under
`ExitOnForwardFailure=yes` — port binding is where these sessions actually
die, so a connection-only test would pass on a setup that cannot forward.
`resolve_tunnel_launch` is shared with `start_port_tunnel`, so there is one
definition of "can this connect", and the test walks the same failover order.

- **The environment's tunnel is stopped first and the passing session is
  adopted.** A byte-identical session binds the same ports, so leaving the
  old one up fails the test with `Address already in use` *because* the thing
  works; and respawning after a pass would throw away the session just proven.
- **`login_test_in_flight` makes `poll_port_tunnels` and `sync_port_tunnels`
  skip that environment** for the duration. Without it the poll starts a
  competing session on the same ports and `ExitOnForwardFailure` kills both.
- **The watch is frame-polled, never blocking.** `is_running` is `try_wait`,
  the same idiom as `poll_port_tunnels`; egui is immediate mode and a 5s wait
  would freeze the app. `TestState::Running` owns the `Tunnel`, so closing the
  dialog mid-test kills the child via `Drop`.
- **Failure hints annotate stderr, never replace it**
  (`classify_tunnel_failure` returns `None` for anything unfamiliar). A
  confident wrong hint sends the user to change the wrong field.
- **Elapsed time is in both result log lines** because "should this be quick?"
  is otherwise unanswerable from the log, and the dialog counts up live so a
  slow connect looks slow rather than hung.

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

**A protected user can be deleted, but only with a second confirmation.**
`protected_users` used to make delete impossible without editing
features.json and rebuilding. It is now a confirmation: when the typed name is
on the list, Bastion User Delete shows a red line naming it and a second
checkbox — *Confirm to delete protected user '<user>'* — and the Delete button
stays disabled until **both** boxes are ticked (`delete_confirmed`). A separate
box on purpose: the everyday one is ticked on every delete and stops being
read, while this one appears rarely and names the user, which is the moment
someone notices they typed `ec2-user` rather than `ec2user`.

`begin_delete_preflight` enforces the same rule where a stale dialog state
cannot bypass it, and logs at **warn** naming the user when the confirmation is
given — this is the one delete where the record matters.

**It does not unlock system accounts.** `delete_user.sh` keeps its own
hardcoded `PROTECTED_USERS` list *and* refuses any uid below 1000, so `root`,
`ec2-user`, `ssm-user` and friends are still refused on the bastion no matter
what is ticked here. What the confirmation unlocks is the *configurable* layer:
a site-specific name added to `features.json`.

**Restore is open to every user and every username.** The menu row is ungated
(no `allowed_users` list), and `protected_users` gates delete only — it briefly
gated restore too and that was removed on 2026-08-11, because re-keying a
shared account like `ec2-user` after its PEM is lost is exactly the job restore
exists for. The consequence is real and intended: restore replaces
`authorized_keys`, so a restore aimed at a shared account revokes the key
everyone on that bastion uses. The dialog's ⚠ line says so.

Every Scripts menu row carries hover text saying what it does; the personal
and default script rows show a trimmed preview of the body itself
(`script_hover_text`), since a name like "prep" says nothing about what is
about to be pasted into a live shell.

### uid/gid allocation, and Bastion User Sync

Both bastions mount the same EFS at `/efs/home`, and **NFS authorises by
number, not by name**. An account is therefore only usable on both boxes when
its uid *and* gid are identical on both — a user whose numbers differ cannot
read their own home directory (and the key material in it) on the box that
disagrees, while whoever holds that number there can.

**`create_new_user.sh` allocates one number for both** (`pick_shared_id`),
above every id already owning something under `/efs/home` and free in this
box's own passwd *and* group. Letting `useradd` and `groupadd` choose for
themselves is what broke: they are separate allocators over separate
databases, each taking its own lowest free number, so an account came out
uid 1011 / gid 1012 — and 1011 belonged to a different person on the
secondary, which failed the mirror with `UID 1011 is not unique`. Ids at or
above 60000 are excluded from the floor because root is squashed to `nobody`
(65534) on EFS, and one file owned by it would push every future account past
65535.

**The home directory on EFS is the authority**, not either passwd file. The
files already carry the numbers that matter, both boxes see the same ones, and
they are the thing being protected. That choice is what makes most repairs
cheap: moving an *account* onto its own home's numbers is a `usermod` and
**chowns nothing**, because the files already hold the destination. Moving the
home onto the account would be the opposite — a recursive `chown` of live data.
Getting this backwards is why an earlier version refused to fix a uid mismatch
at all.

**Every create runs a pre-flight first** (`begin_create_preflight` →
`run_create_preflight`). It reads both bastions, repairs what it can, and then
picks the new user's uid/gid with `choose_shared_id` — the lowest number free
as **both** a uid and a gid on **both** boxes, above everything either has
spent on a shared home. That number is passed as `--uid`; the script's own
`pick_shared_id` is only the fallback for a run by hand, because a script can
only see the box it runs on. That blind spot is the original bug: the primary
picked its own lowest free uid, the secondary had spent it on somebody else,
and the mirror failed with `UID nnnn is not unique` after the account was half
made.

The two repairs it applies, both putting an account onto its home's numbers:

- **ADD** — the account exists on one box only. Created on the other with the
  same numbers, `-M`, so it lands on files already owned by them.
- **REALIGN** — the account exists but disagrees with its own home.
  `groupmod` + `usermod` onto the home's numbers. `usermod` refuses while the
  account has a running process, which is the right answer: renumbering out
  from under a live session leaves a shell writing files nobody owns. It
  reports that on stdout rather than failing the command, so the pre-flight
  logs the output instead of assuming the repair landed.

**Every user gets a private group: one gid per home, named after the user.**
`create_new_user.sh` guarantees it for new accounts (`pick_shared_id` hands one
number to both the uid and the gid, and the group is created with the
username), and the audit enforces it on the existing ones. `realign_command`
creates the group where a box has none — half a mirror leaves exactly that, and
`usermod -g` fails against a gid no group holds.

What it will not repair, and why:

- **SHARED-ID — two homes owned by one uid.** Unrepairable here: one of them
  must be renumbered, and *that* is the recursive chown of live data. Until a
  human settles it, whichever account holds the number can read the other's
  files. Every other verdict about such a name is suppressed, since "align to
  the home" has two answers. **This is the state the bastions are in now**,
  from the 1011 incident.
- **SHARED-ID also covers gids.** Two homes on one gid means each carries
  group access to the other's files, which is the private-group rule broken.
  Same reason it cannot be fixed here: settling it is a `chgrp` of live data.
- **SHARED-GRP — the home's gid is a shared group** (`users`) rather than the
  user's own. No account change repairs it: the home needs its own gid, and
  that `chgrp` can only be run **by the user**, since root is squashed to
  `nobody` on EFS and cannot touch files inside a 0700 home.
  A group merely *sitting* on the number because it is misplaced is **not**
  this — it gets realigned away and the second pass finds the number free, so
  it is reported as CONFLICT rather than sending someone to fix what fixes
  itself (`a_group_that_will_be_realigned_away_is_not_a_shared_group`).
- **DIFFERS** — the boxes disagree and the home is gone, so nothing arbitrates
  and neither number can be called correct.
- **CONFLICT** — the repair needs a number already spent there. Taking it
  needs `--non-unique`, which makes two people one identity on shared storage.

**The pre-flight makes two passes when one repair unblocks another.** An
account holding a number that belongs to somebody else has to vacate it before
the rightful owner can be created there, and the plan comes from a single
snapshot — so the first pass reports `CONFLICT` and the second, after the
realign, sees the vacancy. Two is enough: a repair only ever moves an account
onto the number its own home already carries, so it frees at most the one it
left. The second read only happens when something moved *and* something was
blocked, so the ordinary case still costs one round trip.

Worth keeping straight, because they look alike: **two accounts on different
boxes holding one number is repairable** — the homes still say who each of them
should be, so the wrong one is realigned and the other is then created.
**Two homes owned by one number is not** — that is `SHARED-ID`, and it has no
answer, because "align the account to its home" gives two.
`one_number_held_by_two_accounts_on_different_boxes` and
`the_shared_id_case_is_two_homes_not_two_accounts` pin the difference.

A pre-flight that cannot read a bastion **aborts the create**: creating with an
unchecked number is the failure being fixed. Anything it could not repair is
logged but does **not** block, since the new account takes a number free on
both regardless. **Restore skips the pre-flight** — the account exists and
keeps its uid.

**Bastion User Sync** (gated by `user_sync.allowed_users`, shipped `[]`) is the
same comparison as a dialog, for auditing and for repairing drift without
creating anyone. It is the one Scripts dialog that does **not** drip-feed a
terminal: it needs both account tables before deciding anything, and the
secondary's session is deferred until the primary finishes, so it goes over
`exec_remote_command` (SSM send-command) in a worker and compares in Rust —
plain data in, plain data out, and tested as such.

**The dump's in-use tables are unfiltered on purpose.** A uid held by a
*system* account still blocks a `useradd`, so `UIDU`/`GIDU` carry the whole
passwd/group table while `ACCT` carries only the managed set (a home under
`/efs/home`) and `HOMEOWN` carries the shared mount itself. Filtering them the
same way would call a number free and then fail against it.

**There is no cron.** An earlier version installed one on both bastions;
checking at create time replaces it, and is better: it runs exactly when the
answer matters, needs no root cron entry or log file on either box, and cannot
drift out of step with the app.

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

**A dropdown offering exactly one bastion preselects it**, here as in the Port
Forwards login dialog — `sole_bastion_candidate` and `sole_secondary_bastion`
are shared, so the rules under "Port Forwards: the Login dialog" hold for
these dialogs too (fills only an empty field, counts candidates *after* the
filter, and never repeats the primary as the secondary).

What is specific to the Scripts dialogs is *where* it runs: inside
`load_bastion_pair`, which all three call on open **and** again on every
Environment change. So switching the dropdown to a one-bastion environment
prefills it, rather than only the environment the dialog happened to open on.
The secondary's exclusion of the primary matters more here than for the
tunnel: a pair naming one box makes the user scripts run their commands on it
twice.

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

### Bastion create "fails on the secondary" — account MISSING, group present

Symptom: the create finishes, the SSH test fails, and `create_diagnostics.sh`
reports `account : MISSING` on the secondary while `group : <user>:x:NNNN:`
exists. The `home`/`keydir`/`authkeys`/`userpem` lines are absent too — not a
separate fault, they come from a `sudo -n -u <user>` block that cannot run
without the account. `sudoers` may still be listed: it is written by four
**separate** GUI steps (`sudoers_grant.sh`), so its presence proves nothing
about whether the mirror got as far as `useradd`.

That state means `groupadd` succeeded and `useradd` did not, in
`secondary_mirror.sh`. Both used to run under `2>/dev/null || true`, so the one
sentence naming the cause was discarded and the step reported only
`secondary: FAILED to create <user>`. They now report a real failure (non-zero
exit) and still swallow the benign `GROUP=100` / skel / "home already exists"
warnings, which come with exit 0. On failure the step also prints who holds
that uid and gid on the secondary, because the likely causes are a **UID
already taken** there (the two bastions' user tables drift when accounts are
created in different orders) and a **stale group of the same name with a
different GID**, which makes `groupadd -g` fail and then `useradd -g` fail
against a GID that does not exist.

**Not to be confused with the restore feature.** Restore added nothing to the
create path: `7ccc181` left the secondary steps untouched, and the mirror is
byte-identical to before it was extracted into an asset (`bc47563`).

**Separately, the mirror step was taking the wrong timeout.** The worker picked
its 40s wait by matching `stat -c %u /efs/home`, but `143e690` had refactored
the step to `stat -c %u "$H"` — so the match silently stopped working and the
step took the 6s `STEP_WAIT` while its own retry loop runs up to ~24s. Nothing
fails loudly when that happens: the worker logs `TIMEOUT — prompt bump missed`
and sends the *next* step into a shell still running this one, where the TTY
hands those bytes to the running command instead of bash (the same mechanism as
"Multi-line paste" below). The matcher is now `SECONDARY_MIRROR_MARK` and
`secondary_mirror_step_matches_its_wait` pins the const and the script
together, since they live in different files and drifted apart once already.

When triaging: get the secondary tab's scrollback and the `[secondary i-…]`
lines from the app log. `secondary: useradd failed: …` names the cause outright;
`step N/M done in …ms (TIMEOUT — prompt bump missed)` means the drip-feed
desynced instead.

**Root cause, found 2026-08-11:** `useradd: UID 1011 is not unique`. The
primary allocated uid 1011 while 1011 belonged to a different person on the
secondary — someone had created that account on one bastion and not the other,
so the two boxes' counters had drifted. See "uid/gid allocation, and Bastion
User Sync" above for the fix on both sides: new accounts now take one number
free on both, and the existing drift is repaired by the sync dialog.

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

# CLAUDE.md

## Project overview

Rust-only EC2 + SSM instance explorer with CLI (`ec2_manager`) and desktop GUI (`ec2_manager_gui`, egui/eframe). The GUI embeds a terminal for interactive SSM sessions.

## Working with this repo

**The code that ships is in `WSL/`. The repo root holds a stale copy of the
same package** — same `name = "ec2_manager"`, its own `Cargo.toml`, its own
`src/`. It is not a workspace member and nothing builds it on purpose; it was
simply left behind and drifted. As of 2026-08-14 its
`src/bin/ec2_manager_gui.rs` is ~350 KB against `WSL/`'s ~1.2 MB, and three
months older.

Building it produces something that *looks* like the app and quietly is not:
it has no `build.rs`, no `assets/app_icon.*` and no `with_icon` call, so
**neither** app-icon path is wired up and the result shows the generic
Windows glyph on the taskbar. That is how it was found — a "the logo is
broken" report that was really "this binary came from the wrong directory".

A `build.rs` at the root now `panic!`s with a message pointing at `WSL/`, so
the mistake is impossible to make silently. `ALLOW_STALE_ROOT_BUILD=1`
overrides it. Nothing was deleted — the old tree is still there to read.

When the user asks to revert, restore, or reference a prior change (e.g. "change it back", "like it was before"), always check recent git history first (`git log`, `git show <sha>`, `git diff <sha>~..<sha>`) to see what was actually changed before making edits. Also check uncommitted/unpushed changes (`git status`, `git diff`, `git diff --cached`) since the relevant change may not yet be committed.

## Build commands

**A build against unconfigured assets fails** — no port forwards declared, or
an `accounts.json` / `features.json` still holding this repo's template values
— so the commands below need `ALLOW_NO_FORWARDS=1` on this tree, which ships
all three that way. One variable waives all three; see "The forwards.json
build check" and "The still-the-template check" below.

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
./scripts/build_binaries.sh test    # host-native dev build, forwards check bypassed
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
- **`email_local_suffixes` widens the shape without weakening it.** Some staff
  carry a marker in the address itself — a contingent worker as `tuser.cw@` —
  and the plain `flast` rule rejected them, so every such create opened a draft
  instead of sending. Configured suffixes are accepted *alongside* the bare
  form (`^tuser\d*(\.cw)?$`), never instead of it: one directory holds both
  kinds of user. They are **also probed** — `expected_email_locals` emits the
  bare and the suffixed form at every numeric rank — because the shape check
  and the probe must agree, or the mailbox is found by an address the check
  then rejects. Values are taken literally (no dot is assumed) and escaped into
  the regex, since `.` is a wildcard and `".cw"` unescaped would accept
  `tuserXcw@`. Blank restores exactly the old behaviour.
  - **The probe list doubles per suffix**, and its ordering (`tuser`,
    `tuser.cw`, `tuser2`, `tuser2.cw`…) is what the probe log reads back.
  - **A person with two *separate* mailboxes — `tuser@` and `tuser.cw@` with
    different primary SMTP addresses — is now `PROBE ambiguous`** and gets a
    draft rather than an unattended send. That is the correct answer (nothing
    here can say which is theirs), but it is a new way for someone who sent
    fine before to stop sending. Aliases on one mailbox still collapse, since
    the probe dedupes by `PrimarySmtpAddress`.
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

#### Ctrl+C — the highlight outside the terminal, or the shell's interrupt

The summary line above the terminal (`Instance: name (i-…)  Private IP: …`) is
a plain `ui.monospace`, and **egui labels are text-selectable by default**, so
a user can drag-highlight the instance id there. Highlighting a label does not
take focus off the terminal — typing still goes to the shell — so the terminal
was converting that Ctrl+C into an interrupt. `terminal_copy_action` is the one
place the three cases are decided:

| keys | with | action |
|---|---|---|
| Ctrl+Shift+C | — | copy the terminal's **own** drag-selection |
| Ctrl+C | a highlight outside the terminal | egui's copy; the shell gets nothing |
| Ctrl+C | nothing highlighted | ETX (0x03), the shell's interrupt |

- **The terminal's own drag-selection deliberately does *not* make Ctrl+C a
  copy.** Only a highlight *outside* the terminal does. A selection sitting in
  the scrollback is easy to forget about — leave for the Inventory tab, come
  back, and a Ctrl+C meant for a runaway command would silently copy instead.
  Terminal selections copy with Ctrl+Shift+C and right-click, as before.
- **egui emits `Event::Copy` and nothing else** for Ctrl+C — `egui-winit`'s
  `is_copy_command` returns early, so no `Key::C` follows, and the event
  carries no modifiers. Hence reading `ctx.input(|i| i.modifiers)` separately.
  `copied_selection` still guards the `Key::C` arm for a backend or keyboard
  layout that does emit one.
- **"Is something highlighted outside the terminal" is egui's own state** —
  `LabelSelectionState::has_selection` via `ctx.with_plugin`
  (`outside_highlight_active`). egui already clears it when the pointer is
  pressed anywhere that is not a label, so clicking into the terminal drops it.
- **Typing into the terminal clears that highlight**
  (`key_event_drops_outside_highlight`), so what is highlighted on screen is
  always what Ctrl+C would copy — the user re-highlights to copy again, and
  Ctrl+C goes back to interrupting. **Ctrl+C itself never clears it**: it is
  the one key that reads the highlight. A key the terminal ignores
  (Ctrl+Shift+C, a bare modifier) sends no payload and so is not typing.

### App icon — two places, both needed

The "e" glyph is set **twice**, and the two cover different things. Losing
either one is a bug that only shows up in some of the places an icon appears.

- **`assets/app_icon.png` → `ViewportBuilder::with_icon`** is the *live
  window*: the taskbar button while running, alt-tab, the title bar. eframe
  applies it with `WM_SETICON` once the window exists.
- **`assets/app_icon.ico` → the exe's Win32 resource**, embedded by
  `embed_windows_icon()` in `build.rs`, is the *file on disk*: Explorer, the
  Start menu, and a **pinned taskbar shortcut**. None of those ever run the
  app, so none of them can see the `WM_SETICON` one.

**The app used to set neither.** What it was showing was eframe's own fallback
(`load_default_egui_icon`, `eframe/data/icon.png`) — which is where the glyph
came from, and `assets/app_icon.png` is a byte copy of it so nothing changed
visually. Depending on a fallback meant the icon was a crate default one bump
away from changing, and the exe had no `.rsrc` section at all, so anything
reading the file got the generic executable glyph.

- **The resource compiler differs per target env, and both are supported.**
  `windres` emits a COFF `.o` for `*-pc-windows-gnu`; `rc.exe` (or `llvm-rc`)
  emits a `.res` for `*-pc-windows-msvc`. Only the tool and its arguments
  differ — the run-and-link loop is shared, so how the result reaches the
  linker cannot drift between the two. MSVC was unsupported until 2026-08-14,
  which mattered because `build_binaries.sh` picks the **MSVC** target on any
  non-Linux host: a Windows-native build got the window icon and no file
  icon.
- **`windres` runs from `OUT_DIR` on bare filenames.** Same hazard as the
  `dlltool` one below: the repo path has a space in it. The `.ico` is copied
  next to the generated `.rc` so no path is ever passed.
- **The step still fails soft** — no resource compiler, an unknown target env,
  a bad run — as a `cargo:warning`. An icon must not break a *developer's*
  build. **A release is different:** `verify_windows_icon` in
  `build_binaries.sh` re-reads the exe it just copied and `exit 1`s unless it
  has a `.rsrc` section of at least half the `.ico`'s size. That is where the
  hole was — every way of losing the icon is silent, and a `cargo:warning`
  scrolls past in a build log. `SKIP_ICON_VERIFY=1` overrides it for a host
  with no `objdump`. Set alongside
  `the_windows_icon_resource_carries_the_shell_sizes`, which checks the
  *asset*; this checks the *artifact*.
- Only `ec2_manager_gui` gets it (`rustc-link-arg-bin`); the CLI is a console
  tool.
- The `.ico` carries 16/24/32/48/64/128 as DIB and 256 as PNG. 16 is the
  taskbar size and 256 is Explorer's large-icon view; Windows scales badly
  when the nearest entry is far off.
- `strip = true` does not touch `.rsrc` — verified on the release
  cross-compile.

**A pinned shortcut can still look stale**, and that is Windows, not the
build: it caches by exe path, and `build_binaries.sh` renames each release to
`ec2_manager_gui_${APP_VERSION}.exe`, so a pin made against an older version
points at a file that is no longer there. Re-pin after upgrading.

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
  `curl.exe` ships with Windows 10 1803+. That request lives in
  **`src/atlassian_http.rs`**, not here: the Jira issue calls behind the Jira
  Tickets button use the same token, and one definition of how a token reaches
  curl is worth more than two modules that each look right. `alerts.rs`
  re-exports `ApiCall` / `recent_api_calls` / `clear_api_calls` so the Logs tab
  still reaches them as `alerts::…`.

Its **Pingdom** entry is the second watcher — see "Pingdom: acknowledge, wait
ten minutes, escalate" below.

The `alerts` section of `assets/features.json` (compiled in) carries only
`allowed_users` — the list of OS usernames that see the button (`["*"]` =
everyone, `[]` = nobody; the shipped default is `[]`, so the button is hidden
until an admin opts users in). The Atlassian cloud id, email and API token are
**not** fields there and must never become fields there — that file is
committed, so none of the five JSM/Opsgenie values (cloud id, email, token,
JSM schedule id, Atlassian account id — plus the Opsgenie API key) may live in
it. They resolve environment → Windows Credential Manager only, via
`src/jsm_auth.rs` (`load_auth`, `resolve_id`); see `scripts/store_jsm_credential.ps1`
or the `cmdkey` one-liners in its header to store them. The button also stays
hidden when that resolution comes back incomplete — it fails closed, like
`allow_delete_user`. `Features::alerts_visible_for` takes the resolved
`AlertsAuth` as a parameter rather than looking it up itself, because
resolving it is a `CredReadW` per field; the GUI resolves it once at startup
(`resolved_alerts_auth` in `App::new`) and reuses that one value for the
button gate, the reaper startup gate, and every later alerts-API call — never
per frame.

`JIRA_TOKEN` (and `ATLASSIAN_EMAIL`, `CLOUD_ID`, `SCHEDULE_ID`, `MY_ID`,
`OPSGENIE_API_KEY`) in the environment always overrides whatever is stored in
Credential Manager.

`assets/scripts/alerts_10min.sh` is the standalone bash equivalent (curl + jq,
same tag parsing, same local-time conversion) for terminal use.

#### The alert window (click an alert's id)

Clicking the `#id` in the Alerts list opens that alert in its own window,
several at once, keyed on the alert id — the same shape as the Jira ticket
windows, and `move_to_top` rather than a duplicate when it is already open.

- **It reads the alert in full, by id.** `fetch_latest` omits `description`,
  which is the field the window exists to show — the same trap the reaper poll
  fell into (see "The poll always reads the alert in full"). Never rendered
  from the list row.
- **Links are pulled out rather than left to be hunted for.** These alerts are
  machine-generated and the useful link — a Grafana panel, a runbook — arrives
  buried in the description or in an `extraProperties` value, never in a field
  of its own. `alerts::extract_links` scans message, description, properties
  and tags, dedupes, and **trims trailing punctuation**: a link at the end of a
  sentence or inside brackets otherwise carries a `.` or `)` that is invisible
  until someone pastes it. The `{{extraProperties}}` template key is skipped,
  as everywhere else here, or every link is listed twice. Each gets the
  Inventory table's own `paint_copy_button`.
- **Acknowledge has no on-call gate.** That rule belongs to the unattended
  watchers, which must not silence a page nobody has taken; a person clicking
  the button has taken it. "Acknowledge all" has never consulted the schedule
  either.
- **One scroll area, nothing inside it with a fixed height** — the constraint
  the ticket window records, for the reason it cost a layout bug there.

#### Escape closes the topmost ticket or alert window

`close_top_window_on_escape` runs **after** both sets of windows are drawn,
and that ordering is what gives the precedence for nothing: the `@` mention
dropdown consumes Escape with `consume_key` during its own render, and a
consumed key is gone from the queue — so Escape dismisses the dropdown when
one is open and closes the window when none is.

- **Only the topmost window, and only if it is one of ours.** Escape with the
  Alerts list or Port Forwards on top must not reach past them and shut a
  ticket window the user cannot see. `ctx.top_layer_id()` is matched against
  each window's own `Id`.
- **Closing keeps an unsent comment draft** (`stash_jira_draft`), restored by
  `open_jira_ticket`. Escape is easy to press, and binning what somebody had
  typed is the same silent loss the failed-post path already refuses to allow.
  **Both close routes stash** — the X and Escape must not differ — which is
  why the ticket list is drained rather than `retain`ed.
  The picked mentions travel with the draft, or a restored `@name` would post
  as literal text.

#### The window governs closed alerts only

`fetch_recent`'s window used to filter **every** row by `createdAt`, so an
alert acknowledged two hours ago and never closed vanished from a one-hour
window — found by having to widen the window to 4 hours to see a live
incident. `retain_in_window` now keeps any alert whose `status` is not
`closed`, whatever its age; the window applies to closed alerts, which are the
history it exists for.

- **`acknowledged` is not `closed`.** They are separate fields, and acking is
  what you do to an alert you are *working*. Only `status` says it is over.
  That distinction is the whole bug.
- **Open alerts come from a second pass** (`fetch_open`, `query=status:open`),
  not from walking the main feed further. That walk has no bound — one
  forgotten open alert would drag it to `MAX_PAGES` on every 10-second
  refresh — while this asks the API for exactly the rows wanted, so
  `OPEN_MAX_PAGES` is 4.
- **It re-filters by status client-side**, so an API that ignores `query`
  costs coverage (the open alerts among the newest few pages) and never
  correctness (closed rows leaking past the window).
- **A failed open pass degrades to the windowed results** rather than failing
  the fetch. Losing the whole window because one extra request failed is the
  worse trade; it is the behaviour this had before open alerts were pinned.
- **`page_is_past_cutoff` returns false for a page with no parseable
  timestamp.** Nothing on it says anything about the cutoff, and stopping
  there would end the walk on one malformed page.
- An unparseable timestamp is still kept whatever its status — better a
  visible odd row than a silently missing alert.

#### The Jira Alerts API trace (Logs tab)

The Logs tab carries a **Jira Alerts** checkbox that shows the last 5 calls to
the on-call alerts API — endpoint with query, ok/failure detail, duration and
response size, newest first.

- **Recorded at `curl_request`**, the single chokepoint every alerts call goes
  through (`fetch_recent`'s paging, `fetch_latest`, `fetch_alert`,
  `acknowledge_alert`, `fetch_on_calls`). Adding an endpoint traces it for free;
  bypassing that function is what would make a call invisible.
- **Only the newest call keeps its response body.** Recording a call drops the
  previous one's, so at most one is ever resident — a page is up to 50 alerts
  and the app does not control how large that is, so holding five would be
  unbounded in practice. Older rows keep `bytes`, which is then the only thing
  left saying how much came back.
- **A failed call is recorded *with* its body.** `--fail-with-body` leaves the
  API's own explanation on stdout, and that is the thing worth reading.
- **Nothing here can carry the token.** Credentials reach curl on stdin via
  `-K -`, never argv or the query, which is what makes the URL safe to render
  and to copy. `the_trace_line_cannot_carry_the_api_token` pins it, so a change
  that moved credentials into the query fails a test rather than putting them
  on screen.
- **Gated by `alerts_visible_for`** — the same gate as the Alerts button, so
  being off `alerts.allowed_users` (or an unconfigured site) hides the
  checkbox. `render_alerts_api_trace` re-checks it rather than trusting the
  checkbox state alone.
- The trace lives in a process-wide `OnceLock<Mutex<VecDeque<_>>>`, so its
  test is deliberately **one** test — split up, the cases would race under the
  parallel runner and pass or fail by scheduling.

#### Reaper remediation: the four `docker ps -a` snapshots

A remediation now photographs the box four times — before anything is touched,
straight after `up -d`, then again at **+1m** and **+5m** — and the whole thing
lands in the log. The verdict is still read from `compose ps` and nothing about
that changed; these are evidence for the human who reads the run afterwards.

- **The first two live in `reaper_fix.sh`** (its `snapshot` helper), the last
  two in `reaper_docker_ps.sh`, sent as their own `send-command`s by
  `spawn_reaper_snapshots`. **They cannot be one script.** The fix runs under
  `send_command_timeout` (90s shipped), so sleeping five minutes inside it
  would cut the invocation off before the `compose ps` block was ever read and
  turn every remediation into `Indeterminate` — with the fix having run.
- **Nor can they block `run_reaper_remediation`.** That would hold a failed
  fix's escalation back by five minutes, which is the one cost an on-call path
  must not pay for a diagnostic. So the follow-ups are a detached thread and
  the verdict is decided on exactly the schedule it always was.
- **`-a`, not a bare `ps`.** An exited container is the interesting case, and
  it is the one a bare `docker ps` does not list at all — the log would say
  "no containers" about a box full of dead ones.
- **Both listings are capped at 4000 bytes** (`head -c`).
  `get-command-invocation` truncates StandardOutputContent at 24KB and the
  verdict block is at the *end* of the fix's output, so an unbounded listing on
  a busy box pushes the one machine-read block off the cap.
- **The snapshot markers are deliberately not a superstring of
  `__RE_PS_BEGIN__`.** `parse_verdict` requires that marker exactly once and
  calls any other count untrustworthy, so a rename that made them collide would
  make every run unreadable. `snapshots_do_not_disturb_the_verdict` pins it.
- **The follow-ups run on every outcome except `__RE_NODIR__`**
  (`reaper_follow_ups_due`) — a failed `up -d` and a send-command that never
  answered included, because "did anything answer on that box a minute later"
  is most worth asking exactly then. `__RE_NODIR__` is the honest exclusion:
  nothing was touched, so there is nothing to watch settle.
- **The label reaches the follow-up script as a prepended shell assignment**
  (`RE_SNAP_LABEL='+1m'`), because the body is handed over base64'd on bash's
  stdin and there is no argv to put it in. It is always one of the literals in
  `REAPER_SNAPSHOT_DELAYS`, never user text.
- **The narration moved off stderr.** `run_reaper_remediation` used to
  `eprintln!` the ack, the last look and the verdict, so the only account of an
  unattended `compose down`/`up -d` on production went somewhere nobody reads.
  It now sends `ReaperEvent::Note` / `ReaperEvent::Transcript` and
  `poll_reaper_events` logs them. The transcript is reported **before** the
  verdict — a conclusion that arrives ahead of its evidence reads as a
  conclusion with nothing behind it.
- **`reaper_transcript_lines` emits one log entry per line**, not one entry
  holding a blob, so the level filters, the Find box and `MAX_LOG_LINES` behave
  as they do for every other line. It prints a count per listing *and* the
  output verbatim: the count is the same before and after a restart, and only
  the STATUS column says one was `Exited (137)` and the other is `Up 2 seconds`.
  A listing with no `CONTAINER ID` header is reported as **no readable
  listing**, never as zero containers — "docker would not answer" and "there
  was nothing there" are different diagnoses.
- **A test using a `Verdict::Success` transcript will hang the suite.** Success
  enters the stage-2 loop, which sleeps 30s per poll for a window defaulting to
  ten minutes. `applied_transcript()` exists for this: a fix that ran, carrying
  both listings, whose stack did not come back — so it returns before stage 2.

#### The alert names a target group, not an instance

`match_alert` required an `i-…` and returned `None` without a word when it
found none. The real reaper alert carries `Resource ID:
targetgroup/<name>/<id>` and no instance id anywhere, so **every** reaper
alert was silently declined — the feature looked dead and the log was empty.

- **`match_alert` returns a `Subject`, not a `Target`.** `Subject::Instance`
  or `Subject::TargetGroup`, so `src/reaper.rs` stays pure — alerts in,
  decisions out, no AWS — and the one network hop lives in the GUI.
  `AlertMatch::into_target` closes the gap once the instance is known, which
  keeps `Target` meaning exactly what it always did: *we know the box*.
- **An instance id wins wherever the alert has one.** Resolving a target group
  is a network hop and one more chance to be wrong; it is the fallback, never
  the preference.
- **`find_target_group` treats `-` as a word character**, unlike
  `find_instance_id`. Target group names are full of hyphens, and stopping at
  the first one looks up a name that does not exist. It matches the bare
  resource id and the tail of a full ARN, since they are the same token.
- **The ARN is looked up, never constructed.** `describe-target-groups
  --names` then `describe-target-health --target-group-arn`. Building
  `arn:aws:elasticloadbalancing:<region>:<account>:<resource>` by hand would
  bake in the partition and an account id taken from an alert *tag*, and a
  wrong ARN does not fail loudly — it finds nothing, which reads exactly like
  an empty target group.
- **Two instance targets are refused, not arbitrated** (`sole_target_instance`).
  Nothing here can say which one the alert was about, and picking wrong runs
  `compose down` on somebody else's box. Refusing costs a remediation a human
  then does by hand — with the page still live, because nothing was
  acknowledged. An IP-type target group is refused with that as the stated
  reason.
- **Resolution happens before the cooldown check but after `already_handled`**,
  which is why `ReaperState` gained that method: an alert this process has
  already acted on must not cost two ELB calls on every 30s poll for the rest
  of the run.
- **Live mode only.** Sim fakes `auth_status: Ok`, so resolving there would
  make real ELB calls out of the mode whose promise is that it does not.
- **A resolution failure does not mark the alert handled**, so a transient AWS
  error is retried next poll. What stops the log filling is
  `report_reaper_reason_change`, which reports a reason once per alert and
  again only when it *changes* — the same pattern, and the same reasoning, as
  `poll_port_tunnels`.

#### Why the reaper feature could look dead with an empty log

Three different states all wrote nothing, and telling them apart took five
rounds of guessing. Each now says so:

- **The gate.** `reaper_enabled_for` false was a bare `else { None }`. Startup
  now logs `reaper: off — reaper.enabled is false in this build` or
  `reaper: off — os_user 'x' is not on reaper.allowed_users`, and `reaper=` is
  on the `gates:` line beside `alerts=`.
- **Armed and idle.** A poll that matches nothing wrote nothing, so a running
  thread and a thread that never started were indistinguishable. There is now
  a DEBUG heartbeat per poll: `reaper: polled 10 alert(s), 0 matched`.
- **Identified but unusable.** `identifies` is split out of `match_alert` so
  the caller can tell "not a reaper alert" from "a reaper alert naming nothing
  actionable". The second is a WARN naming the alert id. That is the case
  above, and it must never be silent again.

#### The reaper stack lives at `/opt/cassandra-reaper`

`reaper::REAPER_DIR`, named once and used by both scripts and by every
message either this module or the GUI produces about them. It was `/opt/reaper`
in all four, which is a failure that hides itself completely: the guard trips,
the script reports `__RE_NODIR__`, and both a real remediation and the probe
say "not one of our boxes" — indistinguishable from the truth.
`the_scripts_and_the_parser_name_one_reaper_directory` checks both scripts use
the constant *and* that neither still says `/opt/reaper`, which is
substring-safe since `/opt/cassandra-reaper` does not contain it, so a
half-applied rename fails.

#### The poll always reads the alert in full

`fetch_latest` (the list) **omits `description`**, and these alerts name their
target group *only* there:

```
Resource ID: targetgroup/<name>/<id>
```

So the poll declined every reaper alert while Test Alert Match — which reads
`/alerts/{id}` — resolved the same alert id fine. Same matcher, different
payload. `Alert::description` warns about this in its own doc comment.

Every alert the poll might act on is now read in full by id
(`fetch_alert_in_full`), never matched off the list. Matching the list first
and falling back would work, but these alerts are shaped the same every time,
so a fallback that fires on every single alert is a slower way of always
fetching with a second path to keep correct.

**The three checks the list *can* answer run first** — identified, closed,
already handled — so a settled alert never costs a call. A failed read leaves
the alert unhandled, so the next poll tries again.

#### Duplicate alerts: the first one owns the incident

Two reaper alerts a minute apart are one outage. The first starts the fix and
carries the escalation; every later alert for the same thing is
**acknowledged and otherwise ignored**.

- **The suppression used to skip the acknowledge too.** The cooldown check sat
  above it, so a duplicate was dropped without being acked and kept ringing —
  observed as two alerts a minute apart where only the first was
  acknowledged. The on-call lookup now happens **before** the decision,
  because the duplicate branch needs it.
- **The key is `(fix, instance)`, not the instance** (`ReaperState::incident_key`).
  Keyed on the box alone, the first script's run would silence a second
  script's alerts on the same box — which is why `FixKind` is a value on
  `AlertMatch`/`Target` rather than being left implied. There is one variant
  today; the point is that adding the second does not require finding this
  code again.
- **`ActDecision` replaces a bool**, because "do not act" now has two
  meanings that must not share a branch: `AlreadyHandled` (this exact alert,
  already acted on and already acked — do nothing) and `Duplicate` (another
  report of a live incident — **acknowledge**, run nothing).
- **Off call the acknowledge is still withheld.** Same rule as everywhere else
  here: silencing a page nobody has taken is the one thing this must not do.
- **`mark_duplicate` deliberately does not touch the incident.** It adds the
  alert to `handled` so it is never reconsidered, and leaves the owner and the
  timestamp alone. Refreshing them would let a steady trickle of duplicates
  extend the suppression indefinitely and never re-run the fix for an outage
  that is genuinely still going.
- **The escalation is unchanged and belongs to the first alert.** Duplicates
  emit no `Outcome`, so they cannot start a second stage-2 watch or page twice
  for one outage. If reaper is still down, the first alert's remediation
  escalates exactly as it would have.
- **The window is `cooldown_mins`, now defaulting to 2.** It already meant
  "minimum gap between two remediations of the same thing"; re-keying it is
  what makes that sentence true. Two alerts a minute apart are one outage; a
  fresh one several minutes later is a fresh problem and gets a fresh fix.

#### Run Remediation, Re-run Alert Check, and Override

Two buttons beside Test Alert Match, same `reaper.allowed_users` gate.

**Run Remediation** treats the alert as if it had just arrived, whatever state
it is in — open, closed, acknowledged or not — and ignores the cooldown and
whatever this session has already handled, because a person asking for it has
overruled both.

- **A closed alert is a caller's choice, not a constant.**
  `run_reaper_remediation`'s last look aborts on a closed alert, which is right
  for the poll (a sixty-second self-close is real on this feed) and wrong for a
  button. It takes a `ClosedAlert` — an enum, not a second `bool`, because it
  sits next to `on_call` at every call site and two adjacent booleans swap
  silently.
- **Always enabled once an alert id is typed.** It used to be disabled under
  `reaper.dry_run`; that field no longer exists.
- **The confirmation names the alert, lists the three commands, and says the
  watchdog is left stopped and that being on call means acknowledging first.**
  It cannot name the instance: resolution is two AWS calls away and happens
  after the click. It says so, and points at Test Alert Match.

**Override** (beside it) skips the health pre-check.

- **Unticked, the box is read before it is touched** — the same read-only probe
  script — and the fix is skipped when the stack is already up. An alert stays
  open long after the box behind it recovers, so the alert cannot answer "does
  this still need fixing"; only the box can.
- **The rule is `stack_health`:** a compose service not running outranks any
  uptime; otherwise the *youngest* container decides, since one container
  looping is enough to make the stack unwell. At or under
  `RESTART_WINDOW_SECS` (60) it is treated as stuck restarting and fixed; above
  it the run stops and reports `currently up and running (cassandra-reaper up
  8m, cassandra up 1d 9h)`.
- **The boundary is inclusive on purpose.** The two errors are not symmetric:
  calling a looping stack healthy leaves an outage in place, while calling a
  just-recovered stack broken costs one more restart of something that was
  already restarting.
- **`Unknown` is never read as healthy.** A transcript that did not say gets
  the fix, saying which it was — the person asked for the run, and refusing on
  evidence we do not have is the worse call.
- **Uptimes are computed on the box, not parsed from `docker ps`.** That column
  is prose — `Up 8 minutes`, `Up 33 hours (healthy)`, and `Up About a minute`,
  which sits exactly on the boundary this decides. `reaper_probe.sh` emits
  `__RE_UPTIME__ <name> <seconds>` from `docker inspect .State.StartedAt`
  against the box's own clock. Still read-only.
- The tick is captured **when the button is pressed**, not read at confirm
  time, so toggling it while the dialog is open cannot change what was agreed
  to. The dialog restates which mode it will run in.

**Re-run Alert Check is gone**, along with the condvar nudge behind it. It read
as "check the alerts again" and actually meant "forget every handled alert and
every cooldown, then act on whatever is still open" — so the one button whose
name promised the least was the one that could start a real `compose down`. The
row menu and the Alert ID box aim at a specific alert, which is what anyone
re-running something actually wants. The poll thread sleeps its interval again.

**One alert cannot be remediated twice at once.** The poll and the button each
spawn their own thread, so `ReaperState` cannot arbitrate between them —
it is owned by the poll. `ReaperInFlight` is a shared set of alert ids with a
`Drop` guard, claimed *before* the spawn for the same reason `mark_handled` is.
Without it a press and the next poll can put two `compose down`/`up -d` on one
box at once.

#### Test Alert Match (Alerts window)

An alert id in, the whole decision out, without taking anything down to see
it: fetch the alert, run `identifies` / `match_alert`, resolve the subject,
**read the box itself**, report the projection. Results land in the log under
On-Call → Reaper Down.

Named for the alert, not for reaper — the matcher is reaper's today and more
are expected. The log lines still say `reaper probe:`, which is accurate about
what actually ran.

`reaper_probe.sh` is `reaper_fix.sh` with every command that *changes*
something removed: the `/opt/cassandra-reaper` guard, `docker ps -a`, `systemctl
is-active reaper-watchdog`, `docker compose ps --format json`. Same markers,
so one parser reads both.

- **That it changes nothing is a test, not a review note.**
  `the_probe_script_changes_nothing_on_the_box` scans the executable lines for
  every mutating verb and for any redirection. This is pointed at production
  from a button labelled "test"; a mutating line added later would run
  unannounced. The scan skips comments deliberately — the file's header names
  the commands it must never run, and that sentence is worth keeping.
- **The container listing runs *before* the directory guard**, in both the
  probe and the fix. The guard exists to stop the fix *changing* anything on a
  box that is not ours; a listing changes nothing, and the moment the guard
  trips is exactly when someone needs to know what is running there. Found the
  hard way: a real box with three containers reported a bare `__RE_NODIR__`
  and nothing else. `both_scripts_list_the_containers_before_the_directory_guard`
  pins the new order and `the_fix_still_checks_the_directory_before_it_changes_anything`
  pins the half that is load-bearing — the watchdog stop and the compose
  commands stay behind the guard.
- **`systemctl is-active`, never `status`.** It reports and nothing else. Worth
  having because the fix leaves the watchdog stopped on purpose, so a box found
  with it inactive is the trace of an earlier remediation rather than a fault.
- **The state summary is `compose_services`, not `parse_verdict`.** The verdict
  answers "did the fix work" and words its failure as `not running after
  restart` — a restart the probe never performed, and a sentence in the log
  describing something that did not happen. `compose_services` also *skips* an
  unreadable line rather than degrading to `Indeterminate`: nothing acts on
  this, it is one line for a human, and the transcript is logged verbatim
  beside it.
- **`reaper_account_context` is shared** with `resolve_reaper_target`, so the
  Live-mode, blank-account, credentials and auth checks happen in one order in
  one place. The probe builds the context itself because it needs it twice —
  to resolve the target group, then to send the read-only command to whatever
  that resolved to.
- **A box that cannot be read is a warning, not an abort.** The projection
  below it is still worth reporting, and "the box could not be reached" is
  itself an answer about whether a remediation would get anywhere.

- **It never acknowledges and never changes anything on the box** — but it
  *does* send one read-only SSM command, and it *does* reach the notifier.
  See "Test Alert Match is the dry run, and it really does page" below; the
  read-only guarantee rests on `reaper_probe.sh`, not on the caller. The read-only ELB calls behind a target group
  are the only thing it touches in AWS.
- **Gated on `reaper.allowed_users` without `reaper.enabled`.** Gating it on
  the feature being armed would withhold the tool from the only situation it
  exists for — getting the rules right *before* arming.
- **Its own channel** (`reaper_probe_tx/rx`), because `ReaperRuntime`'s exists
  only when the feature is armed. `poll_reaper_events` drains both.
- A closed alert is reported and the projection continues, since a probe run
  after the fact is the normal case.

### Pingdom: acknowledge, wait ten minutes, escalate

`src/pingdom.rs` is the second alert watcher on the same JSM feed. It does
three things and only three: **acknowledge** a pingdom alert as soon as it is
seen, **wait** for it to close, and **escalate** if it is still open
`watch_mins` (10) later. There is no remediation, and nothing here ever
touches an instance — which is why it is its own module rather than a second
`FixKind` in `reaper.rs`.

- **Off call it does nothing at all** — no acknowledge, no timer, no
  escalation. This is the opposite trade to reaper's, where off call still
  sends one quiet message. Pingdom is only looked at by whoever holds the
  pager, so acting off call would silence somebody else's page *and* ring a
  phone about an alert this machine has no business taking. A lookup that
  **failed** counts as off call: it is the only direction that can do neither
  by mistake. `pingdom_on_call_decision` is pure and separate from the loop so
  that rule is pinned by a test rather than by reading a `continue`.
- **The environment comes from the alert summary, not the `Environment:`
  tag.** These alerts do not carry that tag, so `environment_from_title` reads
  `Alert.message`. That field *is* in the list payload — unlike `description`,
  which is what forces reaper to `fetch_alert_in_full` — so pingdom matches
  straight off `fetch_latest` and never reads an alert in full to decide.
- **`pingdom.environment_after` is the rule that matters: a list of app names,
  and the environment is EVERYTHING AFTER the first one that appears.** List
  order decides, not the summary's word order, and markers match
  case-insensitively (an app name drifts in case the same way `MMODAL_ENV`
  does). A blank entry is skipped — it would match at offset 0 and make the
  whole summary the environment.
- **Everything after, not the next word, because an environment can be two
  words** (`prod one`). Taking one word would file `prod one` and `dev one`
  under the same `one`, which is the wrong incident. The cost is that trailing
  prose after the environment ends up in the key — visible in the log line, so
  the marker gets moved.
- **The fallback is the last word**, which is what this did before markers
  existed. It cannot see a two-word environment, so the log says
  `last word of the summary` when it fires — that is the signal to add a
  marker. `IncidentKey` normalises **case and internal whitespace**, so
  `prod one`, `PROD ONE` and `prod  one` are one environment.
- **Every first sighting logs what the summary yielded and by which rule**
  (`EnvSource::describe`), and the dry run logs it along with the configured
  markers. Without that line an environment read wrongly is invisible until
  two unrelated outages share a timer.
- **A token carrying `{`, `}`, `%`, `<` or `>` is refused.** This feed has been
  observed serving unrendered `{{…}}` and `&{%…%}%` where values should be; a
  live pull found two of ten alerts with a templated `App:` tag. Keying an
  incident on a template string would file unrelated outages under one
  environment and suppress all but the first.
- **Duplicates are keyed on environment** (`IncidentKey`), upper-cased —
  `MMODAL_ENV` is free text, and the repo already has the scar of treating
  `dev1` and `DEV1` as two things. Several checks failing in one environment
  are one outage: the first alert owns the timer and the escalation, every
  later one is **acknowledged and otherwise ignored**. A different environment
  is a new incident with its own timer, so PROD escalating never suppresses a
  DEV1 outage.
- **A duplicate deliberately does not refresh the owner's deadline.** Same
  reasoning as reaper's `mark_duplicate`: a steady trickle would otherwise
  hold the escalation off indefinitely, which is exactly when it is needed.
- **An alert whose environment cannot be read becomes its own incident**,
  keyed on its alert id. That can page twice for one outage; grouping them
  under a blank key would swallow a second one, and swallowing is the worse
  error.
- **The environment's slot clears only when its alert closes** — there is no
  ceiling. One alert nobody closes therefore mutes that environment until the
  app restarts. Chosen deliberately: never paging twice for one incident was
  worth more than the stuck-open case, and per-environment keying already
  bounds the blast radius to one environment.
- **Only the *owner's* close ends an incident.** A duplicate resolving on its
  own says nothing about the outage the timer is about
  (`a_duplicate_closing_does_not_end_the_incident`).
- **Owners are re-read by id every poll**, never inferred from the list: an
  alert older than `fetch_count` has fallen off it, and its absence there is
  not evidence that it closed. A failed re-read leaves the incident alone — it
  must not both free the slot and cancel the escalation.
- **A failed acknowledge does not drop the watch.** It costs a duplicate page;
  dropping the timer costs the escalation.
- `PingdomState` is owned by the poll thread and never shared, so there is no
  lock anywhere — the same property `start_reaper_poll` keeps. Nothing here
  blocks longer than one HTTP call, so unlike reaper there is no per-incident
  thread; the only work handed off is the send.

#### The escalation send — the app's first working outbound path

Until this landed **nothing in the app sent anything**: `ReaperEvent::Outcome`
pushed into `pending_notify` and no code drained it, exactly as
`2026-08-19-oncall-test-send-design.md` records. `send_escalation_email` is
that missing half.

- **The subject is the entire payload and the body is empty.** No domain, no
  environment, no account, no alert text. The environment is used *locally* to
  key incidents and never leaves — `a_pingdom_escalation_carries_nothing_but_a_code_and_a_time`
  asserts the incident label and the alert id are both absent.
- **The code is `RE-F`, reused from reaper rather than minted new**, and taken
  from `OutcomeCode::Failure.as_str()`, never written as a literal. The
  Pi-side daemon tiers on that vocabulary; a code it does not recognise is
  escalated as unknown, which is a failure that looks exactly like success
  from this side. Reusing it also means no Pi-side change was needed.
- **It must never route through `send_access_email.ps1`.** That script's
  recipient gates exist because it attaches a private key, and they must not
  be relaxed to fit a fixed configured address with no attachment.
- **Two EDR constraints carry over from the access-email spawn**: the script
  is run from the file **next to the exe**, never written to `%TEMP%`, and
  `-WindowStyle Hidden` is not used — the console is suppressed with
  `CREATE_NO_WINDOW`, an ordinary Win32 flag. `package_windows_zip` already
  copies `send_escalation.ps1` beside the exe.
- **No marker is read as failure, never as success.** The script prints
  exactly one marker on every path, so silence means it did not run.
- **A send that fails is never retried**, and is logged at **error** saying the
  alert is acknowledged and nothing will ring. Retrying risks double-paging,
  and the parent design's most important property is one email per failure and
  never a repeat — a re-send would defeat the Pi-side acknowledgement, which is
  the one thing that must not travel back across the boundary.
- **`escalation_script_args` names `-To`/`-Subject` once**, shared by the spawn
  and by the test that checks the script declares them. They live in different
  files, a rename is not a compile error, and PowerShell would reject the call
  at runtime inside a spawn nobody is watching — the same drift that broke
  `secondary_mirror_step_matches_its_wait`.

#### Why the pingdom watcher could look dead

Four states leave it dark and each one names itself at startup
(`pingdom_gate_report`, pure and tested), plus `pingdom=` on the `gates:` line:

- `pingdom.enabled` is false in this build
- the os_user is not on `pingdom.allowed_users` (`"*"` is **not** honoured
  here, as it is not for reaper)
- no JSM credentials
- **no escalation mailbox** — `ESCALATION_MAILBOX`, or the
  `ec2_manager/escalation_mailbox` credential. It stays dark rather than
  running acknowledge-only, because silencing a live page and then never being
  able to ring is worse than doing nothing.

The address itself never appears in a startup line: the app log gets pasted
into tickets. Armed, there is a DEBUG heartbeat per poll
(`polled N alert(s), M matched, K incident(s) being timed`) so a running
thread and a thread that never started are distinguishable.

`dry_run` is **not** a config field for either watcher — see "Test Alert
Match is the dry run, and it really does page" below.

#### Right-click an alert row

Every cell in the Alerts window's grid carries the same context menu
(`alert_row_menu`): **Test Alert Match** and **Run Remediation**, aimed at that
row. Copying an id out of the table and into a box to act on it was a step that
existed only because the box came first.

- **Every cell, not the row.** egui's `Grid` has no row-level response, so
  without this the target would be whichever column the user happened to aim
  at.
- **It writes into the caller's `Option`s rather than acting.** The menu is
  drawn deep inside a closure with `&self` borrowed; the results are applied
  after the window renders, exactly as the buttons' already are. That is also
  what keeps the two entry points from drifting — they converge on one pair of
  variables.
- **Same gate as the buttons** (`reaper_probe_enabled`, i.e.
  `reaper.allowed_users`), read into a local before the grid closure for the
  same borrow reason.
- Dry run runs at once; Run Remediation goes through its confirmation carrying
  the Override tick as it stands.

#### A closed alert is said once, not every poll

`reaper::alert_is_closed` and `already_handled` used to send a `Skipped` event
per alert **per poll**. A closed alert stays on the `fetch_latest` window until
newer alerts push it off, so a single settled alert wrote a line every 30
seconds — DEBUG, so usually invisible, but real, and it counted against
`MAX_LOG_LINES`. Both now go through `report_reaper_reason_change`, which
reports once per alert and again only if the reason changes. A closed alert is
finished, and the log says so once.

Pingdom never had the problem: its closed check `continue`s silently, and a
watched alert that closes logs one line and drops the watch, so it stops being
re-read at all.

#### Test Alert Match is the dry run, and it really does page

`dry_run` used to be a `features.json` field on `reaper` (and briefly on
`pingdom`) meaning "log what you would do and touch nothing". It is gone from
both, and there is no checkbox in its place. **Test Alert Match is the dry
run.** Run Remediation and Override are unchanged.

- **It sends a real escalation.** That is the point: a simulated send leaves
  the only part that can silently be broken untested. The button's hover text
  leads with it, because a control called "Test" that rings a phone is
  otherwise a trap.
- **It does everything except the thing that changes something.** For a reaper
  alert: fetch, identify, match, resolve the target group to an instance, and
  read the box with `reaper_probe.sh` — `docker ps -a`, `compose ps`, the
  watchdog, container uptimes — then escalate as though the fix had failed. No
  watchdog stop, no `compose down`, no `compose up -d`. For a pingdom alert:
  fetch and identify, which is all a pingdom decision has ever needed, then
  escalate. No ten-minute wait.
- **It never acknowledges**, either watcher. Acking silences a live page, and
  doing that as a side effect of a test is not a test.
- **On call is reported but not obeyed.** The log says what a real run would
  have done about acknowledging; the escalation goes either way, since it
  lands in the operator's own mailbox and is the thing being tested.
  Withholding it off call would make the feature untestable exactly when
  someone is setting it up.
- **`dry_run_route` decides which watcher runs, and reaper wins when both
  claim the alert** — it is the one with a box to read, so the more thorough
  run happens and answers the pingdom question on the way. Pure, so the
  precedence is pinned by a test rather than by the order of two `if`s inside
  a thread. Blank `*_contains` rules claim nothing, so an unconfigured build
  routes everything to `Neither`.
- **An alert no watcher claims escalates nothing** and says so. Paging about
  an alert neither watcher would ever act on proves nothing about either.
- **Its output is its own log source, `LogSource::AlertTest`**, with its own
  **Alert Test** entry in the Logs tab's On-Call dropdown — not the claiming
  watcher's. A dry run is one thing a person just asked for and wants to read
  end to end, and a *pingdom* run filed under Reaper Down (which is what
  happened before this existed) is invisible to the filter they would tick.
  The run is bracketed by `===== START` / `===== END` lines at warn level, so
  it is findable in an unfiltered log too. The reaper transcript is rendered
  inline into the same source rather than sent as `ReaperEvent::Transcript`,
  or that one part of the run would file itself as reaper output.
- **The hover text must stay watcher-neutral.** Which watcher claims the
  alert is not known until it has been fetched, so a tooltip promising
  `compose down` is simply wrong on a pingdom alert — it said exactly that
  until a user hovered one. It now names what each watcher does.
- **Run Remediation is reaper-only**, and its tooltip says so in the first
  line. The row menu offers it on every alert because the menu is drawn
  before anything is fetched; on a pingdom alert there is no fix to run and
  the log says so.
- **A failure the person needs to see goes to the window, not just the log**
  (`alert_submit_error`, a red line under the Alert ID box): an alert id that
  could not be fetched, one no watcher claims, or a send that did not go. The
  log is a different tab, and "nothing happened" is what a wrong id otherwise
  looks like. It clears on the next submit, and goes through `note_label` like
  every other status-coloured line — a bare `colored_label` stays thin on a
  light panel, and there is a test that says so.
- **No confirmation dialog.** Nothing is taken down, and the mode is meant to
  run the moment it is asked to. Run Remediation keeps its confirmation.
- **`start_reaper_probe` is gone**, folded into `start_alert_dry_run`. Keeping
  both would have been two overlapping walks of the same alert, one of which
  escalated and one of which did not.

**The cost of removing the field:** reaper drops from three safeties to two.
`enabled: true` plus a name on `allowed_users` now arms a live
`compose down`/`up -d`; there is no longer a third flag standing between a
config edit and production. Both remaining gates still ship closed, and every
`*_contains` rule ships blank, which matches nothing.

#### The Logs tab's On-Call filter

Right of the **Jira Alerts** checkbox, an **On-Call** dropdown with one
checkbox per on-call script (today: **Reaper Down**). Ticking one narrows the
log to that script's lines; nothing ticked is the whole log, exactly as before.

- **`LogEntry.source` is what it filters on**, not a substring scan of the
  message. Only the on-call scripts are distinguished — `LogSource` is not a
  general-purpose subsystem tag.
- **Nothing ticked must stay "everything".** A dropdown nobody opens must not
  remove anything from view, so `OnCallFilters::includes` returns `true` for
  every source when the selection is empty rather than matching nothing.
- **The closed button says when it is filtering** (`On-Call: Reaper Down`).
  The popup shuts as soon as it is used, and without this the only evidence
  that most of the log is hidden is the log being short — which reads as the
  app having stopped logging.
- **The level checkboxes still apply**, ANDed with the selection: both filters
  are on screen and independent, and one predicate feeds the count line, the
  view and Copy All.
- **The Jira Alerts trace is not affected by the selection.** An alerts call
  and the remediation it triggered are the same story; hiding one while reading
  the other is the opposite of what the dropdown is for.
- Gated by `alerts_enabled`, the same gate as the checkbox beside it.

### Jira Tickets (the ticket list and the ticket view)

`src/jira.rs` reads the Jira **issue** API — a different API from the JSM Ops
alert feed, reached with the *same* credentials. The **Jira Tickets** button
(beside Alerts) opens a list of your open tickets; clicking one opens a ticket
window, and a search box opens any ticket by key.

- **No new secret, and no second resolution.** The Atlassian email, API token
  and cloud id are the ones already resolved once at startup for Alerts
  (`App::alerts_auth`), and `jira.rs` takes `AlertsAuth` rather than declaring
  a near-identical `JiraAuth`. `assets/scripts/oncall_probe.sh` had been
  reading `https://api.atlassian.com/ex/jira/<cloud_id>/rest/api/3/myself`
  with that token since before this feature existed.
- **The Jira site is not necessarily the alerts tenant, and that is the
  failure this feature shipped with.** The JSM Ops feed is addressed by cloud
  id through `api.atlassian.com/ex/jira/<cloud_id>`; an org can run Jira on
  its own domain entirely. Addressing the alert feed's tenant then returns
  HTTP 200 and an **empty** ticket list — a valid answer about the wrong
  site, indistinguishable from having no tickets. `jira::resolve_base_url`
  layers `JIRA_BASE_URL` → `features.json` `jira.base_url` → the cloud-id
  form, so a build that configures nothing behaves as it did before.
  - **`base_url` is in `features.json` at the maintainer's explicit request**,
    against this file's usual rule — that file is committed, so the domain
    lands in git, which is why every *other* tenant-identifying Atlassian
    value resolves from the environment or Credential Manager (see
    `jsm_auth`). `JIRA_BASE_URL` overrides it, so a machine can point
    elsewhere without committing anything.
  - **The resolved site is logged at startup** (`jira: reading tickets from
    …`). Nothing else in the log would say which site an empty list came
    from, and that ambiguity is what cost a debugging round trip.
  - **`resolve_base_url` is forgiving about input**: a bare host, a trailing
    slash, and a pasted URL that already carries `/rest/api/{2,3}` or
    `/search/jql` all normalise to the same base. It is typed by a human out
    of a browser bar or a working curl command. A scheme-less host is
    upgraded to `https`, never `http` — a token travels on it.
- **API v3, and search is `POST /search/jql`.** Atlassian removed the old
  unsuffixed `/search`, and the v2 spelling of the replacement is not
  dependable — a real tenant served v3 and only v3. POST because that is the
  form proven against that tenant, and it keeps a long JQL out of a URL that
  gets logged and rendered.
- **v3 means the description is ADF**, so `description_text` flattens it: a
  recursive walk that keeps the words and the paragraph shape and drops
  formatting marks. It **also accepts a plain string**, which is what v2
  returns — one function, either payload, rather than a version to track.
  A mention keeps its name and a link its URL, because those are content
  rather than decoration; an **unknown node type recurses into its children**
  rather than being dropped, since ADF gains node types and losing a
  paragraph because it sat in an unfamiliar panel is the worse failure.
- **The default JQL uses `currentUser()`**, so nothing has to be told who you
  are — the Atlassian account id that `reaper`/`pingdom` need is not consulted
  here at all.
- **The buttons are whatever the ticket's own workflow allows.** Jira is asked
  (`GET /issue/{key}/transitions`) and one button is rendered per answer,
  labelled as Jira names it. "Start Progress" and "Close" appear where the
  workflow has them; a project that calls them something else still works. A
  pair of hardcoded buttons would be dead in any project that disagrees. The
  landing status is hover text, so a button named "Done" that moves the ticket
  to "Closed" says so.
- **The key and the transition id are whitelisted, not escaped.** The key is
  interpolated into a URL path and the id into a JSON body — the same stance
  `alerts::validate_alert_id` and `vault_iam` take with ARNs.
- **"No open tickets" and "the reply could not be read" must never look
  alike.** `parse_issue_list` errors on a missing `issues` key and returns an
  empty list for an empty one. Rendering the second as the first is the
  silent-empty failure the forwards.json build check exists to prevent
  elsewhere in this repo.
- **The issue and its transitions are two events**, though one thread makes
  both calls. A token that can read a ticket but not list its transitions is
  an ordinary permissions state and must leave the ticket **readable**, with a
  note where the buttons go — the same reasoning that keeps volumes and
  security groups on separate events in the Details tab.
- **Four error surfaces, deliberately not folded together**: the list's, each
  ticket's, each ticket's *transitions*, and the per-transition outcome. They
  have four independent async writers, and `AlertsWindow` already carries the
  scar of merging two — `ack_summary` is separate from `error` because a
  routine fetch landing seconds later erased a partial-failure banner.
- **The ticket window has exactly ONE scroll area, and nothing inside it may
  set a fixed height.** Both halves are load-bearing, and getting either
  wrong produced the same bug:
  - Without an outer `ScrollArea` wrapping the whole body, everything past
    the window's bottom edge is **clipped and unreachable** — there is no way
    to scroll to it. That is what happened to the comment box: it was visible
    until a comment thread loaded, then pushed off the bottom for good.
  - An inner `ScrollArea` with `max_height(N)` **and**
    `auto_shrink([false, false])` occupies exactly N pixels *always*, empty or
    not. A 240px description plus a 200px thread reserved 440px before the
    header, the fields or the buttons — and since an `egui::Window` sizes to
    its content, that gave the window a floor it could not shrink past, so
    dragging it smaller **snapped straight back**. The two symptoms looked
    unrelated and were one cause.
  - So the description and the comment thread render **inline**. A long one
    scrolls with everything else, which is also what a reader expects: one
    scrollbar, not three.
- **One window per ticket, keyed on the ticket key.** `egui::Id::new(("jira_ticket",
  key))` — *not* the title, which gains the summary the moment the fetch lands
  and would move the window if it were the id. Clicking a ticket that is
  already open calls `ctx.move_to_top` rather than opening a second.
- **A transition with a screen asks first, then sends once.** Jira
  transitions can carry required fields — a change request whose Close wants
  a comment, an ordinary ticket whose Close wants a Resolution — and posting
  the bare `{"transition":{"id":…}}` at one of those is rejected with a 400
  that names nothing the caller anticipated. **There is no two-phase
  transition API**: the screen cannot be opened and then submitted, so
  `expand=transitions.fields` reads it up front, the window renders a prompt,
  and the answers go out in one POST. A transition with no required fields
  stays a single click.
  - **A comment goes under `update.comment[].add.body`; everything else goes
    in `fields`.** The two are not interchangeable — a comment in `fields` is
    rejected, and a resolution in `update` sets nothing.
    `transition_payload` is pure and separate from `do_transition` because a
    wrong answer here is a wrong write to a live ticket.
  - **A dropdown's options come from Jira, never a built-in list.** A
    Resolution's choices are per-project; a hardcoded list would be right on
    one project and wrong on the next. `field_kind` keys on `allowedValues`
    rather than on the schema type, which covers Resolution, priority and
    every custom select without naming any of them.
  - **A chosen option id must be one Jira offered.** Jira accepts any id that
    exists, including one belonging to a different field's options, so an
    unchecked id is a silent wrong write.
  - **Nothing is pre-selected.** A Resolution defaulted to whatever Jira
    listed first is one click from being wrong, and "it was already filled
    in" is how that goes unnoticed. The submit stays disabled until every
    field is answered, so the prompt refuses locally instead of by 400.
  - **A required field this window cannot render disables the button and
    names the field** — user pickers, dates, cascading selects. Guessing at
    one posts a wrong value to a live ticket; a general Jira form renderer is
    a different feature from a ticket viewer.
  - Optional screen fields are dropped. This reproduces a *required* prompt.
- **`@` mentions are picked, never guessed.** Typing `@John Smith` by hand
  posts literal text and tags nobody — a Jira mention is a distinct ADF node
  carrying an **account id**, and nothing about typed text supplies one. So
  the comment box has an `@` dropdown, and only a name **picked** from it
  becomes a real mention.
  - **`picked_mentions` pairs the inserted text with the account id**, and
    `comment_adf_with_mentions` converts exactly those. Anything typed by
    hand stays text: without an account id there is nobody to tag, and
    resolving a typed name loosely would notify the wrong person — the same
    hazard the access-email work already has a scar from.
  - **Ranking is `ticket_participants`: reporter, then commenters (most
    recent first), then assignee**, deduped by account id so someone filling
    two roles appears once at the rank they first earned. An `@` in a comment
    usually answers whoever raised the ticket; an assignee is often you and
    may never have said anything.
  - **A bare `@` costs no request.** It lists the ticket's own people
    immediately, and the reporter is row 0 — so `@` then Enter, which is the
    common case, is two keystrokes. The directory is only queried once
    `MENTION_SEARCH_MIN` (3) characters are typed; below that the same local
    list is filtered.
  - **Matching is per word**, so `@smith` finds "John Smith". A surname is
    typed at least as often as a first name, and a whole-string
    `starts_with` would find nobody.
  - **Keys are consumed *before* the text box is built** — a widget only sees
    events still in the queue when it is added — so Enter picks a name while
    the dropdown is open and inserts a newline when it is not.
  - **The `@` must start a word**, or every email address typed into a
    comment would open the dropdown; and the token stops at whitespace, so
    the popup closes when the name ends rather than swallowing the sentence.
  - **Longest label wins when one mention's text prefixes another's**
    ("@John Smith" vs "@John Smithson"), or the shorter would be tagged and
    the remainder left as stray text.
  - **It scrolls itself into view.** The list is drawn below the comment box,
    which already sits near the bottom of the window, so it opens off-screen
    and would have to be scrolled to by hand — most of the point of a
    dropdown gone. `scroll_into_view` is set when it opens, when the token
    changes and when the highlight moves, and **cleared once used**, so it
    never fights someone deliberately scrolling up to read the ticket while
    the dropdown is open.
  - **The caret is moved past the inserted name, using the id the widget
    reports** (`edit.response.id`) — **not** `Id::new(salt)`. `id_salt` is a
    salt: egui derives the real id from it and the parent `Ui`, so an id
    built from the salt alone addresses nothing and `load_state`/`store_state`
    silently do nothing. Without the move the caret stays where the `@` was
    and the next keystroke lands inside the name just inserted.
  - **No glyphs from Unicode's Arrows block (U+2190–U+21FF) in any UI
    string** — write `->`. egui's default font carries none of them, so every
    one renders as an empty box. `·`, `—` and `…` are fine and used widely.
    `no_ui_string_uses_a_glyph_the_default_font_cannot_draw` scans this file
    and fails naming the line, because this shipped three separate times
    before it was pinned: `↻` on the ticket reload button, `↑↓` in the
    mention hint, and `→` between the two ends of every row in the **Port
    Forwards** window — that last one found by a user *after* the first two
    were fixed. Comments are skipped: they are never rendered, and the prose
    in that file uses `→` freely.
  - **The dropdown renders inline, not as a floating `Area`.** A popup inside
    a window inside a scroll area is where egui z-order and clipping bugs
    live, and this window has already cost one layout bug.
  - A deactivated account or an `app` (bot) account is dropped from the
    results: neither is worth one of five slots, and a deactivated one cannot
    be notified at all.
- **Comments read and post; `comment_adf` is the load-bearing half.** v3
  rejects a plain string body, so typed text has to be built into an ADF
  document — the inverse of `description_text`, and tested as a round trip
  because if the two disagree, what you typed is not what the ticket shows.
  - **A blank line is a paragraph; a single newline is a `hardBreak`.** That
    is what Enter and Shift+Enter do in Jira's own editor. Mapping every
    newline to a paragraph — which the first version did — turns each line
    break the author typed into a blank line, and the round-trip test is what
    caught it.
  - **Built with `serde_json`, never `format!`.** This is arbitrary user text
    going into a JSON request body; a quote or a backslash written into a
    hand-built string breaks out of it.
    `a_comment_body_cannot_break_out_of_its_json` pins that.
  - **A failed post keeps the draft**, and the draft is cleared only once the
    post is known to have landed. Losing what someone wrote is the worst
    outcome here.
  - **Only the button posts; Enter inserts a newline.** A comment is visible
    to the whole team and must not be one stray keystroke away. The button
    disables while in flight so it cannot be double-sent.
  - The thread is fetched on its own event, like transitions, so a thread
    that will not load leaves the ticket readable.
- **The due date is a calendar date and is never timezone-converted.**
  `duedate` has no time and no offset; putting it through `local_time` shifts
  the day for anyone not on UTC, so a ticket due the 1st reads as the 31st.
  `due_label` renders the date Jira stated and shows an unparseable value
  verbatim. `due_state` takes `today` as a parameter so the boundaries are
  testable without freezing a clock, and **a ticket in the `done` category is
  never overdue** — it is finished, not late, and colouring the one row
  needing no attention red is noise. Only overdue and due-today are coloured;
  colouring future dates too would leave nothing standing out.
- **A failed call now carries the API's own explanation.** `atlassian_http`
  preferred stderr whenever it was non-empty, and curl writes
  `(22) The requested URL returned error: 400` there every time — so the
  `--fail-with-body` payload, which for Jira says exactly which field was
  required, was discarded on every failed call. The body leads the detail
  now, with curl's status after it.
- **A transition has no confirmation dialog** — it is your own ticket and the
  move is reversible from the same button row. What guards it is `in_flight`,
  which disables the row so a double-click cannot fire two moves. On success
  the ticket **and** the list are re-read: the status changed, and so did the
  set of legal next moves.
- **Opened / Closed are exclusive, and Opened is the launch state.** Mixing
  finished and outstanding work in one list is what the toggle exists to
  avoid, so it is a `TicketScope`, not two checkboxes. Session state, never
  persisted — a window left on Closed months ago must not be what greets you.
  Switching scope clears the rows before refetching: the rows on screen belong
  to the other list, and showing them under the new heading misreports them.
- **Closed looks back a day window, default 30, editable in the header.**
  `parse_days` **whitelists** the input (1–3650) because it is interpolated
  straight into a JQL clause — the same stance `validate_issue_key` takes with
  a URL path. `applied_days` moves only on Go/Enter, so the rows on screen are
  always labelled with the window that actually produced them rather than with
  whatever is half-typed in the box.
- **The closed window is not simply `resolved >= -Nd`.** `resolved` is null on
  any ticket closed *without* a resolution — ordinary in workflows that do not
  use them — so that clause alone drops those tickets from the list entirely
  rather than mis-ordering them. The real clause ORs in
  `resolved IS EMPTY AND updated >= -Nd`, and the sort falls back the same way
  (`ORDER BY resolved DESC, updated DESC`).
- **There is no single Jira field for "when was this closed".**
  `closed_stamp` prefers `resolutiondate` and falls back to
  `statuscategorychangedate` (when the ticket last entered its current status
  category, which for a closed ticket is when it closed). Either alone is
  wrong for one kind of project.
- **The Due column swaps for Closed in the closed view**, rather than an
  eighth column being added. A due date on a finished ticket is dead weight
  exactly where the closed date is wanted, and `due_state` already refuses to
  call a done ticket overdue.
- **The empty closed list names the window it searched** — otherwise "no
  tickets" reads as "you have closed nothing, ever" rather than "nothing in
  the last 30 days".
#### The unread badge

The **Jira Tickets** button carries an amber fill and a count while anything
has changed since you last looked at it.

- **What "unread" detects.** `unread_keys` is pure over the rows and the seen
  store: a ticket is unread when its **status moved**, its **`updated` stamp
  moved**, or it has **no record at all**. The list search already returns
  both fields, so this costs **no extra API calls** — which is the whole
  reason it is defined this way. A comment always bumps `updated`, so every
  case it is meant to catch is caught; so does a label edit, which is why the
  list marker reads **`new`** (something changed) rather than "new comment".
- **The ticket window is where it becomes exact.** The comments are fetched
  there anyway, so it says `2 new comments, status: To Do -> In Progress`,
  built from `seen_before` — the record captured **before** opening
  overwrites it.
- **A ticket with no record is unread**, because a newly assigned ticket is
  the thing most worth being told about. The exception is the **first ever
  run**: with an empty store every ticket is unread, which is true and
  useless, so the first poll **baselines** silently and sets
  `jira_seen_baselined`.
- **Read is recorded on a successful fetch, not on opening the window.** A
  failed read would otherwise eat the notification silently.
- **A closed ticket's record is dropped**, which bounds the store to roughly
  your open-ticket count — it lives in `config.ini` and would otherwise gain
  an entry per ticket ever opened. A *reopened* ticket then has no record and
  reads as unread, which is right. **`prune_closed` is guarded on
  truncation**: the open search is capped at 50, so when the cap is hit,
  absence from the list is not evidence a ticket closed — it may be row 51,
  and pruning would make it announce itself the moment it surfaced.
  `prune_seen` is the age backstop (180 days) for tickets that left the list
  another way, such as being reassigned.
- **The background poll runs every five minutes and steps aside while the
  window is open** (a shared `AtomicBool`), because the window's own
  auto-refresh already polls on the same cadence and running both doubles the
  traffic for one answer. It polls the **Open** scope only — unread on a
  closed ticket is meaningless. DEBUG heartbeat per tick, so "the thread never
  started" and "nothing changed" stay distinguishable, which is the lesson
  both the reaper and pingdom watchers carry.
- **A filled button, not coloured text.** In a row of identically-shaped grey
  buttons a few coloured characters are easy to skim past, and a notification
  cannot have that failure mode. **Amber, not red** — red means "a thing
  failed" everywhere else in this app, and a new comment is not a failure.

- **Auto-refresh is five minutes, not ten seconds.** An unacknowledged page is
  time-critical; a ticket list is not. It runs only while the window is open,
  the `loading` flag makes a tick that lands mid-search a skip rather than a
  queued request, and the window calls `request_repaint_after` for the
  remainder — egui only redraws when something happens, so a timer merely
  *checked* at render fires whenever the next frame happens to occur. That is
  the mistake the tunnel status banner already made and fixed.
- **Colour is keyed on `statusCategory`, never the status name.** Names are
  per-workflow free text ("Closed", "Resolved", "Shipped"); the category is
  one of three fixed values, so matching the name would colour correctly in
  one project and wrongly in the next.
- **The Logs tab trace opens for *either* feature.** It records every
  Atlassian call, so gating it on `alerts` alone hid the ticket calls from a
  user opted into `jira` and not `alerts` — the shipped state of both lists,
  and therefore the exact state anyone debugging the ticket list is in. The
  checkbox is labelled **Jira API** rather than "Jira Alerts" for the same
  reason.
- **The search box takes a key and nothing else.** Free-text search across
  summaries is deliberately absent: it is a second search mode with its own
  result list, not a variation on opening a ticket. Non-key input gets an
  inline hint rather than a request.
- **`jira.allowed_users` ships empty**, like `alerts` — the button is hidden
  until an admin opts users in, and on this tree that means **you will not see
  it until you add your OS username** (or `"*"`) to `assets/features.json` and
  rebuild. Not because the feature is dangerous — it reads and moves *your
  own* tickets, and `Features::jira_visible_for` already hides it wherever
  credentials do not resolve — but because a button onto a live ticket system
  is an opt-in on the same terms as everything else in that file.
  `Features::default()` is empty too, so a features.json nobody can parse
  hands out no button either.
- **A missing button names the gate that closed it** (`jira_gate_report`,
  pure and tested) — off the allow-list, no credentials, or **no site** —
  and `jira=` is on the startup `gates:` line beside `alerts=`. The site
  counts toward the gate: a visible button that cannot reach anything is
  worse than an absent one. "The button is not there" is not a diagnosis — reaper had three
  dark states that all wrote nothing and telling them apart took five rounds
  of guessing. The report never carries the address or the token: the app log
  gets pasted into tickets.
- **Nothing here is persisted.** Open windows, the search box, the list and
  the Auto tick are all session state; there are no new `config.ini` keys.

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

#### The forwards.json build check

**`build.rs` fails the build when `assets/forwards.json` declares no port
forwards**, and names what is wrong. `validate_forwards` runs
`forwards_check::check_forwards_json`, a file `include!`d by build.rs *and*
compiled into the library — which is where its tests live, since a build
script's own `#[cfg(test)]` code is never run.

- **It exists because the runtime is fail-soft on purpose.** `ForwardsConfig::parse`
  yields no forwards rather than an error, so a bad config can never block a VS
  Code launch. The price is that every mistake in that file is silent: a stray
  comma, `"port": "8443"` written as a string, `"Port"` capitalised (serde drops
  an unknown key without a word), or an `environments` block nobody filled in
  all produce one indistinguishable result — an empty Port Forwards window,
  ports the app never binds, and nothing said. Nothing on the running side can
  tell that apart from a site that genuinely has no forwards, so the question
  moved to the one place that can still shout.
- **It checks shape as well as emptiness**: unknown keys at every level (`_`
  prefixed ones are documentation — `_comment` and `_example_environments` are
  load-bearing prose), an `ip` that is not an IP address, a `host` carrying
  whitespace or a `:port`, and any port that is not a number in 1–65535.
  A quoted port matters more than it looks: serde fails the *whole file* on it,
  so one typo takes every other environment down with it.
- **`ALLOW_NO_FORWARDS=1` bypasses the empty case only.** Shape errors stay
  fatal either way — the variable is for a developer with no endpoints to
  declare, not for a file that is wrong. `./scripts/build_binaries.sh test` is
  that mode as a build target: host-native, the variable exported for you, and
  three warning lines plus a tagged completion message, because what it
  produces is indistinguishable from a real build until someone opens Port
  Forwards and finds it empty. Do not release one.
- **The shipped file declares nothing**, so this tree needs the bypass until
  real environments are filled in. `the_bundled_forwards_file_passes_the_build_time_check`
  runs the same check with `require_forwards: false`, so the shipped file's
  *shape* is still guarded by the test suite.

#### The still-the-template check (accounts.json / features.json)

`src/defaults_check.rs` is the same arrangement as `forwards_check` — one file
`include!`d by build.rs *and* compiled into the library, which is where its
tests live — asking the third version of the same question: **not "is this
file well-formed" but "has anyone actually filled it in".**

- **It is the only one of the three checks a correctly-shaped file can fail.**
  build.rs's own `accounts.json` / `features.json` validators check that every
  required field is present with the right type, and the shipped template
  passes both, because it is well-formed on purpose. So the build that comes
  out is valid, runs, and is wrong in the one way nothing downstream can
  notice: three AWS accounts that do not exist, a git host called
  `github.YOUR-ENTERPRISE.com`, and an access-email path whose only permitted
  mail domain is `test.com`. Each of those fails later, somewhere else, as
  something else — an empty inventory reads exactly like an account you have
  no access to.
- **`validate_not_template` runs last**, after both shape validators, so a
  file that is malformed or missing a field is reported by the check that
  knows what to do about it. For the same reason `check_accounts_json` /
  `check_features_json` return **no** problems for input they cannot parse:
  offering "fill in your accounts" as the remedy for a stray comma is worse
  than staying quiet.
- **Placeholders are matched case-insensitively against every string**
  (`YOUR-COMPANY`, `YOUR-ENTERPRISE`, `example.com`…), recursively, so a
  template value is caught wherever it was copied to and the report names its
  dotted path. Two values that read as perfectly real are named **by value**
  instead — the shipped `test.com` / `test2.com` mail domains and the
  all-zeros `encrypt_template_guid` — because nothing about their shape says
  placeholder.
- **`_`-prefixed keys are documentation, exactly as in `forwards_check`.**
  Most of features.json by volume is `_*_comment` prose, and that prose names
  example hosts and example domains deliberately. Reading it as configuration
  would make the check impossible to satisfy.
- **A section nobody can reach is skipped**, and this is what stops the check
  becoming a nuisance: `personal_scripts` is only examined once its
  `allowed_users` names somebody, and `access_email` only while `enabled` is
  true. The binary hands that text to no one, so holding a build up over it
  would force a site to configure a feature it deliberately left switched off.
  Both gates live in the same file the check does, so arming a feature and
  checking it are one rebuild.
- **The bypass is `ALLOW_NO_FORWARDS=1`, shared with the forwards check** and
  set by `./scripts/build_binaries.sh test`. The name is now wider than it
  reads; one variable is deliberate, since the three checks ask the same
  question about three files and a developer on an unconfigured tree wants
  past all of them or none. It waives *only* "still the default" — every
  shape problem stays fatal.
- **There is no test asserting the shipped files are still the template.**
  It would be true today and would have to be deleted the moment real values
  land, and the build itself already asserts it: this tree does not compile
  without the bypass.

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

### File browser Upload — who the file ends up owned by

The upload writes through `sudo -n tee`, because the destination is usually a
directory the person's own account cannot write. That leaves the file owned by
**root**, which is useless to the user who just uploaded it. So the write now
chains a `chown` onto itself:

```
echo '<b64>' | base64 -d | sudo -n tee '<path>' > /dev/null \
  && { e=$(sudo -n chown '<user>': '<path>' 2>&1) && s=OK || s=FAIL; echo "__CHOWN_${s}__$e"; }
```

- **"The logged in user" cannot be answered by asking the box.** The file
  browser runs on its own hidden control channel
  (`ensure_control_channel`) — a *separate* SSM session that knows nothing
  about a `sudo su` typed in the visible terminal, so `whoami` there always
  says `ssm-user`. It is tracked instead from what was typed:
  `switch_user_target` pushes onto `PtySession.session_user_stack`, and
  `exit` / `logout` / Ctrl-D-on-an-empty-line pop it. `is_sudo_su_line` was
  folded into that function — the re-prep trigger and the chown target read
  the same line, so they cannot disagree about what a switch means.
- **It is a stack, so nesting unwinds correctly.** `sudo su - john` →
  `sudo su -` → `exit` lands back on john, not on the SSM login. An `exit`
  against an empty stack is a no-op — that one is leaving the SSM session
  itself. `the_tracked_login_follows_a_real_session_line_by_line` walks a
  whole session asserting the answer after every line.
- **`su` counts with or without `sudo`.** Once you are root, `su - <user>`
  is what people actually type; tracking only the `sudo` spelling left the
  tab believing it was still root and handed the file back to root.
  `sudo -i` / `sudo -s` count too. `su -c '…'` does not — it runs a command
  rather than opening a shell to sit in.
- **Pastes are scanned as well as keystrokes.** `paste_to_connection_tab`
  writes straight to the PTY writer and never goes through
  `send_raw_bytes_to_connection_tab`, so a `sudo su - <user>` inside a
  Scripts-menu body would move the shell with the tab none the wiser.
  `apply_pasted_lines_to_user_stack` applies the same scan and returns the
  trailing unterminated fragment to seed `input_line_buf`, so a paste with
  no final newline is completed by the Enter typed after it.
- **A `su` that *fails* is still counted**, because nothing local can know
  it failed — a typo'd or nonexistent account leaves the tab tracking a
  login it never reached. That does not fail silently: the chown to that
  name is refused and the yellow note says so, naming it.
- **An empty stack is not a failure to answer.** It means the tab is still on
  the login the SSM session opened as — which the control channel *shares* —
  so the command resolves it remotely with `id -un`. Hardcoding `ssm-user`
  would only be right by coincidence.
- **The name is whitelisted, not escaped** (`is_safe_remote_username`). It is
  scraped from a line a user typed into a terminal and interpolated into a
  shell command, the same stance `vault_iam` takes with ARNs. A refused name
  falls back to `id -un`, so the handover still happens.
- **The group is a bare trailing `:`**, so chown uses the account's own login
  group. Every account `create_new_user.sh` makes has a private group of its
  own name (see the uid/gid section), but a `<user>:<user>` spelling would
  fail outright on an account that does not.
- **The verdict marker is assembled at runtime** (`__CHOWN_${s}__`), for the
  reason `vault_iam`'s sentinel is: the shell echoes the command line before
  running it, so a literal `__CHOWN_OK__` in the command would match the echo
  and report success no matter what chown did. `parse_chown_marker` checks
  FAIL first.
- **The braces are load-bearing.** Flat, the trailing `|| s=FAIL` also catches
  a *write* that failed and reports it as a refused chown — a verdict about a
  file that was never there. Grouped, a failed write emits no marker, which
  reads as ownership *unconfirmed*: the truthful answer, since
  `exec_remote_command` discards the exit code and nothing here actually knows
  the write's fate.
- **A failed chown is a warning, never a failed upload.** The bytes are on the
  box; calling it a failure sends someone looking for a file that is already
  there. It surfaces as a yellow line under the Upload button
  (`FileBrowserState.upload_note`) and a `log_warn`. The expected case is the
  EFS home mount, where root is squashed to `nobody` and cannot chown at all —
  the message carries chown's own reason, which is the whole diagnosis.
- **The Upload button names the account before a file is picked**
  (`on_hover_text`, plus a weak `as <user>` beside it). The tracked login is
  scraped, so the user is the only one who can catch it being wrong — and
  afterwards the file already belongs to someone.
- **`--parameters` is JSON, never the CLI's shorthand** (`ssm_parameters_arg`).
  Shorthand `commands=["…"]` ends a value at the first unescaped double
  quote, so *this* command — which has them in both forms, the
  `echo "__CHOWN_${s}__$e"` marker and the `"$(id -un)":` owner fallback —
  was rejected by argument validation before a byte was sent:
  `Error parsing parameter '--parameters': Expected: ',', received: '_'`.
  It stayed hidden from feb303e (2026-08-20) until a real upload hit it
  on 2026-08-21, because the file browser
  normally runs on the control channel; `ssm_send_command` is only the
  fallback for when that session is unusable, so uploads worked until the
  day one fell back. Any new command sent this way inherits the fix, and
  `send_command_parameters_survive_a_command_with_double_quotes` pins it.
- The editor's **Save** path is untouched: `tee` truncates rather than
  recreates, so saving an existing file preserves its owner.

### The Details tab: volumes, IAM role, security groups

"See Details" spawns one worker thread that makes three `aws` calls and posts
results back as they arrive, so the panel fills top-down rather than after the
slowest one.

- **Volumes and security groups are separate `ProcEvent`s** (`VolumeResult`,
  `SecurityGroupResult`). They are different APIs needing different IAM
  permissions — `ec2:DescribeSecurityGroups` is commonly granted where
  `ec2:DescribeVolumes` is not — so one failing must not blank the other.
- **The IAM role and the group ids come from one `describe-instances`**
  (`fetch_instance_extras`). They were two calls on the same instance for a
  while; they answer the same query, so they are one.
- **`Instance` carries no security groups**, and adding them would mean an
  extra call per instance across the whole inventory. That is why the Details
  tab fetches them and why `filter::searchable_text` cannot match on them.
- **A rule is rendered per *source*, not per permission.** The API models a
  permission as one protocol/port range with a list of sources; the console
  lists a line each, and so does `sg_rules`. Showing only the first CIDR would
  hide who else can reach the box, which is the question the panel answers.
  Sources are `IpRanges`, `Ipv6Ranges`, `UserIdGroupPairs` (rendered
  `sg-… (name)`, or the bare id for a peer in another account, which comes
  back nameless) and `PrefixListIds`.
  A permission carrying **no** sources yields no rows — a row with an empty
  source column reads as "open to anything".
- **`"-1"` is the API's spelling of "every protocol"** and such a rule has no
  ports at all; both surface as `All`. A `-1` port means the same thing.
- **Parsing is a pure function over the JSON** (`parse_security_groups`), so
  the call uses no `--query` and the flattening is unit-tested without AWS.
  `fetch_volumes` predates that split and still parses inline, untested.
- **An unexpected response yields no groups, never an error.** This is one
  section of a panel; "None found" is recoverable where a panic is not.
- **Groups are sorted by name**: the API's order is not stable between calls,
  and sections that reshuffle between visits are hard to read.
- **An instance with no groups makes no second call** — `--group-ids` with
  nothing after it is an error, not an empty result.

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

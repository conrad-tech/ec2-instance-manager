# ec2_manager

Rust-only EC2 + SSM instance explorer with:
- CLI (`ec2_manager`)
- Desktop GUI (`ec2_manager_gui`, Rust `egui/eframe`)

## What is implemented

- Reads AWS profile from `~/.aws/profileChoice` (live mode), with sim fallback when missing.
- Resolves auth context and region (override/env/aws config/config fallback).
- Fetches EC2 inventory + SSM managed/ping status (live mode via AWS CLI).
- Fast client-side filtering (`--search`, `--state`, `--only-ssm`).
  - `--search` / `--include` include term(s), `--exclude` removes matches.
  - Search matches across instance name, private IP, instance ID, and all tags.
- Terminal discovery and launch adapters (Linux + Windows definitions).
- SSM connect and port-forward launch plans.
- Favorites, recents, saved filters, and port-forward presets persisted to config.
- Diagnostics for auth/dependencies/permissions.
- Interactive Rust shell mode (`--interactive`) for local operation without JS/HTML.
- **Scripts menu** (GUI, Connections page) — run bastion helper scripts against a
  primary/secondary bastion pair: `create_new_user.sh` (create a user, verify
  cross-bastion SSH, pull the PEM to your local Downloads), an admin-gated
  `delete_user.sh`, **Vault IAM Access** (write a Vault policy + AWS auth role
  bound to an IAM role, then read both back to verify), and an admin-gated
  **Vault IAM Delete** that undoes it.
  See [Scripts menu (bastion user management)](#scripts-menu-bastion-user-management).
- **Port forwards** (GUI, Connections page) — one hidden `ssh` session per
  environment holding that environment's forwards open, so an internal service
  is reachable in a browser without starting anything by hand. Per-environment
  setup for the bastion, login and key; failover to a secondary bastion; a Test
  connection that opens the real session; and a live `ssh -v` log per
  environment. See [Port forwards](#port-forwards).
- **Access email** (Windows) — after a create, copy a ready-to-run command that
  composes, encrypts and sends the bastion-access email with the PEM attached.
  You run it in your own terminal; the app never sends anything itself.
  See [Access email (post-create)](#access-email-post-create).

## Prerequisites

- **Rust / Cargo** — Install via [rustup](https://rustup.rs):
  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```
  Or download and run `rustup-init.exe` from https://rustup.rs. Restart your shell after install.
- **Windows (building from source):**
  - Visual Studio Build Tools — download and install from:
    https://visualstudio.microsoft.com/visual-cpp-build-tools/
    After installing, open Visual Studio Installer, click **Installed**, then click **Modify** on Build Tools.
    Check the **"Desktop development with C++"** workload and click **Modify** (bottom right) to install it.
  - MSVC target and toolchain:
    ```bash
    rustup target add x86_64-pc-windows-msvc
    rustup toolchain install stable-x86_64-pc-windows-msvc
    ```
  - `zip` for packaging (optional): `choco install zip`

## Build

```bash
cargo build
```

Build GUI binary too:

```bash
cargo build --features gui --bin ec2_manager_gui
```

Build scripts:

# Windows gitbash

./scripts/build_binaries.sh windows

# Windows gitbash command with WSL
./scripts/build_binaries.sh native

# Linux OS

```bash
# host-native build
./scripts/build_binaries.sh

# Pop!_OS linux + Windows artifacts
./scripts/build_binaries.sh all
```

Artifacts are written to:
- `dist/linux/`
- `dist/windows/`

Expected files (Windows — built on Windows with MSVC):
- `dist/windows/ec2_manager_1.1.exe`
- `dist/windows/ec2_manager_gui_1.1.exe`

Expected files (Windows — cross-compiled from Linux with MinGW):
- `dist/windows/ec2_manager_1.1.exe`
- `dist/windows/ec2_manager_gui_1.1.exe`
- `dist/windows/libgcc_s_seh-1.dll`
- `dist/windows/libstdc++-6.dll`
- `dist/windows/libwinpthread-1.dll`

Expected files (Linux):
- `dist/linux/ec2_manager_1.1`
- `dist/linux/ec2_manager_gui_1.1`

### The build refuses a still-template configuration

Three of the compiled-in asset files are checked by `build.rs`, and the build
**fails** — naming the file, the field and the value — when one of them is
still what this repo ships as a template:

- **`assets/accounts.json`** — an account still on an example account number
  (`123456789012`, `234567890123`, `345678901234`), or any value still
  carrying a `YOUR-COMPANY` / `YOUR-ENTERPRISE` / `example.com` placeholder.
- **`assets/features.json`** — placeholder text in any feature that is
  switched **on**: `personal_scripts.git_host` and its `default_scripts`
  (checked only once `personal_scripts.allowed_users` names somebody), and
  `access_email`'s `email_domains` and `encrypt_template_guid` (checked only
  while `access_email.enabled` is `true`). A section nobody can reach is left
  alone — it hands its template text to no one.
- **`assets/forwards.json`** — no port forwards declared at all.

None of these can be noticed later, which is why they are asked about here.
All three files are *valid* as shipped, so a template build comes up looking
healthy and is simply pointed at nothing: an empty inventory reads exactly
like an account you have no access to, an empty Port Forwards window reads
exactly like a site with no forwards, and an access email addressed to
`test.com` just quietly opens a draft instead of sending.

**`ALLOW_NO_FORWARDS=1` waives all three**, for building a tree nobody has
configured yet:

```bash
ALLOW_NO_FORWARDS=1 cargo build --features gui
ALLOW_NO_FORWARDS=1 cargo test --features gui

# the same thing as a build target — it exports the variable for you
./scripts/build_binaries.sh test
```

The name is historical: the variable started as the `forwards.json` check and
now covers all three, deliberately — they ask the same question about three
files, and a developer on an unconfigured tree wants past all of them or none.

It waives only *"this is still the default"*. A file that is genuinely **wrong**
— bad JSON, a missing required field, a misspelled key, a port written as a
string — fails the build either way.

> **Do not release a build made with it.** What it produces is
> indistinguishable from a properly configured build until somebody opens the
> app and finds nothing in it.

## Launch Desktop GUI (Pop!_OS 24.04)

From source:

```bash
cargo run --features gui --bin ec2_manager_gui
```

From built binary:

```bash
./dist/linux/ec2_manager_gui_1.1
```

Flags:
- `--mode sim` for local simulation
- `--mode live` for AWS live mode (default)
- `--dry-run` prevents launching terminal sessions
- `--no-dry-run` allows real terminal/session launches (default)

## Sim mode (testing without AWS credentials)

Sim mode loads fake EC2 instances locally so you can test the GUI and CLI without any AWS credentials or connectivity. Instances include realistic data for all columns: Name, State, SSM status, Private IP, AMI ID, Instance Type, Env, and MMODAL_ENV.

### GUI sim mode

From source (run from the project root, e.g. `D:\Work Projects\ec2-instance-manager`):

```bash
cargo run --features gui --bin ec2_manager_gui -- --mode sim
```

From built binary (from the project root or any directory):

```bash
# Windows (Git Bash)
./dist/windows/ec2_manager_windows/ec2_manager_gui_1.1.exe --mode sim

# Windows (PowerShell/CMD)
.\dist\windows\ec2_manager_windows\ec2_manager_gui_1.1.exe --mode sim

# Linux
./dist/windows/ec2_manager_windows/ec2_manager_gui_1.1.exe --mode sim
```

The `--mode sim` flag starts the GUI with simulated inventory data. You can test sorting, filtering, favorites, copy buttons, column resizing, and saved filters without connecting to AWS.

### CLI sim mode

From the project root:

```bash
cargo run -- --mode sim
```

Filter + dry-run connect:

```bash
cargo run -- --mode sim --search prod --only-ssm --connect i-sim0001 --dry-run
```

Interactive mode:

```bash
cargo run -- --mode sim --interactive --dry-run
```

## Live mode (Windows)

Requirements in `PATH`:

- `aws`
- `session-manager-plugin`
- one supported terminal (`pwsh`, `powershell`, `wt`, `cmd`, or `wsl`)

### Windows terminal dependencies (GUI)

The GUI embeds a terminal via ConPTY. Supported embedded shells on Windows:

- `cmd`: No extra install; built into Windows.
- `powershell`: Windows PowerShell 5.1 is built into Windows.
- `pwsh`: PowerShell 7 must be installed and `pwsh.exe` must be in `PATH`.

### Windows setup (binary distribution)

#### Built on Windows (MSVC)

1. Copy the `.exe` files to your Windows machine:
   - `ec2_manager_1.1.exe`
   - `ec2_manager_gui_1.1.exe`
2. No DLLs needed — everything is statically linked.

#### Cross-compiled from Linux (MinGW)

1. Copy the entire `dist/windows/` folder to your Windows machine.
2. Keep the `.exe` files and the three DLLs in the **same folder**:
   - `ec2_manager_1.1.exe`
   - `ec2_manager_gui_1.1.exe`
   - `libgcc_s_seh-1.dll`
   - `libstdc++-6.dll`
   - `libwinpthread-1.dll`
3. If Windows reports a missing DLL, it means the DLL is not next to the `.exe`.
   Do not put DLLs in `C:\Windows\System32`; keep them beside the `.exe`.

#### Common setup (both builds)

1. Install AWS tools on Windows:
   - AWS CLI v2
   - Session Manager Plugin
2. Ensure AWS auth/profile setup exists:
   - `%USERPROFILE%\\.aws\\profileChoice`
   - plus normal AWS config/credentials as needed by your profile.
3. Launch GUI:
   - `ec2_manager_gui_1.1.exe` (defaults to `--mode live`)


If Windows reports a missing DLL, it means the DLL is not next to the `.exe`.

### Exactly where DLL files go

Place the DLLs in the **same directory** as the executable you run.

Example target layout on Windows:

```text
C:\tools\ec2-manager\windows\
  ec2_manager_1.1.exe
  ec2_manager_gui_1.1.exe
  libgcc_s_seh-1.dll
  libstdc++-6.dll
  libwinpthread-1.dll
```

Run from that folder:

```powershell
cd C:\tools\ec2-manager\windows
.\ec2_manager_gui_1.1.exe
```

Do not put these DLLs in `C:\Windows\System32`; keep them beside the `.exe`.

### Windows sim-mode quick check

```powershell
# From project root
.\dist\windows\ec2_manager_gui_1.1.exe --mode sim
```

Commands:

```bash
cargo run -- --mode live --refresh
cargo run -- --mode live --refresh --connect i-0123456789abcdef0 --terminal pwsh
cargo run -- --mode live --port-forward i-0123456789abcdef0 --local-port 15432 --remote-port 5432 --terminal pwsh
```

## Windows VM test on Linux via Docker Compose

When you want to smoke-test Windows artifacts from a Linux workstation, use:

```bash
docker compose -f docker-compose.windows-test.yml up -d
```

Notes:
- This uses `dockurr/windows` (Windows VM in a container), not native Windows containers.
- Requires hardware virtualization support (`/dev/kvm`) and Docker privileges.
- Windows build artifacts are mounted read-only from `./dist/windows` into the container path `/shared/dist/windows`.
- Exposed ports:
  - `8006` web viewer
  - `3389` RDP

Validate compose config quickly:

```bash
./scripts/test_windows_compose.sh
```

One-command helper to build, run terminal-specific GUI validation tests, start the VM, and verify:

```bash
./scripts/run_windows_vm_test.sh
```

Skip terminal-specific GUI validation tests if needed:

```bash
./scripts/run_windows_vm_test.sh --skip-gui-terminal-tests
```

Helper script tests:

```bash
./scripts/test_run_windows_vm_test.sh
```

Automated Windows in-guest GUI terminal smoke test (PowerShell; uses `/oem/install.bat` hook and shared result marker):

```bash
./scripts/run_windows_gui_smoke_test.sh
```

Related smoke harness checks:

```bash
./scripts/test_windows_gui_smoke_compose.sh
./scripts/test_run_windows_gui_smoke_test.sh
```

## Debug mode

Run the GUI.exe with `--debug` to enable verbose logging in the built-in log panel:

```bash
# From source
cargo run --features gui --bin ec2_manager_gui -- --debug

# From built binary
ec2_manager_gui_1.1.exe --debug
```

Debug mode shows detailed PTY output events, credential lookups, WSL command construction, and connection lifecycle in the log panel (View > Logs or the log area at the bottom of the GUI).

To also capture low-level stderr output (PTY spawn commands, credential lookup results), redirect stderr to a file:

```powershell
# PowerShell
.\ec2_manager_gui_1.1.exe --debug 2> debug.log

# Git Bash
./ec2_manager_gui_1.1.exe --debug 2> debug.log
```

## Troubleshooting

### WSL running slow or connections timing out

If WSL terminal connections are slow to establish or appear to hang, WSL may need a restart:

```powershell
wsl --shutdown
```

Then relaunch the app. This resets the WSL virtual machine and often resolves performance issues caused by WSL updates or long uptime.

### WSLENV not forwarding environment variables

If WSL connections fail with `The config profile (xxx) could not be found`, WSLENV variable forwarding may be broken. Test it:

```powershell
set WSLENV=TEST_VAR/u
set TEST_VAR=hello
wsl -- bash -lc "echo TEST_VAR=$TEST_VAR"
```

If `TEST_VAR` is empty, WSLENV forwarding is broken. Run `wsl --shutdown` and retry. If the issue persists after restart, check for pending Windows or WSL updates.

## Useful options

- `--interactive`
- `--include <csv>`
- `--exclude <csv>`
- `--list-terminals`
- `--watch-profile`
- `--diagnostics`
- `--favorite <instance>`
- `--list-favorites`
- `--list-recents`
- `--save-filter <name>`
- `--apply-filter <name>`
- `--region us-east-1`
- `--dry-run`

## Run And Test Script

```bash
./scripts/run_and_test.sh
```

## Config path

- Linux: `~/.config/ec2-manager/config.ini`
- Windows: `%APPDATA%\ec2-manager\config.ini`

## Account configuration (accounts.json)

Place an `accounts.json` file next to `config.ini` (same directory) to configure your AWS accounts.
The file is an array of account objects:

> The copy in `assets/accounts.json` is compiled into the binary, and the build
> refuses it while it still holds the example account numbers or a
> `YOUR-COMPANY` placeholder — see
> [The build refuses a still-template configuration](#the-build-refuses-a-still-template-configuration).

```json
[
  {
    "label":      "Dev",
    "account_id": "123456789012",
    "region":     "us-east-1",
    "sort_order": 1,
    "color":      "#2ea043",
    "environments": [
      { "name": "DEV1", "vault_addr": "https://vault.dev1.example.com:8200" },
      { "name": "DEV2", "vault_addr": "https://vault.dev2.example.com:8200" }
    ]
  },
  {
    "label":      "QA",
    "account_id": "234567890123",
    "region":     "us-east-1",
    "sort_order": 2,
    "color":      "#c8b41e",
    "vault_addr": "https://vault.qa.example.com:8200",
    "environments": [
      { "name": "QA1" },
      { "name": "QA2" }
    ]
  },
  {
    "label":      "Staging",
    "account_id": "345678901234",
    "region":     "us-east-1",
    "sort_order": 3,
    "color":      "#e69600",
    "vault_addr": "https://vault.staging.example.com:8200"
  },
  {
    "label":      "Prod",
    "account_id": "456789012345",
    "region":     "us-east-1",
    "sort_order": 4,
    "color":      "#c82828",
    "vault_addr": "https://vault.prod.example.com:8200"
  }
]
```

Above: **Dev** declares two environments with their own Vault servers, **QA**
declares two that share one account-level Vault server, and **Staging** /
**Prod** are single-environment accounts that declare none — see
[Accounts with one environment](#accounts-with-one-environment).

### Fields

| Field          | Required | Description |
|----------------|----------|-------------|
| `label`        | Yes      | Display name shown in the UI |
| `account_id`   | Yes      | AWS account ID (used as profile identifier) |
| `region`       | No       | Default AWS region for this account |
| `sort_order`   | No       | Display order in legend and dropdowns (lower = first). Omit for alphabetical. |
| `color`        | No       | Hex color code for tab coloring (e.g. `"#2ea043"`). Omit for auto-assignment. |
| `vault_addr`   | No       | Account-level Vault server URL. Used by every environment in the account that doesn't set its own. |
| `environments` | No       | Environments hosted in this account, as `{ "name": …, "vault_addr": … }` objects. `name` must match the instances' **`MMODAL_ENV`** tag (case-insensitive). Omit for single-environment accounts. |

`vault_addr` resolves in this order: the selected environment's own value, then
the account-level value, then blank. It only pre-fills the
[Vault IAM Access](#vault-iam-access) dialog and is always editable there.

### Environments within an account

Several accounts host more than one environment, distinguished by the
**`MMODAL_ENV`** tag on each instance. The Scripts dialogs therefore select an
**environment**, not an account, and narrow both bastion dropdowns to instances
carrying that tag value.

The dropdown lists environments **by name** (`DEV1`, `DEV2`, …), not
`Account — ENV`. It shows the union of:

- every environment declared in `environments` for that account, and
- every distinct `MMODAL_ENV` value found in that account's loaded inventory.

So a new environment appears as soon as its instances do — declaring it in
`accounts.json` is only needed to give it a `vault_addr`.

Environments hidden with the toolbar's **Exclude Env** dropdown are left out, so
the Scripts dialogs offer the same environments the Inventory page is showing.
If every environment in an account is excluded, that account disappears from the
list entirely — it does **not** fall back to an unfiltered whole-account entry,
which would re-expose the bastions you just hid.

> Because rows are named by environment alone, two accounts that use the *same*
> `MMODAL_ENV` value produce two identically-labelled rows. Keep environment
> names unique across accounts if you rely on this dropdown.

### Accounts with one environment

Nothing extra to configure. An account with a single environment shows a single
row and behaves exactly as it did before this change. Two cases:

- **The instances are tagged** (one `MMODAL_ENV` value across the account) — the
  environment is discovered automatically. Declare it in `environments` only if
  it needs its own `vault_addr`; otherwise set `vault_addr` at the account level,
  as **Staging** does above.
- **The instances are untagged** — there is nothing to discover, so the account
  itself is the single entry, **labelled with the account name** (there is no
  environment name to show), no environment filter is applied to the bastion
  dropdowns, and the account-level `vault_addr` is used. **Exclude Env** cannot
  hide such an entry, since it has no environment name to match. This is the
  pre-existing behavior, unchanged.

### Available color codes

Use any hex color code (`#RRGGBB`). Here are some suggested defaults:

| Color | Hex | Suggested use |
|-------|-----|--------------|
| Green | `#2ea043` | Dev |
| Teal | `#00b4a0` | Test |
| Blue | `#369adc` | QA |
| Yellow | `#c8b41e` | Integration |
| Orange | `#e69600` | Staging |
| Red-Orange | `#d2503c` | UAT / Pre-prod |
| Red | `#c82828` | Production |
| Crimson | `#aa1e46` | Production (secondary) |
| Purple | `#8c3ca0` | Sandbox |
| Indigo | `#6464be` | Shared services |
| Dark Teal | `#008c78` | DR / Backup |
| Brown | `#b4783c` | Legacy |

Colors can also be customized at runtime via **right-click on a legend item** or **Edit > Account Tab Colors > Edit**. Runtime overrides are saved to `config.ini` and take priority over `accounts.json`.

## Port forwards

Reaching an internal service from your workstation means tunnelling it through a
bastion. The app keeps **one hidden `ssh` session per environment** holding that
environment's forwards open, so `https://vault.dev1.example.net` in a browser
works without you starting anything by hand. The sessions stop when the app
closes.

The **Port Forwards** button on the Connections toolbar (left of Close All)
opens the window that manages them.

### The window

| Column | Meaning |
|--------|---------|
| **On** | Whether this environment's tunnel runs. **On by default** — the opt-*out* is what gets saved (`forward_ports_off.<account>.<ENV>` in `config.ini`). |
| **Environment** | The `MMODAL_ENV` value, uppercased. |
| **Status** | See below. |
| **Bastion** | The box actually carrying the session, flagged `(secondary)` when it has failed over. |
| **Setup** | Opens the connection-setup dialog for that environment. |

Status values, and what each one actually proves:

- `connecting to i-0abc… 3s` — the session has started but has not yet bound
  its ports.
- `forwarding 9 ports · up 4m` — **the session itself reported binding those
  ports** (read from its own `ssh -v` output, so another program listening on
  the same address cannot be mistaken for it). The clock runs from the bind,
  not from process start.
- `forwarding 9 ports · up 4m · verified` — a request through the tunnel was
  answered. See [Verification](#verification).
- `not connected — alive 2m but no ports bound` — ssh is running but never
  finished connecting, so nothing is forwarded. Expand that environment's
  **session output** for the reason.
- Anything else is the reason it has not started, e.g. `no pem saved for this
  environment` or `<account> needs authorizing — forwards start once it is`.

Below the table, each environment has two collapsible sections: its **forwards**
(`ip:port → host:port`) and its **session output** — the live `ssh -v` log from
that hidden process, scrollable, with a Copy button. That log is the first place
to look when something is not working, since these sessions are otherwise
invisible.

The toolbar shows `Forwarded ports for all environments (4 tunnels up)` in green
for a minute once everything is up, then hides itself; it comes back for another
minute if forwarding breaks and recovers.

### Setup: bastion, login and key

Every row has a **Setup** button — including working ones, since the usual
reason to open it is a setup that used to work and stopped (a terminated
bastion, a deleted login, a rotated key).

- **Primary bastion** — the box the tunnel connects through.
- **Secondary bastion** — the failover target, tried when the primary will not
  hold a session. Optional; one bastion means no failover.
- **Login user** — defaults to `ec2-user`.
- **Key (pem)** — chosen from the shared key library, or added with
  **+ Add pem...**.

A dropdown offering exactly one bastion selects it for you. A saved bastion that
is no longer in the inventory stays selected and is flagged **"no longer in
inventory"** rather than being silently cleared — that terminated instance is
usually the whole explanation.

These are the **same** key and login that **Open in VS Code** uses, and the same
bastion pair the Scripts dialogs run against, so fixing a login here fixes it
everywhere. Changing the primary bastion therefore re-aims Bastion New User,
Bastion User Delete and Vault IAM as well; the dialog says so and it is recorded
in the log.

Values are stored **per environment** (`<account_id>.<ENV>`), and Port Forwards
reads only that key — it never inherits an account-wide default. An account
hosting two environments would otherwise connect one with the other's key.

### Test connection

**Test connection** saves your changes, then opens the identical hidden session
the tunnel uses — the real `-L` forwards under `ExitOnForwardFailure=yes`, and
the same failover to the secondary. It is not a cheap reachability probe: port
binding is where these sessions actually fail.

It counts up while it runs, and either reports `connected to i-0abc… in 4.2s —
9 forward(s) bound` or shows ssh's own error with a hint (`the key or the login
user is wrong`, `another process is already holding that local port`, `the
bastion could not be reached`). The raw stderr is always shown underneath.

If the environment is switched on, the session that passed **becomes** the live
tunnel rather than being thrown away and restarted.

### Failover

If the primary bastion cannot be reached — terminated, missing from the
inventory, or simply refusing connections — the tunnel retries on the secondary.
A session that dies within 30s of starting counts as "this bastion did not
work"; one that dies after running for an hour is treated as a dropped
connection and restarted on the same box.

Failover is **sticky**: a tunnel happily up on the secondary is not moved back
to disturb it. The rotation keeps both boxes in play, so a later failure on the
secondary falls back to the primary. Which box a session is on is a fact about
that run and is never written to `config.ini`.

### Verification

Binding a local port only proves ssh is listening; it does not prove anything
reaches the far side, because ssh does not dial the remote host until a forward
is first used. So once a tunnel settles, the app makes one request through it
with `curl` and reports `verified` or `nothing answering through it`.

The endpoint is that environment's **Vault** forward. An environment with no
Vault forward is skipped rather than reported as failed — `is_bound` already
answers whether ssh is listening. Any HTTP reply counts as proof, including
`503`/`429` from a sealed or standby Vault.

### Configuring which ports are forwarded

Forwards come from the compiled-in `assets/forwards.json` plus your hosts file.
Edit that file and rebuild to change them.

```json
{
  "default_port": 443,
  "port_rules": [
    { "match": "postgres", "port": 5432 },
    { "match": "solr", "port": 8984 }
  ],
  "environments": {
    "DEV1": [
      { "ip": "127.200.10.1", "host": "uweb01.dev1.example.net" },
      { "ip": "127.200.10.2", "host": "admin01.dev1.example.net", "port": 8443 }
    ]
  }
}
```

- `key` — the `MMODAL_ENV` tag, matched **case-insensitively**.
- `ip` — the loopback address bound on your machine. **Ignored where your hosts
  file already resolves `host`** — that IP wins, so an existing setup keeps
  working.
- `port` — optional. Omitted, the port comes from the first matching entry in
  `port_rules`, and failing that from `default_port`.

**A service on a non-standard port needs a `port_rules` entry**, or it silently
gets `default_port` and the forward breaks in a way that looks fine: the local
bind succeeds, the window says the ports are forwarded, and only a request
through the tunnel finds nothing there. Take the port from a `LocalForward` line
you know works.

Every environment's tunnel runs at once, so **no two environments may share an
`ip:port`**. Give each its own range (`127.200.10.x`, `127.200.20.x`). A clash is
detected at startup and the later environment's forward is dropped, but the fix
is distinct addresses.

Entries under a single-word comment in your hosts file (`# DEV1`) are picked up
for that environment too, even when `forwards.json` does not declare them.

**The hosts file is only ever read, never written.** A forward works without an
entry — the remote name is resolved on the bastion — the entry just lets you
type the name in a browser instead of the loopback IP. Where entries are
missing, the app offers the lines to paste in.

### Running from WSL

If you run the Linux build under WSL, the tunnel is started with the **Windows**
ssh client (`C:\Windows\System32\OpenSSH\ssh.exe`) so its ports bind where your
browser can reach them. A tunnel started by the Linux client binds inside WSL's
own network namespace, which Windows cannot reach at all, and it fails silently.
Keep your `.pem` under `/mnt/<drive>/…` — Windows OpenSSH refuses a private key
whose permissions it cannot vouch for, which includes one reached over
`\\wsl.localhost`.

## Scripts menu (bastion user management)

On the **Connections** page there is a **`Scripts (N)`** dropdown, where `N` is
the number of available scripts (toolbar order: Exclude Env, Scripts, Alerts,
Close All). It runs helper
scripts against a **primary + secondary bastion pair** in a chosen environment.

Each script opens a dialog. The two user-management scripts share these fields:

- **User** — the username to act on.
- **Environment** — which environment's bastions to target, listed by
  environment name. See
  [Environments within an account](#environments-within-an-account).
- **Primary Bastion** / **Secondary Bastion** — dropdowns (`choose ▾`) narrowed
  to instances whose `MMODAL_ENV` tag matches the selected environment, and then
  by the `primary_bastion_filter` / `secondary_bastion_filter` values in
  `features.json` (substring match on instance **name or id**). Items display as
  `Name  i-0abc…`.

The environment filter is never relaxed — if an environment has no matching
bastion the dropdown stays empty rather than falling back to another
environment's boxes, so a script can't be aimed at the wrong environment by
accident.

The selected bastion pair is cached per environment in `config.ini`
(`bastion_pair.<account_id>.<env>=<primary>|<secondary>`) and pre-filled on the
next run — for all three scripts, which share the cache. Accounts with a single
untagged environment keep using the older `bastion_pair.<account_id>` key; that
key is also read as the starting value the first time you open a dialog for a
newly-split account, so no existing selection is lost.

For each bastion the app reuses an already-connected tab if one exists, otherwise
opens a new SSM session; for the user-management scripts it then elevates with
`sudo su`, `cd ~`, and runs the script. Commands are drip-fed one line at a time,
waiting for the shell prompt between lines.

### Bastion New User (`create_new_user.sh`)

- **Grant sudo (NOPASSWD:ALL)** checkbox (off by default) passes `--sudo`.
- Runs `create_new_user.sh` on the **primary** (creates the user and generates a
  PEM key), and mirrors the matching UID/GID `groupadd`/`useradd` (and sudoers,
  if `--sudo`) on the **secondary** from the shared EFS home.
- After both finish it verifies SSH login **primary → secondary** and
  **secondary → primary** as the new user, then pulls the generated PEM back to
  your **local Downloads folder** as `<username>-<MMODAL_ENV>.pem` (the
  `MMODAL_ENV` tag is read from the primary bastion).
- On success the status line reports all tests passed and the saved PEM path.

### Bastion User Delete (`delete_user.sh`, admin-gated)

This entry is **hidden unless enabled at build time** (see
[Feature flags (features.json)](#feature-flags-featuresjson)). The menu shows it
as **Bastion User Delete**; the script it runs is `delete_user.sh`.

- Runs `delete_user.sh` on both bastions: the primary removes the account, group,
  sudoers entry, generated PEM, **and the shared EFS home** (`/efs/home/<user>`);
  the secondary removes only its local account/group/sudoers.
- After both finish it confirms the account is gone on both bastions and reports
  the result.

> **Active users are never deleted.** Before running, a pre-flight check
> (`who` + `pgrep -u`) runs on **both** bastions; if the user has any login
> session or running process on either one — or the check can't be verified —
> the delete is aborted and the status line reports where and why. The script
> re-checks on the host and refuses (exit 3) as a safety net, and surfaces the
> real `userdel` error if one occurs. No sessions are ever killed; ask the user
> to log out and re-run.

### Vault IAM Access

Creates a Vault policy and an AWS-auth role bound to an IAM role, from a bastion
that can reach the Vault server. Visible to everyone by default — see
`vault_iam.allowed_users` under
[Feature flags (features.json)](#feature-flags-featuresjson) to restrict it.

The dialog has:

| Field | Notes |
|-------|-------|
| **IAM Role** | The full ARN, used verbatim as `bound_iam_principal_arn`. Hint shows `arn:aws:iam::123456789012:role/my-role`. |
| **Vault Policy** | The policy HCL. Hint shows `path "ctt/*" { capabilities = ["read", "write", "list"] }`. |
| **Vault Role Name** | The `auth/aws/role/<name>` path. Defaults to the role name parsed off the ARN; stops tracking once you edit it. |
| **Vault Policy Name** | Defaults to the Vault Role Name; edit it when the policy is shared across roles. |
| **Environment** | Selects the bastions **and** the pre-filled VAULT_ADDR. Always shown uppercase. |
| **Primary / Secondary Bastion** | Same env-filtered dropdowns and same `config.ini` caching as above. |
| **VAULT_ADDR** | Pre-filled from the environment's `vault_addr`, falling back to the account-level one; editable, and required if neither is set. |
| **VAULT_TOKEN** | Masked, typed per run, **never stored** — not in `config.ini`, not in `features.json`. |

Unlike the user scripts this runs on the **primary bastion only** (Vault is a
shared server, so a second write would be redundant); the secondary is used only
if the primary session won't open. It also runs as the **logged-in SSM user** —
no `sudo su` — since Vault authenticates by token, not by OS user.

On the bastion it exports `VAULT_ADDR`/`VAULT_TOKEN`, writes the policy, writes
the role with `resolve_aws_unique_id=true`, `token_ttl=0s`, `token_max_ttl=24h`,
`max_ttl=24h`, then reads the policy and the role back. A success/failure popup
reports the verdict with the captured terminal output under **Details**.

> **Token handling.** The export line is sent with a leading space under
> `HISTCONTROL=ignorespace` so it stays out of the remote shell history, the
> token is passed base64-encoded rather than as a literal, and the screen is
> cleared immediately after — the same hygiene the git PAT flow uses. Note that
> `clear` only wipes the visible screen; the encoded value can still sit in that
> tab's scrollback. The `vault` binary must be on the bastion's PATH for the SSM
> user, otherwise you get "command not found" and a failure popup.

### Vault IAM Delete (admin-gated)

The flip of Vault IAM Access, for tearing down a test role before re-running the
create. **Hidden unless enabled at build time** — it needs a username on *both*
`vault_iam.allowed_users` and `vault_iam.delete_allowed_users` (see
[Feature flags](#feature-flags-featuresjson)); the delete list ships empty, so
being able to create never implies being able to delete.

Same dialog minus the **IAM Role** and **Policy** boxes — it removes objects
rather than describing them. You supply the Vault role name, the Vault policy name, the
environment, the bastions, and the Vault address and token, then tick the
confirmation before the **Delete** button enables.

On the primary bastion it runs `vault delete auth/aws/role/<name>` and
`vault policy delete <name>`, lists the remaining roles so you can see it gone,
and confirms that **neither** object reads back before reporting success. Token
handling is identical to the create — the two paths share the same export
prelude.

> Deleting something already absent is not an error: Vault's delete is
> idempotent and the verdict checks the end state rather than the delete's exit
> code, so a half-finished earlier run still converges on success.
>
> The policy is always removed alongside the role. If you point this at a policy
> shared by another role, that role loses its policy too.

The script sources live in `assets/scripts/` (`create_new_user.sh`,
`delete_user.sh`) and are compiled into the binary; edit them and rebuild to
change behavior. The Vault commands are built in-app, not from a script file.

## Feature flags (features.json)

Build-time feature gates live in `assets/features.json`, which is compiled into
the binary (like `accounts.json`). An admin edits the file and **rebuilds** —
end users cannot change these at runtime. This is intentional for destructive
actions.

```json
{
  "allow_delete_user": false,
  "primary_bastion_filter": "bastion",
  "secondary_bastion_filter": "bastion",
  "vault_iam": {
    "allowed_users": ["*"],
    "delete_allowed_users": []
  },
  "access_email": {
    "enabled": true,
    "auto_run": true,
    "email_domains": ["xyz.com", "old-xyz.com"],
    "encrypt_template_guid": "{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}",
    "encrypt_permission": 3,
    "encrypt_permission_service": 1,
    "encrypt_smime_flag": 0,
    "encrypt_sendkeys": "%6"
  }
}
```

| Field                      | Default     | Description |
|----------------------------|-------------|-------------|
| `allow_delete_user`        | `false`     | Exposes the destructive `delete_user.sh` entry in the Scripts menu. |
| `primary_bastion_filter`   | `"bastion"` | Substring that narrows the **Primary Bastion** dropdown (matches instance name or id, case-insensitive). Empty shows all. |
| `secondary_bastion_filter` | `"bastion"` | Same, for the **Secondary Bastion** dropdown. |
| `vault_iam.allowed_users`  | `["*"]`     | OS usernames that see the **Vault IAM Access** entry. `["*"]` = everyone, `[]` = nobody, or list specific usernames (case-insensitive). |
| `vault_iam.delete_allowed_users` | `[]`  | OS usernames that additionally see the destructive **Vault IAM Delete** entry. Requires membership of `allowed_users` too. Ships empty — nobody. |
| `access_email.enabled`     | `true`      | Master switch for the whole access-email feature. `false` disables the automatic send and hides the **✉ Send Email Command** menu. See [Access email](#access-email-post-create). |
| `access_email.auto_run`    | `true`      | Run `send_access_email.ps1` automatically once a create finishes with a saved PEM. `false` leaves the ✉ menu as the only route. |
| `access_email.email_domains` | `[]`      | Your organization's **mail** domains, e.g. `["xyz.com", "old-xyz.com"]`. A recipient resolving outside all of them is never mailed unattended — this is what stops a stale local Contacts entry or autocomplete hit from receiving the PEM. Staff often have mail on several domains; this is unrelated to the Windows/AD domain the machine is joined to. A single string or a comma-separated string also works, as does the older `email_domain` key. Empty skips the check. |
| `access_email.email_local_format` | `"flast"` | Shape the address must have for the username. `flast` = first initial + surname with an optional number, so `john.smith` accepts `jsmith@` or `jsmith2@` but not `johnsmith@`. Catches an in-domain address belonging to a different person — which the domain check cannot. Empty skips it. |
| `access_email.email_local_suffixes` | `[".cw"]` | Markers the local part may **also** carry, e.g. `test.user` matching `tuser@` *and* `tuser.cw@`. Accepted alongside the bare form, never instead of it, and **probed** as well as checked — `tuser`, `tuser.cw`, `tuser2`, `tuser2.cw`… — so a marked mailbox is found by address rather than falling back to display-name resolution. Taken literally, so `-contractor` or `_ext` work; a single string or a comma-separated string also works. Empty leaves the plain stem-plus-number shape. |
| `access_email.encrypt_template_guid` | *(placeholder)* | Your **Microsoft 365 tenant's** RMS/IRM template GUID, braces included. Ships as an all-zeros placeholder that must be replaced — see [Finding your template GUID](#finding-your-template-guid). |
| `access_email.encrypt_permission` | `3`    | The `MailItem.Permission` value your Encrypt button applies (`2` = Do Not Forward). `0` skips it. |
| `access_email.encrypt_permission_service` | `1` | `MailItem.PermissionService` (`1` = olWindows). Needed alongside the template GUID. |
| `access_email.encrypt_smime_flag` | `0`    | S/MIME encrypt flag, used only when there is no template GUID (`1` = encrypt). |
| `access_email.encrypt_sendkeys` | `"%6"`   | Your Outlook QAT Encrypt shortcut, for the visible fallback path (`Alt+6` = `%6`). |

Parsing **fails closed**: if the file is malformed, every gate defaults to off.
To ship a build for admins who need user deletion, set `"allow_delete_user": true`
and rebuild.

The build also refuses placeholder values in any section that is switched on —
see [The build refuses a still-template configuration](#the-build-refuses-a-still-template-configuration).

### Enabling Vault IAM Access

It is on for everyone in the shipped default, so there is nothing to switch on —
but it needs one thing to be useful:

1. Set `vault_addr` on each account in `assets/accounts.json` that has a Vault
   server (see [Account configuration](#account-configuration-accountsjson)).
   Without it the VAULT_ADDR box opens blank and has to be typed each run.
2. Rebuild — both files are compiled into the binary.

To restrict it instead, replace `["*"]` with the usernames who should have it
(or `[]` to hide it from everyone) and rebuild.

### Enabling Vault IAM Delete

This one is off for everyone in the shipped default, deliberately:

1. Add the usernames to `vault_iam.delete_allowed_users` in
   `assets/features.json`. They must also be covered by `allowed_users` — the
   gate requires both, so nobody gets a destructive Vault action without the
   create it undoes.
2. Rebuild.

`["*"]` works here too, but think before using it: the entry deletes a Vault
role *and* its policy.

## Access email (post-create)

**Windows only**, and it needs Outlook installed and signed in.

After a Bastion New User run finishes *and the PEM was saved*, the app runs
`send_access_email.ps1` for you (`access_email.auto_run`, on by default). That
script composes the email in Outlook, attaches the PEM, encrypts it, and:

| Outcome | What happens |
|---|---|
| Exactly one directory match, in an `email_domains` entry, address fits `email_local_format` | Encrypted and **sent in the background** — no window |
| Two or more people match the name | Opens the draft, **To field empty**, listing who matched |
| Nobody matches | Opens the draft, **To field empty** |
| The directory could not be searched | Opens the draft and says so — never falls back to a weaker check |
| One match, address outside every `email_domains` entry | Opens the draft, **To empty**, naming the address it found |
| One match, but the address does not fit the username | Opens the draft, **To empty** — `test.user` expects `tuser@` (or a configured suffix such as `tuser.cw@`), not `testuser@` |
| Encryption could not be confirmed | Opens the draft and applies your `Alt+6` shortcut |

The duplicate-name count is an LDAP **Ambiguous Name Resolution** query — the
same resolution Outlook's suggestion dropdown uses — **not**
`Recipient.Resolve()`, which reports success even when several people share a
name. The mail is addressed by SMTP address so Outlook never re-picks.

The result popup shows which of these happened, so a silent send is still
visible. Nothing is ever sent unattended without a confirmed single in-domain
recipient **and** confirmed encryption — the attachment is a private key.

The **✉ Send Email Command** menu remains as a manual fallback: it copies a
ready-to-run command for WSL, Git Bash or PowerShell, useful for re-sending or
when the automatic attempt fails.

> Two things in the spawn are deliberate EDR hygiene and should not be
> "tidied": the script is run **from the file next to the exe** (never written
> to `%TEMP%` and run from there), and `-WindowStyle Hidden` is **not** used. A
> brief console window is the accepted cost. `scripts/build_binaries.sh` copies
> the script next to the `.exe` rather than embedding it, for the same reason.

Set `access_email.auto_run` to `false` to leave the ✉ menu as the only route.

### Finding your template GUID

Encryption is the one thing that must be configured per organization: the script
applies the same encryption your Outlook **Options > Encrypt** button applies,
which means it needs your tenant's RMS/IRM template GUID.

1. In Outlook, open **New Email** and click **Options > Encrypt** (or your
   `Alt+6` shortcut). **Leave that draft open — do not send it.**
2. From a source checkout, in PowerShell:

   ```powershell
   powershell -NoProfile -ExecutionPolicy Bypass -File assets\scripts\outlook_verification.ps1
   ```

   It finds the draft whether you composed in a pop-out window or inline in the
   reading pane, prints what the Encrypt button set, and ends with a
   **WHAT THIS MEANS** block naming the exact `features.json` values for your
   case.
3. Copy the GUID **with its braces** into `access_email.encrypt_template_guid`
   in `assets/features.json`, set `encrypt_permission` to the `Permission`
   number it printed, and **rebuild** — features.json is compiled in.

Reading the output:

| What you see | Meaning | What to set |
|---|---|---|
| A `Permission` number **and** a GUID | Tenant RMS/IRM template | `encrypt_permission` = that number, `encrypt_permission_service` = `1`, `encrypt_template_guid` = the GUID |
| `Permission = 2`, no GUID | Do Not Forward | `encrypt_permission: 2` |
| `Permission = 0`, no GUID, S/MIME flag `1` | S/MIME | `encrypt_smime_flag: 1` |
| No GUID, but **Sensitivity label: set** | Purview label / OME "Encrypt-Only" | No GUID exists — use the visible path: `encrypt_template_guid: ""`, `encrypt_permission: 0` |

The GUID belongs to your **Microsoft 365 tenant**, not your machine — it is the
same for everyone in your org and only changes if IT rebuilds the template.

#### If the GUID comes back blank

Blank is a real answer, not always a failure. Work down this list:

- **Everything blank, including `Permission`** — the script found no draft, or
  you ran the older one-liner that only sees a *pop-out* compose window. Composing
  inline in the reading pane leaves `ActiveInspector()` empty. The current script
  handles all three locations (pop-out, inline, newest item in Drafts); rerun it.
- **`Sensitivity label: set`, no GUID** — your Encrypt button applies a Purview
  sensitivity label or OME "Encrypt-Only". **These do not expose a template GUID
  at all**, so there is nothing to find and headless encryption cannot be set
  through the object model. This is the expected modern-tenant result. Set
  `encrypt_template_guid: ""` and `encrypt_permission: 0`; every access email
  then opens in Outlook with your `encrypt_sendkeys` shortcut applied, ready for
  a one-click Send.
- **Everything blank and no label either** — the Encrypt may not have applied,
  or your tenant only stamps it on save. Click into the body, type a character,
  press `Ctrl+S`, and rerun.
- **`Could not attach to Outlook`** — the **new Outlook for Windows has no COM
  object model**. Toggle "New Outlook" off (top-right of the Outlook window) to
  get classic Outlook, then rerun.

Blank in the label case costs you nothing but the extra click: the email is still
composed, attached and encrypted, it just isn't sent unattended.

To verify the value end-to-end before you rebuild (it re-reads the GUID from the
open draft, echoes it untruncated, then encrypts and sends a test to yourself
with no window and no `Alt+6`):

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File assets\scripts\test_headless_encrypt.ps1 -Username first.last
```

> Setting `PermissionTemplateGuid` prints `The operation failed` even when it
> works — the value still sticks and applies at send time. That is why the
> scripts confirm by **reading the GUID back**, never by the setter's error.

Full procedure, including what to do when headless encryption won't confirm:
[`ACCESS_EMAIL_WALKTHROUGH.md`](ACCESS_EMAIL_WALKTHROUGH.md).


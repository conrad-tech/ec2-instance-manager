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

### create_new_user.sh

- **Grant sudo (NOPASSWD:ALL)** checkbox (off by default) passes `--sudo`.
- Runs `create_new_user.sh` on the **primary** (creates the user and generates a
  PEM key), and mirrors the matching UID/GID `groupadd`/`useradd` (and sudoers,
  if `--sudo`) on the **secondary** from the shared EFS home.
- After both finish it verifies SSH login **primary → secondary** and
  **secondary → primary** as the new user, then pulls the generated PEM back to
  your **local Downloads folder** as `<username>-<MMODAL_ENV>.pem` (the
  `MMODAL_ENV` tag is read from the primary bastion).
- On success the status line reports all tests passed and the saved PEM path.

### delete_user.sh (admin-gated)

This entry is **hidden unless enabled at build time** (see
[Feature flags (features.json)](#feature-flags-featuresjson)).

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

Parsing **fails closed**: if the file is malformed, every gate defaults to off.
To ship a build for admins who need user deletion, set `"allow_delete_user": true`
and rebuild.

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


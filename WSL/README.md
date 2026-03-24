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
- `dist/windows/ec2_manager.exe`
- `dist/windows/ec2_manager_gui.exe`

Expected files (Windows — cross-compiled from Linux with MinGW):
- `dist/windows/ec2_manager.exe`
- `dist/windows/ec2_manager_gui.exe`
- `dist/windows/libgcc_s_seh-1.dll`
- `dist/windows/libstdc++-6.dll`
- `dist/windows/libwinpthread-1.dll`

Expected files (Linux):
- `dist/linux/ec2_manager`
- `dist/linux/ec2_manager_gui`

## Launch Desktop GUI (Pop!_OS 24.04)

From source:

```bash
cargo run --features gui --bin ec2_manager_gui
```

From built binary:

```bash
./dist/ec2_manager_gui
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
./dist/windows/ec2_manager_windows/ec2_manager_gui.exe --mode sim

# Windows (PowerShell/CMD)
.\dist\windows\ec2_manager_windows\ec2_manager_gui.exe --mode sim

# Linux
./dist/windows/ec2_manager_windows/ec2_manager_gui.exe --mode sim
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
   - `ec2_manager.exe`
   - `ec2_manager_gui.exe`
2. No DLLs needed — everything is statically linked.

#### Cross-compiled from Linux (MinGW)

1. Copy the entire `dist/windows/` folder to your Windows machine.
2. Keep the `.exe` files and the three DLLs in the **same folder**:
   - `ec2_manager.exe`
   - `ec2_manager_gui.exe`
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
   - `ec2_manager_gui.exe` (defaults to `--mode live`)


If Windows reports a missing DLL, it means the DLL is not next to the `.exe`.

### Exactly where DLL files go

Place the DLLs in the **same directory** as the executable you run.

Example target layout on Windows:

```text
C:\tools\ec2-manager\windows\
  ec2_manager.exe
  ec2_manager_gui.exe
  libgcc_s_seh-1.dll
  libstdc++-6.dll
  libwinpthread-1.dll
```

Run from that folder:

```powershell
cd C:\tools\ec2-manager\windows
.\ec2_manager_gui.exe
```

Do not put these DLLs in `C:\Windows\System32`; keep them beside the `.exe`.

### Windows sim-mode quick check

```powershell
# From project root
.\dist\windows\ec2_manager_gui.exe --mode sim
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
    "color":      "#2ea043"
  },
  {
    "label":      "QA",
    "account_id": "234567890123",
    "region":     "us-east-1",
    "sort_order": 2,
    "color":      "#c8b41e"
  },
  {
    "label":      "Staging",
    "account_id": "345678901234",
    "region":     "us-east-1",
    "sort_order": 3,
    "color":      "#e69600"
  },
  {
    "label":      "Prod-A",
    "account_id": "456789012345",
    "region":     "us-east-1",
    "sort_order": 4,
    "color":      "#c82828"
  },
  {
    "label":      "Prod-B",
    "account_id": "567890123456",
    "region":     "us-west-2",
    "sort_order": 5,
    "color":      "#aa1e46"
  }
]
```

### Fields

| Field        | Required | Description |
|-------------|----------|-------------|
| `label`      | Yes      | Display name shown in the UI |
| `account_id` | Yes      | AWS account ID (used as profile identifier) |
| `region`     | No       | Default AWS region for this account |
| `sort_order` | No       | Display order in legend and dropdowns (lower = first). Omit for alphabetical. |
| `color`      | No       | Hex color code for tab coloring (e.g. `"#2ea043"`). Omit for auto-assignment. |

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


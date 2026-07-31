# EC2 Manager — Windows

A desktop tool for browsing your AWS EC2 instances and connecting to them over
AWS **Session Manager (SSM)** — no public IPs, SSH keys, or bastion hops to
manage by hand.

## Before you start: install WSL (one-time)

The app runs its AWS tooling inside **WSL** (Windows Subsystem for Linux) using
**Ubuntu 24.04**. If you've never used WSL, set it up once before launching the
app:

1. **Open PowerShell as Administrator.** Click **Start**, type `PowerShell`,
   right-click **Windows PowerShell**, and choose **Run as administrator**.
2. **Update WSL first.** This avoids a stale distribution catalog (the most
   common cause of "no Ubuntu shows up"):
   ```powershell
   wsl --update
   ```
   If `wsl` isn't recognized at all, run `wsl --install --no-distribution`
   first, **reboot**, then continue.
3. **Install Ubuntu 24.04:**
   ```powershell
   wsl --install -d Ubuntu-24.04 --web-download
   ```
   The **`--web-download`** flag downloads Ubuntu directly instead of through the
   Microsoft Store — important on work laptops where the Store is disabled by IT
   policy (that block is why `wsl --list --online` can come back with **no Ubuntu
   listed**). On a personal machine you can drop `--web-download`.
4. **Reboot if prompted.** The first-time install often asks you to restart
   Windows to finish enabling WSL.
5. **Create your Linux user.** When the Ubuntu window opens, it finishes
   installing and asks you to create a **UNIX username** and **password**.
   Remember this password — it's the **sudo password** the app asks for on first
   launch.
6. **Confirm it worked.** Back in PowerShell, run:
   ```powershell
   wsl --list --verbose
   ```
   You should see `Ubuntu-24.04` listed with `VERSION 2`.

> **`wsl --list --online` shows no Ubuntu?** Your WSL is outdated or the
> Microsoft Store catalog is blocked. Run `wsl --update`, then install with the
> `--web-download` flag as shown above. As a last resort, open the **Microsoft
> Store** and install **"Ubuntu 24.04.1 LTS"** directly (if the Store isn't
> blocked).
>
> **Already have WSL?** If you already run a different distribution, that's
> usually fine, but this app is tested against **Ubuntu 24.04**. Install it
> alongside your existing setup with the command above and make it the default:
> `wsl --set-default Ubuntu-24.04`.

## Setup

1. **Unzip the download.** Right-click `ec2_manager_windows.zip` →
   **Extract All…** (or use your preferred unzip tool). Keep every file together
   in the one extracted folder — don't move the `.exe` out on its own.
2. Open the extracted **`ec2_manager_windows`** folder.
3. Double-click **`ec2_manager_gui_1.1.exe`** (the GUI app) to launch it.
4. On first launch it installs AWS CLI + the Session Manager plugin inside your
   **Ubuntu 24.04** WSL distribution — enter your WSL sudo password (the one you
   set when creating your Ubuntu user) when prompted. This is a one-time step and
   is remembered afterward.

> Be signed in to AWS first (your usual `fed` / Okta login) so the app can see
> your accounts. It reads credentials from `%USERPROFILE%\.aws\credentials` and
> refreshes automatically when you re-authenticate.

## What it does

- **Browse EC2 inventory** across multiple AWS accounts, with fast search and
  filters (by name, tag, instance id, state, SSM-managed, and more).
- **Connect to instances** through SSM in an embedded terminal — one tab per
  connection, color-coded by environment. No public IPs or SSH keys required.
- **Browse & edit remote files** with a built-in file browser and editor —
  drag-and-drop upload, one-click download, inline save.
- **Inspect instance details** — type, AMI, private IP, IAM role, EBS volumes,
  tags, Auto Scaling group, SSM status, and launch time.
- **Save filters and favorites**, and search several accounts at once.
- **Bastion user management (Scripts menu)** — create a Linux user across a
  primary/secondary bastion pair, generate and install its SSH key, verify it,
  and pull the private key (PEM) to your **Downloads** folder automatically. Some
  builds also allow guarded user deletion.
- **Vault IAM Access (Scripts menu)** — create a Vault policy and an AWS auth
  role bound to an IAM role from a bastion, then read both back to confirm it
  took. You supply the Vault token each time; it is never saved.

## Learn more

See **`WALKTHROUGH.md`** in this folder for a full, feature-by-feature guide —
searching, filtering, connecting, the file browser, the Scripts menu, keyboard
shortcuts, and troubleshooting.

## Troubleshooting

- **"WSL is not installed"** (or the app shows an `Initialize WSL` button) —
  WSL isn't set up yet. Open an **admin** PowerShell, run `wsl --update`, then
  `wsl --install -d Ubuntu-24.04 --web-download`, reboot if asked, create your
  Ubuntu username and password, then relaunch the app. See **Before you start:
  install WSL** above.
- **`wsl --install -d Ubuntu-24.04` fails / `wsl --list --online` shows no
  Ubuntu** — your WSL is outdated or the Microsoft Store is blocked by IT policy.
  Run `wsl --update`, then install with the **`--web-download`** flag:
  `wsl --install -d Ubuntu-24.04 --web-download`.
- **`wsl --install` isn't recognized** — run `wsl --install --no-distribution`,
  reboot, then retry. (Updating Windows also brings in WSL.)
- **Windows SmartScreen** may warn about an unrecognized app — click
  **More info → Run anyway** (it's an unsigned internal tool).
- **Connections are slow or hang** — open PowerShell, run `wsl --shutdown`, then
  relaunch the app. This is common after Windows or WSL updates.
- If the app can't find your accounts, confirm you're authenticated to AWS and
  that `%USERPROFILE%\.aws\credentials` is populated.

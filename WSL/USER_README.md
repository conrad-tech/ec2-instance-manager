# EC2 Manager — Windows

A desktop tool for browsing your AWS EC2 instances and connecting to them over
AWS **Session Manager (SSM)** — no public IPs, SSH keys, or bastion hops to
manage by hand.

## Setup

1. **Unzip the download.** Right-click `ec2_manager_windows.zip` →
   **Extract All…** (or use your preferred unzip tool). Keep every file together
   in the one extracted folder — don't move the `.exe` out on its own.
2. Open the extracted **`ec2_manager_windows`** folder.
3. Double-click **`ec2_manager_gui_1.0.exe`** (the GUI app) to launch it.
4. On first launch it installs AWS CLI + the Session Manager plugin inside WSL —
   enter your WSL sudo password when prompted. This is a one-time step and is
   remembered afterward.

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

## Learn more

See **`WALKTHROUGH.md`** in this folder for a full, feature-by-feature guide —
searching, filtering, connecting, the file browser, the Scripts menu, keyboard
shortcuts, and troubleshooting.

## Troubleshooting

- **Windows SmartScreen** may warn about an unrecognized app — click
  **More info → Run anyway** (it's an unsigned internal tool).
- **Connections are slow or hang** — open PowerShell, run `wsl --shutdown`, then
  relaunch the app. This is common after Windows or WSL updates.
- If the app can't find your accounts, confirm you're authenticated to AWS and
  that `%USERPROFILE%\.aws\credentials` is populated.

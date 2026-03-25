# EC2 Manager GUI v1.0 - Walkthrough

## Getting Started

### First Launch
1. Double-click `ec2_manager_gui_1.0.exe` to open the application
2. On first launch, a **WSL Setup** dialog will appear automatically
3. Enter your WSL sudo password to install AWS CLI and Session Manager Plugin inside WSL
4. Once setup completes, the app caches the result so you won't be prompted again

---

## Main Interface

### Account Profile
- Use the **Account Profile** dropdown on the left panel to switch between AWS accounts
- Only authenticated accounts (valid credentials) are selectable
- Switching accounts clears any multi-account selections

### Refreshing Inventory
- **Refresh** — reloads EC2 instances for the current account
- **Refresh All** — reloads all authenticated accounts in parallel (useful for multi-account lookup)

---

## Filtering & Searching

### Search Rules
- Type a search term in the filter box (e.g., `bastion`, `prod`, an instance ID, or an AMI ID)
- Use **Include** to show matching instances, **Exclude** to hide them
- Click **+ Rule** to add multiple search terms
- Use `TagKey: value` syntax for tag-specific filtering (e.g., `Environment: production`)

### State Filter
- Use the **State** dropdown to filter by instance state: running, stopped, terminated, etc.

### Only SSM-managed
- Check this to show only instances with SSM agent connected

### Multi-Account Lookup
- Below the **Only SSM-managed** checkbox, use the **Multi-account Lookup** dropdown
- Check other authenticated accounts to merge their instances into your current view
- Search terms apply across all selected accounts (e.g., search `bastion` to see bastions from all accounts)
- When a checked account finishes loading, its instances appear automatically
- All authenticated accounts are pre-loaded from disk cache on startup for instant multi-account searches
- Switching the **Account Profile** clears multi-account selections
- **Refresh All** ensures all accounts have fresh data

### Saved Filters
- After setting up your filters, type a name and click **Save Current**
- Select a saved filter from the **Choose Filter** dropdown — it **auto-applies** immediately
- **Show Favorites** is a built-in filter that shows only your starred instances for the current account
- If you modify a filter while it's selected, click **Save Current** (it will update the existing filter using the selected filter's name)
- Click the **x** next to a filter name to delete it (the dropdown stays open)
- Click **Clear Filters** to reset everything
- Saved filters are listed in alphabetical order

### Reset Filter on Profile Switch
- By default, switching accounts clears your active filters and saved-filter selection
- To keep filters active across account switches, go to **Edit > Reset Filter on Profile Switch** and toggle it off
- Example: set up a `bastion` filter, switch from Account A to Account B — with reset enabled (default), the filter clears; with reset disabled, the filter stays applied and shows Account B's matching instances

### Favorites & Pinned Filters
- Click the star icon next to any instance to favorite it
- Favorites are saved per-account and persist across sessions
- When saving a filter with favorites starred, only those specific instances (pinned IDs) are shown when the filter is applied
- If no favorites are starred when saving, the filter shows all matching results
- Example: search `bastion`, star 2 out of 6, save as "My Bastions" — selecting that filter shows only the 2 starred bastions

---

## Environment Colors

### Color Legend
- The color legend appears on the **Connections** page, showing environment names from the `MMODAL_ENV` tag
- Colors are derived from the account's base color in `accounts.json` — darker and lighter shades per environment
- Each environment name always gets the same shade (deterministic, consistent across users)
- Environments are ordered by account `sort_order`, then alphabetically within each account
- Single environment per account uses the exact base color from `accounts.json`

### Exclude Env
- Use the **Exclude Env** dropdown (next to the Log tab) to hide specific environments from the legend and connection tab coloring
- Check/uncheck environments to show/hide them
- Exclusions are saved and persist across app restarts

### Connection Tab Colors
- Each connection tab's border and fill color matches its instance's environment shade
- Falls back to the account's base color if no `MMODAL_ENV` tag is found

---

## Instance Details

- **Right-click** any instance in the inventory and select **See Details**
- Opens the **Details** tab with comprehensive instance information:
  - Instance ID, Name, State, Instance Type, AMI ID, Private IP
  - Availability Zone, Environment (from `MMODAL_ENV` tag)
  - IAM Role (fetched on-demand from the instance profile)
  - Auto Scaling Group, SSM status, Launch Time
- **Volumes** section shows attached EBS volumes with:
  - Volume ID, Size, Type, Device, State, and Attach Time
  - Fetched automatically when Details opens
- **Tags** section lists all instance tags in alphabetical order
- **Copy All** button copies all details as formatted text to clipboard
- Click **Close** to return to the Inventory page

---

## Connecting to an EC2

1. Select an instance from the list (click on it or type its Instance ID)
2. Click **Connect** or double-click the instance row
3. A new tab opens in the **Connections** panel with an embedded terminal
4. The terminal connects via SSM (AWS Systems Manager Session Manager)

### Terminal Shell
- **WSL** and **PowerShell** are available as terminal options
- WSL is recommended for SSM sessions (uses credentials from Windows `~/.aws/credentials`)
- Select your preferred shell from the terminal dropdown on the left panel

### Update PS1
- Click **Update PS1** to set a colored prompt showing user, host, and working directory
- This also runs `clear` to clean up the terminal

### Multiple Connections
- Connect to multiple instances across different accounts simultaneously
- Each connection gets its own tab, colored by environment
- The private IP is shown in the connection summary bar (works across all account profiles)

---

## File Browser

The file browser is the left sidebar in the Connections panel.

### Navigation
- The file browser defaults to `/home/ec2-user`
- Type a full path in the path bar and press **Enter** or click **Go** to navigate directly (e.g., `/etc/cassandra/conf` — one API call instead of clicking through each directory)
- Click **.. (Up)** to go to the parent directory
- Click **Refresh** to reload the current directory and all expanded subdirectories (collapsed subdirectories refresh in the background)

### Tree View
- Directories show a triangle arrow: click the **arrow** to expand/collapse a directory inline
- **Double-click** a directory name to navigate into it
- Subdirectories are prefetched in the background for faster browsing (expanded dirs have priority)
- Previously visited directories load instantly from cache

### Opening Files
- **Double-click** a file to open it in the inline editor (status shows "Initializing...")
- The editor appears above the terminal with a **draggable splitter** between them
- Default split is 50/50 when the first file opens
- Drag the blue splitter bar to resize the editor and terminal areas

### Inline Editor
- Edit files directly with line numbers and monospace font
- **Ctrl+S** or click **Save** to save changes back to the EC2 (status shows "Updating...")
- Multiple files can be open as tabs across the top
- Click **x** on a tab to close it
- A `*` appears next to the filename when there are unsaved changes
- Status indicators: "Saved" (green) on success, error message (red) on failure

### Download & Upload
- **Single-click** a file in the tree to select it
- Click **Download** to save it to your local machine (status shows "Downloading...")
- Click **Upload** to upload a local file to the current directory (status shows "Uploading...")
- **Drag and drop** files from Windows Explorer onto the file browser to upload

---

## Window Management

- The app remembers its **position, size, and maximized state** when closed
- Reopening the app restores it to the same monitor and layout
- First launch defaults to maximized on the main monitor

---

## Application Log

- The **Log** tab shows application events with color-coded severity levels
- Filter by level: ERROR, WARN, INFO, DEBUG, TRACE
- **Copy All** button copies all visible log lines to clipboard
- **Highlight text** in the log and use **Ctrl+C** to copy specific lines

---

## Keyboard Shortcuts

| Shortcut | Action |
|----------|--------|
| `Ctrl+S` | Save the active editor file |
| `Ctrl+C` | Copy selected text (log panel, editor) |
| `Enter` | Navigate to typed path in file browser |
| Click terminal area | Focus terminal for keyboard input |
| Right-click terminal | Copy selected text, or paste if nothing selected |
| Scroll wheel | Scroll terminal history |

---

## Troubleshooting

### WSL connections are slow or timing out
If logging into an EC2 via WSL takes much longer than usual or appears stuck, WSL may need a restart:

```powershell
wsl --shutdown
```

Then relaunch the app. This is common after Windows or WSL updates and resets the WSL virtual machine.

### Connection fails with "config profile not found"
If WSL connections fail but PowerShell connections work, WSL environment variable forwarding (WSLENV) may be broken. Run `wsl --shutdown` and retry. See the README troubleshooting section for a diagnostic test.

---

## Tips

- Use **Refresh All** before using multi-account lookup to ensure all accounts have the latest data
- Type full paths directly in the path bar instead of clicking through directories — saves multiple API calls
- The directory cache makes revisiting paths instant — use Refresh to force a fresh listing
- If the file browser gives an SSM error on first try, click Refresh or Go to retry
- Use **Update PS1** after connecting to get a clean colored prompt (also clears the terminal)
- Use `clear` in the terminal before running vim to avoid display issues after exiting
- All AWS API calls for the file browser run silently (no console window flashes)
- If WSL connections are slow, try `wsl --shutdown` in PowerShell and relaunch the app

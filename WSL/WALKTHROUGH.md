# EC2 Manager GUI - Walkthrough

## Getting Started

### First Launch
1. Double-click `ec2_manager_gui_1.0.exe` to open the application
2. On first launch, a **WSL Setup** dialog will appear automatically
3. Enter your WSL sudo password to install AWS CLI and Session Manager Plugin inside WSL
4. Once setup completes, the app caches the result so you won't be prompted again

### Accounts Setup
- Edit `assets/accounts.json` before building to configure your AWS accounts:
```json
[
  {
    "label": "Dev",
    "account_id": "123456789012",
    "region": "us-east-1",
    "sort_order": 1,
    "color": "#2ea043"
  }
]
```
- The `label` is the display name in the app
- The `account_id` must match the account ID in your `~/.aws/credentials` `fed_role` ARN
- The `color` sets the tab border color for connections from that account

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

### Saved Filters
- After setting up your filters, type a name and click **Save Current**
- Select a saved filter from the dropdown — it **auto-applies** immediately
- **Show Favorites** is a built-in filter that shows only your starred instances
- If you modify a filter while it's selected, click **Save Current** (it will update the existing filter)
- Click **Clear Filters** to reset everything

### Favorites
- Click the star icon next to any instance to favorite it
- Favorites are saved per-account and persist across sessions
- When saving a filter with favorites starred, only those specific instances are shown when the filter is applied
- If no favorites are starred when saving, the filter shows all matching results

---

## Multi-Account Lookup

- Below the **Only SSM-managed** checkbox, use the **Multi-account Lookup** dropdown
- Check other authenticated accounts to merge their instances into your current view
- Search terms apply across all selected accounts (e.g., search `bastion` to see bastions from all accounts)
- When a checked account finishes loading, its instances appear automatically
- Switching the **Account Profile** clears multi-account selections
- **Refresh All** ensures all accounts have fresh data for multi-account searches

---

## Connecting to an EC2

1. Select an instance from the list (click on it or type its Instance ID)
2. Click **Connect** or double-click the instance row
3. A new tab opens in the **Connections** panel with an embedded terminal
4. The terminal connects via SSM (AWS Systems Manager Session Manager)

### Terminal Shell
- **WSL** and **PowerShell** are available as terminal options
- WSL is recommended for SSM sessions (uses Linux `aws` CLI inside WSL)
- Select your preferred shell from the terminal dropdown on the left panel

### Update PS1
- Click **Update PS1** to set a colored prompt showing user, host, and working directory
- This also runs `clear` to clean up the terminal

### Multiple Connections
- Connect to multiple instances across different accounts simultaneously
- Each connection gets its own tab, colored by account
- The private IP is shown in the connection summary bar

---

## File Browser

The file browser is the left sidebar in the Connections panel.

### Navigation
- The file browser defaults to `/home/ec2-user`
- Type a full path in the path bar and press **Enter** or click **Go** to navigate directly
- Click **.. (Up)** to go to the parent directory
- Click **Refresh** to reload the current directory and all expanded subdirectories

### Tree View
- Directories show an arrow icon: click the **arrow** to expand/collapse a directory inline
- **Double-click** a directory name to navigate into it
- Subdirectories are prefetched in the background for faster browsing
- Expanded directories refresh when you click Refresh

### Opening Files
- **Double-click** a file to open it in the inline editor
- The editor appears above the terminal with a draggable splitter between them
- Default split is 50/50 when the first file opens

### Inline Editor
- Edit files directly with line numbers and monospace font
- **Ctrl+S** or click **Save** to save changes back to the EC2
- Multiple files can be open as tabs across the top
- Click **x** on a tab to close it
- A `*` appears next to the filename when there are unsaved changes

### Download & Upload
- **Single-click** a file in the tree to select it
- Click **Download** to save it to your local machine
- Click **Upload** to upload a local file to the current directory
- **Drag and drop** files from Windows Explorer onto the file browser to upload

---

## Window Management

- The app remembers its **position, size, and maximized state** when closed
- Reopening the app restores it to the same monitor and layout
- First launch defaults to maximized on the main monitor

---

## Keyboard Shortcuts

| Shortcut | Action |
|----------|--------|
| `Ctrl+S` | Save the active editor file |
| `Enter` | Navigate to typed path in file browser |
| Click terminal area | Focus terminal for keyboard input |
| Right-click terminal | Copy selected text, or paste if nothing selected |
| Scroll wheel | Scroll terminal history |

---

## Tips

- Use **Refresh All** before using multi-account lookup to ensure all accounts have fresh data
- Type full paths like `/etc/cassandra/conf` directly in the path bar instead of clicking through directories
- The directory cache makes revisiting paths instant — use Refresh to force a fresh listing
- If the file browser gives an SSM error on first try, click Refresh or Go to retry
- Use `clear` in the terminal before running vim to avoid display issues after exiting

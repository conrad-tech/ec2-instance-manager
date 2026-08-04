# Access Email Setup - Walkthrough

After a user is created, the app composes the "bastion access" email in Outlook,
encrypts it, and - when the recipient resolves to exactly one person in your
mail domain - sends it in the background. Otherwise Outlook opens with the
email ready and the To field empty. This is **Windows only** and relies on
Outlook being installed and signed in.

The result popup reports which of those happened, so a silent send is still
visible. A **✉ Send Email Command** menu is also there as a manual fallback: it
copies a ready-to-run command for WSL, Git Bash or PowerShell.

> Set `access_email.auto_run` to `false` in features.json to turn the automatic
> run off and leave the ✉ menu as the only route.

The one thing that must be set up per organization is **encryption**: the app
applies the same encryption your **Options > Encrypt** button applies, via the
Outlook object model. It can only do that once it knows your tenant's encryption
template. This walkthrough discovers those values and verifies the path.

All commands run in **PowerShell** (no admin needed). The helper scripts live in
`assets/scripts/`:
- `test_resolve_recipient.ps1` - dry run: who would a username be mailed, and
  is the name really ambiguous? Sends nothing, creates no draft.
- `outlook_verification.ps1` - read what your Encrypt button sets
- `test_headless_encrypt.ps1` - confirm headless encryption works, auto-grab the GUID
- `test_access_email.ps1` - send a full test email to yourself
- `send_access_email.ps1` - the script the app itself runs after a create

> **Encoding note:** the `.ps1` files must be plain ASCII. Windows PowerShell
> reads scripts as ANSI, so a stray "smart" character (em dash, curly quote)
> breaks parsing. If you hand-edit one and get a "string is missing the
> terminator" error, run:
> ```powershell
> ((Get-Content file.ps1 -Raw) -replace '[^\x00-\x7F]','-') | Set-Content -Encoding ASCII file.ps1
> ```

---

## Step 1 - Discover your encryption values

1. Open Outlook -> **New Email**.
2. Click **Options > Encrypt** the way you normally do (or your **Alt+6**
   shortcut). Leave that compose window **open** - do not send it.
3. Switch to PowerShell (the compose window can go behind - that's fine).
4. From `assets\scripts`, run:
   ```powershell
   powershell -NoProfile -ExecutionPolicy Bypass -File outlook_verification.ps1
   ```
5. Note what it prints, e.g.:
   ```
   Permission             : 3
   PermissionTemplateGuid : '{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}'
   S/MIME security flag   : 0
   ```

**How to read it:**

| What you see | Meaning | Config to use |
|---|---|---|
| `Permission` + a GUID | Tenant RMS/IRM template | `encrypt_permission` = that number, `encrypt_permission_service` = 1, `encrypt_template_guid` = the GUID |
| `Permission = 2`, no GUID | Do Not Forward | `encrypt_permission: 2` |
| `Permission = 0`, no GUID, `S/MIME = 1` | S/MIME | `encrypt_smime_flag: 1` |
| No GUID, `Sensitivity label: set` | Purview label / OME "Encrypt-Only" | No GUID exists - see Step 4 |

The script ends with a **WHAT THIS MEANS** block that names your case and the
exact values to use, so you do not have to read the table.

**If it prints a blank GUID:**

- **Everything blank, `Permission` included** - no draft was found. The script
  checks the pop-out compose window, the inline reading-pane compose, and the
  newest item in Drafts, so make sure a draft actually exists and rerun.
- **`Sensitivity label: set` with no GUID** - normal for modern tenants. Purview
  sensitivity labels and OME "Encrypt-Only" **never expose a template GUID**;
  there is nothing to discover. Go to Step 4 and use the visible path.
- **Blank with no label at all** - the Encrypt may not have stuck. Type a
  character in the body, press `Ctrl+S`, rerun.
- **`Could not attach to Outlook`** - the *new* Outlook for Windows has no COM
  object model. Toggle "New Outlook" off to get classic Outlook, then rerun.

> The `PermissionTemplateGuid` is tied to your **Microsoft 365 tenant**, not your
> machine - it's the same for everyone in your org, and only changes if IT
> rebuilds the template. **Keep the braces** `{...}`.

---

## Step 2 - Confirm headless encryption works

This proves the app can encrypt without opening a window, and grabs the full
GUID for you (no copy/paste, no terminal truncation).

1. Keep an encrypted draft open (from Step 1).
2. Run:
   ```powershell
   powershell -NoProfile -ExecutionPolicy Bypass -File test_headless_encrypt.ps1 -Username first.last
   ```

It reads the template GUID from your open draft, echoes the **full value**, then
builds a second email and encrypts it headless (Permission -> PermissionService
-> GUID) and **sends it to you**. Look for:
```
Template GUID     : '{...}'   <-- full value; paste this into features.json
Read-back -> Permission: 3   TemplateGuid: '{...}'
headless encryption confirmed (Permission AND GUID set): True
SENT to yourself HEADLESS (no window, no Alt+6). Check your inbox - is it ENCRYPTED?
```

Then **check your inbox**:
- **Arrived encrypted** -> headless works. Copy the echoed GUID for Step 3.
- **Arrived unencrypted / didn't send** -> see Step 4.

> The setter for `PermissionTemplateGuid` prints `The operation failed` even when
> it works - the value still sticks and applies at send time. That's why the
> scripts confirm by **reading the GUID back**, not by the setter's error.

---

## Step 3 - Put the values in the build config

Edit `assets/features.json`, fill in the `access_email` block, then rebuild:

```json
"access_email": {
  "enabled": true,
  "auto_run": true,
  "email_domain": "xyz.com",
  "encrypt_template_guid": "{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}",
  "encrypt_permission": 3,
  "encrypt_permission_service": 1,
  "encrypt_smime_flag": 0,
  "encrypt_sendkeys": "%6"
}
```

- All three of `encrypt_permission`, `encrypt_permission_service` (1), and
  `encrypt_template_guid` are needed for headless encryption.
- `enabled` - set `false` to turn the whole feature off.
- `auto_run` - set `false` to stop the app running the script itself, leaving
  the ✉ Send Email Command menu as the only route.
- `email_domain` - your organization's mail domain. A recipient that resolves
  to an address outside it is never mailed unattended, which is what stops a
  stale local Contacts entry from receiving the PEM. Leave empty to skip.
- `encrypt_sendkeys` - your QAT Encrypt shortcut, used only on the visible
  fallback path (Alt+6 = `"%6"`).

Rebuild so the config is baked in (see `BUILD_SETUP.txt`):
```bash
cargo build --features gui        # or the release build script
```

> Replace the placeholder GUID with your real one. Leaving the all-zeros
> placeholder will make encryption confirmation misbehave.

---

## What happens after a user is created

Once a create finishes and passes verification (user created on both bastions,
SSH cross-login OK, PEM pulled to `~/Downloads`), the app runs
`send_access_email.ps1` itself. This only happens when the PEM was saved (the
email body promises it as an attachment) and both `access_email.enabled` and
`access_email.auto_run` are `true`.

The script:

1. Composes the email:
   - **To:** `firstname.lastname` -> `Firstname Lastname`, resolved against the
     Global Address List (like typing in the To field).
   - **Subject:** `Bastion Access for <ENV>` (ENV = the primary bastion's
     MMODAL_ENV tag, uppercased).
   - **Body:** greeting with the first name, the username, both bastion instance
     IDs, and a signature using your Outlook profile's first name.
   - **Attachment:** the generated PEM.
2. Then, depending on the recipient:

| Case | What happens |
|---|---|
| **One match, address in your `email_domain`** | Encrypts headless and **sends silently** - no compose window. Prints `SENT recipient='...' address='...'`. |
| **One match, address outside `email_domain`** | Opens the draft with the **To field empty** and names the address it found. Guards against a stale Contacts entry receiving the PEM. |
| **Two or more people share the name, or nobody matches** | Opens the draft with the **To field empty** so nobody is pre-selected. One message covers both - Outlook cannot tell them apart without a full directory scan. |
| **Encryption could not be confirmed** | Opens the draft and presses your Alt+6 shortcut. Only ever pressed when headless encryption failed, since Alt+6 is a toggle. |

The result popup in the app shows which of these happened. On the automatic run
the script is passed `-Quiet`, so it does not also raise its own message boxes;
run it by hand from the ✉ menu and the boxes come back.

> **Autocomplete is not the ambiguity test.** Typing a name in Outlook's To
> field lists *substring* matches, so seeing several entries does not mean the
> name is ambiguous - "Test User2" and "Tester Userson" both appear while you
> type "test user". What matters is whether more than one person has that
> **exact** display name. To check a specific username without sending
> anything:
>
> ```powershell
> powershell -NoProfile -ExecutionPolicy Bypass -File assets\scripts\test_resolve_recipient.ps1 -Username first.last -Domain xyz.com
> ```
>
> It prints what `Resolve()` does, the address it would use, whether that entry
> is a real directory user or a local Contact, and counts exact display-name
> matches - then tells you whether the behavior is correct or a bug worth
> reporting.

The private key is only sent automatically when the recipient resolves to one
person, that person's address is in your configured `email_domain`, **and**
encryption is confirmed on the item. Alt+6 is only ever pressed on a
not-yet-encrypted draft (it's a toggle), so it never accidentally un-encrypts.

---

## Step 4 - If headless encryption won't confirm

If Step 2 sends but arrives unencrypted (some sensitivity-label / OME
"encrypt-only" tenants), the app still works - it just always takes the visible
path: it opens the fully composed draft, presses your **Alt+6** shortcut, and
leaves it for a one-click Send. To force that behavior, set
`encrypt_template_guid` to `""` and `encrypt_permission` to `0` in
`features.json`; every email then opens for manual Send instead of auto-sending.

# Access Email Setup - Walkthrough

After a user is created, the app hands you a ready-to-run command that composes
the "bastion access" email in Outlook, encrypts it, and - when the recipient is
unambiguous - sends it. **You run that command yourself**; the app only copies
it to your clipboard. This is **Windows only** and relies on Outlook being
installed and signed in.

> Why the extra step: running the Outlook automation from your own shell keeps
> it out of the unsigned GUI process, which is what stops EDR (CrowdStrike) from
> quarantining the app. It is not a limitation to be engineered away.

The one thing that must be set up per organization is **encryption**: the app
applies the same encryption your **Options > Encrypt** button applies, via the
Outlook object model. It can only do that once it knows your tenant's encryption
template. This walkthrough discovers those values and verifies the path.

All commands run in **PowerShell** (no admin needed). The helper scripts live in
`assets/scripts/`:
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
| `Permission` + a GUID | Tenant RMS/IRM template (most common) | `encrypt_permission` = that number, `encrypt_permission_service` = 1, `encrypt_template_guid` = the GUID |
| `Permission = 2`, no GUID | Do Not Forward | `encrypt_permission: 2` |
| `Permission = 0`, no GUID, `S/MIME = 1` | S/MIME | `encrypt_smime_flag: 1` |
| `Permission = 0`, no GUID, `S/MIME = 0` | Sensitivity-label only | headless may not work - see Step 4 |

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
SSH cross-login OK, PEM pulled to `~/Downloads`), the result popup grows an
**✉ Send Email Command** menu. Pick your terminal (WSL / Git Bash / PowerShell)
and the command is copied to your clipboard - the popup confirms with "Command
copied. Now run it in your terminal." Paste it into that terminal and run it.

The menu only appears when the PEM was saved (the email body promises it as an
attachment) and `access_email.enabled` is `true`.

Running the command:

1. Composes the email:
   - **To:** `firstname.lastname` -> `Firstname Lastname`, resolved against the
     Global Address List (like typing in the To field).
   - **Subject:** `Access for <ENV> Bastion EC2s` (ENV = primary bastion's
     MMODAL_ENV tag).
   - **Body:** greeting with the first name, the username, both bastion instance
     IDs, and a signature using your Outlook profile's first name.
   - **Attachment:** the generated PEM.
2. Then, depending on the recipient:

| Case | What happens |
|---|---|
| **Exactly one match** | Encrypts headless and **sends silently** - no compose window. The script pops up "Email sent successfully" and prints `SENT recipient='...'`. |
| **Two or more people share the name (or none match)** | **Opens the Outlook window, presses Alt+6 to encrypt, and leaves it open** so you pick the correct recipient and click Send. A popup explains why. |
| **One match but headless encryption can't be confirmed** | Opens the window, presses Alt+6, and leaves it for you to verify and Send. |

The private key is only sent automatically when the recipient is unambiguous
**and** encryption is confirmed on the item. Alt+6 is only ever pressed on a
not-yet-encrypted draft (it's a toggle), so it never accidentally un-encrypts.

---

## Step 4 - If headless encryption won't confirm

If Step 2 sends but arrives unencrypted (some sensitivity-label / OME
"encrypt-only" tenants), the app still works - it just always takes the visible
path: it opens the fully composed draft, presses your **Alt+6** shortcut, and
leaves it for a one-click Send. To force that behavior, set
`encrypt_template_guid` to `""` and `encrypt_permission` to `0` in
`features.json`; every email then opens for manual Send instead of auto-sending.

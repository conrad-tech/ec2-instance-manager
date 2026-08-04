# Access email — background send — design

Date: 2026-08-04
Status: approved, ready to implement

## Summary

`send_access_email.ps1` already sends unattended when the recipient resolves to
one person **and** encryption confirms — that path exists and, with a valid
template GUID in `features.json`, should already work. This change is not about
adding background send; it is about making it trustworthy and correcting what it
sends.

Four substantive changes:

1. **Domain verification.** A resolved recipient must be in the organization's
   own mail domain before anything is sent unattended. Today any resolution is
   trusted, including one from the local Contacts folder.
2. **Empty To field on every failure.** Today an ambiguous name is left sitting
   in the To field; it must be cleared so nobody is pre-selected.
3. **Encryption applied even when the recipient does not resolve,** so a draft
   that opens is already encrypted instead of depending on `Alt+6` landing.
4. **New subject and body,** with the environment uppercased.

Plus one new config value, the mail domain, in `features.json`.

Out of scope: the app still does **not** run the automation itself. The GUI
builds a command, the user copies it from the **✉ Send Email Command** menu and
runs it in their own terminal. That boundary is what keeps EDR off the unsigned
GUI process and is not revisited here.

## Part 1 — Recipient resolution

`john.smith` becomes the display name `John Smith`, which is handed to Outlook's
`Recipient.Resolve()`.

| Outcome | Action |
|---|---|
| Resolves, and the address is in the configured domain | Encrypt headless, **send in the background** |
| Resolves, but the address is in another domain | Open Outlook, To empty, error naming the address found |
| Does not resolve (no match, or two or more people share the name) | Open Outlook, To empty, one combined error |

`Resolve()` returns true **only** for a single unambiguous match; two people
named John Smith return false. That is the whole ambiguity check — there is no
GAL scan and no LDAP lookup.

### Why 0 and 2+ share one message

Telling "nobody matched" from "several matched" is not something `Resolve()`
reports. Getting it would need either a full GAL enumeration (accurate but
seconds-to-minutes on a large directory) or an AD/LDAP query (fast, but a new
dependency and a domain-joined machine). Neither changes what the user does
next: Outlook opens with an empty To field and they pick the right person. One
message covers both.

This is not a latency problem that a head start would solve. The "exactly one"
answer — the only one that gates sending — is immediate. Nor could the search be
started early: the Rust app never talks to Outlook, so there is no process in
which to pre-warm anything.

### Why the domain check earns its place

`Resolve()` can succeed against the local Contacts folder or the autocomplete
nickname cache, not just the GAL. A stale personal entry for "John Smith"
pointing at a personal address would otherwise resolve cleanly and get a private
key mailed to it. Requiring the resolved address to sit in the configured domain
closes that.

The address is read from `Recipient.AddressEntry.GetExchangeUser().PrimarySmtpAddress`,
falling back to `Recipient.Address` when the entry is not an Exchange user. The
comparison is case-insensitive on the text after the final `@`.

A blank domain disables the check, preserving today's behavior for anyone who
has not configured one.

## Part 2 — Encryption and the send decision

Encryption is applied **before** the send/open decision rather than only on the
resolved path, so a draft that opens is already encrypted rather than depending
on the `Alt+6` keystroke landing.

The existing toggle guard is load-bearing and stays: `Alt+6` is sent **only**
when headless encryption was not confirmed. `Alt+6` toggles, so pressing it on
an already-encrypted item would strip the encryption.

The item is sent unattended only when **all three** hold:

1. `Resolve()` succeeded
2. the resolved address is in the configured domain
3. encryption read back confirmed on the item

Failing any of them opens the draft. A private key never leaves unattended
without confirmed encryption — this is the point of the feature, not a
configurable trade-off.

## Part 3 — Subject and body

Subject:

```
Bastion Access for DEV1
```

The environment is uppercased **in the script** (`$EnvTag.ToUpper()`), so the
subject is correct however the `MMODAL_ENV` tag happens to be cased. This
matches how `vault_env_label` handles the same tag for the Vault dialogs.

Body:

```
Hello John,

See below for your login credentials and attached is your PEM file.

Username: john.smith

Primary Bastion: i-0abc...

Secondary Bastion: i-0def...

Thanks,
Brandon
```

- Greeting uses the first name alone, title-cased.
- The signature is the sender's **first name**, read from the Outlook profile's
  `CurrentUser.Name`, handling both `Last, First` and `First Last` orderings.
- The PEM is attached when the path passed in still exists.

## Part 4 — Configuration

`assets/features.json` gains one field in the `access_email` block:

```json
"access_email": {
  "enabled": true,
  "email_domain": "xyz.com",
  "encrypt_template_guid": "{...}",
  "encrypt_permission": 3,
  "encrypt_permission_service": 1,
  "encrypt_smime_flag": 0,
  "encrypt_sendkeys": "%6"
}
```

- `AccessEmailConfig` (`src/features.rs`) gains `pub email_domain: String`,
  defaulting to `""`.
- `build_email_command` (`src/bin/ec2_manager_gui.rs`) appends
  `-Domain <email_domain>` to the argument list, quoted per shell like every
  other value.
- `send_access_email.ps1` gains `[string]$Domain = ""`.

Blank is the fail-open case here, deliberately: an admin who has not set a
domain gets today's behavior rather than a feature that silently stops sending.

## Error messages

All three open the draft with an empty To field.

| Case | Message |
|---|---|
| No single match | "Could not identify a single recipient for 'John Smith' — either nobody matches or more than one person does. The email is ready below; enter the correct recipient and click Send." |
| Wrong domain | "'John Smith' resolved to jsmith@other.com, which is not in xyz.com. The email is ready below; enter the correct recipient and click Send." |
| Encryption unconfirmed | "The email is ready but encryption could not be confirmed automatically. Applying your Encrypt shortcut now — verify the email shows as encrypted, then click Send." |

Each is shown in the existing `Show-Box` dialog and echoed on stdout, so a user
running the command in a terminal sees it without the dialog.

The stdout line keeps its machine-readable shape:
`OPEN recipient='John Smith' resolved=$resolved domain_ok=$domainOk encrypted=$encConfirmed`,
with `SENT recipient='John Smith' address='jsmith@xyz.com'` on the send path.

## Testing

Rust side, in the existing `gui::tests` module:

- `-Domain` appears in both the bash and PowerShell command forms.
- A blank `email_domain` still emits the flag with an empty value, so argument
  positions never shift.
- Existing `build_email_command` tests extended rather than duplicated.

PowerShell side there is no automated coverage and none is proposed — the script
drives a live Outlook COM object. It cannot run in this repo's Linux CI, and a
mock would test the mock. **The behavior must be confirmed by hand on a Windows
box with Outlook**, in four runs:

1. A name that resolves to one person in the domain → sends with no window.
2. A name shared by two people → opens, To empty, combined message.
3. A name that matches nobody → opens, To empty, combined message.
4. `email_domain` set to something the resolved address is not in → opens, To
   empty, domain message.

The first run must be checked in the recipient's mailbox for the encryption
banner, not just for arrival.

## Rollback

Tag `pre-email-readd-58a9b9a` remains the pre-email baseline. This change sits
on top of the restored integration; reverting just this work means reverting its
commits, not the tag.

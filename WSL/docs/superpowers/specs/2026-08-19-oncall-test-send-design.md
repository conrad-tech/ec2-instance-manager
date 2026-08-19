# On-call test button, and the escalation send path

Date: 2026-08-19
Status: approved for implementation.

Implements build-order step 1 of
`2026-08-14-escalation-notifier-design.md` — *"the pipe, with a manual
trigger"* — and gives the corporate side its first way to send anything at
all.

## Problem

`ec2_manager_gui` already decides that an automatic fix failed. `reaper.rs`
produces an outcome code, `ReaperEvent::Outcome` pushes it into
`pending_notify`, and **nothing drains that field.** Verified: there is no
send path anywhere in the app.

So the chain designed on 2026-08-14 — app to mailbox to Pi to phone — has
never run, and cannot be tested. The Pi-side daemon is written and has 116
passing tests, but every one of them proves logic rather than delivery.

This builds the missing half: a gated button that sends one real escalation
email, so the whole chain can be exercised on demand.

## Scope

**In scope:** the send path, the recipient's configuration, the gate, the
dropdown, and the result feedback.

**Out of scope, deliberately:** draining `pending_notify` so that *real*
remediation outcomes send mail. That becomes a small change once this exists —
the seam is already there and the plan's own self-review named it as such — but
it arms an automatic outbound path, and it deserves its own decision rather
than arriving as a side effect of a test button.

## What is sent

One email. **The subject is the entire payload and the body is empty.**

No instance id, no AWS account number, no environment, no product name, no
alert text. This is not a summarisation choice; it is the org-boundary
constraint from the parent design, which the reaper module already states in
code:

> What leaves the org. The code is the entire payload — the body is empty. No
> instance id, account number, environment or product name.
> — `src/reaper.rs`, `OutcomeCode`

The test sends the **escalating** code, `RE-F`. That is the tier that rings a
phone, and proving the phone rings — with the ringer off — is the entire point
of the exercise. A quiet tier would prove strictly less.

**The code is taken from `OutcomeCode::Failure.as_str()`, never written as a
literal.** A literal here could drift from the vocabulary the daemon matches
on, and the failure would be silent: mail sends, nothing recognises it, the
Pi escalates it as an unknown code and it *looks* like it worked.

## The recipient

A Windows Credential Manager entry, following the pattern `src/jsm_auth.rs`
already establishes for values that are not secret but must not be committed:

| | |
|---|---|
| Credential target | `ec2_manager/escalation_mailbox` |
| Field used | password only; the username is a placeholder, because `cmdkey` requires one |
| Environment override | `ESCALATION_MAILBOX` |
| Precedence | environment, then credential store |

This mirrors `CLOUD_ID_TARGET`, `SCHEDULE_ID_TARGET` and
`ATLASSIAN_ACCOUNT_ID_TARGET`, which store cloud ids and schedule ids the same
way and for the same reason.

**Blank means the feature is unavailable, never a default destination.** An
address nobody configured must not become an address the app invents. This is
the same fail-closed posture as `allow_delete_user` and the Alerts button.

Deliberately **not** in `features.json` (committed, so a personal address would
enter the corporate repo) and **not** in `config.ini` (plain text on disk, and
it would let any user aim the app at any external address).

**A blank-but-set environment variable is treated as absent**, so
`set ESCALATION_MAILBOX=` cannot shadow a working stored value. `jsm_auth.rs`
already makes this distinction and says why.

## The gate

`features.json` gains:

```json
"on_call_test": {
  "allowed_users": []
}
```

Shipped empty, so the button is hidden until an admin opts users in — the same
posture as `alerts` and `user_sync`, using the shared `user_in_list` helper so
the match rules cannot drift between features.

**The button is also hidden when no recipient resolves.** A visible control
that cannot work is worse than an absent one: it invites a click, does
nothing, and the user has no way to tell whether the failure was theirs or the
app's.

## The dropdown

A Rust enum, one variant today:

```rust
enum TestTarget { Reaper }
```

rendered in a `ComboBox`, with the outcome code obtained from the variant.

**Deliberately not a `features.json`-driven registry.** A configuration schema
for a single entry has to be guessed at before there is a second case to learn
from, and each future source brings its own code vocabulary — so the schema
guessed now is the one most likely to be wrong. Adding a source later is
adding a variant; promoting it to configuration later is easy, and by then
there will be a real second case to shape it.

## The send path

A new `assets/scripts/send_escalation.ps1`, creating an Outlook `MailItem`
with the coded subject and an empty body.

**It must not route through `send_access_email.ps1`.** That script's
recipient-resolution gates — directory user, domain allow-list, local-format
match — exist because it attaches a **private key**. A fixed configured
address with no attachment must not inherit them, and those gates must not be
relaxed to accommodate this. It gets its own script and its own path.

Spawned exactly as `launch_access_email` spawns its script:

- `powershell.exe -NoProfile -ExecutionPolicy Bypass -File <path>`
- **run from the file beside the exe**, never written to `%TEMP%` and run from
  there
- **`CREATE_NO_WINDOW`**, and **no `-WindowStyle Hidden`**

The last two are not style preferences. Both are patterns EDRs quarantine on
sight, and this application has a CrowdStrike quarantine in its history; the
existing notes call the substitution load-bearing — no console *and* no
flagged PowerShell switch.

`build_binaries.sh` copies the script beside the exe in `package_windows_zip`,
alongside the existing `send_access_email.ps1` copy, rather than embedding it —
for the same EDR reason.

## Result feedback

The script prints a status marker on stdout; the GUI parses it and shows the
outcome. This mirrors the existing access-email flow (`parse_email_marker`,
`EmailStatus`), so there is one idiom for "a PowerShell script reported back"
rather than two.

The result must distinguish **sent** from **failed**, and a failure must name
what went wrong — no Outlook profile, no recipient configured, send refused.
"Something went wrong" sends the user nowhere.

## Platform

Outlook automation is Windows-only. The Linux development build must stay
warning-free without weakening the check where it ships, so the same treatment
the access-email code already uses applies:
`#[cfg_attr(not(target_os = "windows"), allow(dead_code))]` on the types only
the Windows build constructs.

**Cross-compile before trusting a change here.** The existing notes record
that `launch_access_email` fails to compile only on the Windows target,
because the crate's own `Result<T>` alias is in scope and takes one parameter.
A new spawn function in the same file is exposed to the same trap.

## Testing

Pure and testable, with no Outlook and no network:

- The gate: allowed and disallowed users, an empty list, and `"*"`.
- Recipient resolution: environment beats credential store; blank-but-set
  environment does not shadow a stored value; nothing configured yields
  nothing.
- The button is hidden when the gate fails **or** when no recipient resolves.
- The subject carries `OutcomeCode::Failure.as_str()`, asserted against
  `reaper::OutcomeCode` rather than against a literal, so the two cannot
  drift apart silently.
- The body is empty, and the composed mail carries no instance id, account
  number, environment or product name.
- Marker parsing: sent, each failure reason, and an unrecognised line.

Not unit tested, by the same precedent as the access-email work: the actual
spawn and the Outlook interaction. Those are covered by running it.

## What this does not resolve

**The chain is still unproven until the Pi side is deployed.** The daemon
exists and is tested but is not on the box, and it currently reads a config
file rather than the environment variables now chosen for it. Both are
separate, already-specified work.

Until then this button sends mail to a mailbox nothing is polling.

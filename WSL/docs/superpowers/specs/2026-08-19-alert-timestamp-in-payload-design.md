# The alert timestamp crosses the boundary

Date: 2026-08-19
Status: approved for implementation.

Amends the content-free rule in `2026-08-14-escalation-notifier-design.md`.
Everything else in that document stands.

## What changes

The escalation email's subject becomes:

```
<CODE> <alert createdAt, RFC 3339 UTC>
```

for example `RE-F 2026-08-19T20:12:09Z`. The body stays empty.

The Pi-side daemon renders that timestamp in **local time** in the Telegram
message it sends.

## Why, and what it costs

**The problem.** When a remediation fails, the notification says only that one
did. If several alerts fire close together there is no way to tell which fix
the message is about, so the record of what happened cannot be tied back to
the alert that caused it.

**The cost, stated plainly.** The parent design's second constraint is that no
corporate detail leaves the org, and it is emphatic:

> The notification is therefore **content-free**: it says an escalation
> happened and nothing more.

That is no longer true. A timestamp is a record of *when an internal system
had trouble*, and it now travels through a personal Gmail account, a Pi in a
house, Telegram's servers, and — once the call leg exists — potentially a
third-party text-to-speech service. Accumulated over months it describes the
operational rhythm of the estate: when things break, how often, at what hours.

This was raised before the decision and the decision was made with it in view.
It is recorded here so that the rule's erosion is deliberate and visible
rather than something a later reader finds by accident.

**What still does not cross.** No instance id, no AWS account number, no
environment name, no product name, no alert text. The payload is now the code
plus a time, and nothing else. The line moved; it did not disappear.

## A limitation accepted on purpose

**A timestamp does not solve the stated problem in the case that motivated
it.** Two alerts arriving in the same second carry the same timestamp and are
indistinguishable on the phone — and alerts firing together is precisely the
scenario described.

`Alert.tiny_id` — JSM's short alert identifier — was offered as the fix. It is
unique by construction, and it leaks *less* than a timestamp, since it is
meaningless outside the org's own Jira and reveals nothing about when anything
happened. It was declined in favour of the timestamp alone.

Recorded so it is not re-proposed as though it were a new idea. If two
same-second alerts ever actually collide in practice, this is the answer.

## Corporate side

`reaper::Target` gains `created_at`, carried from `Alert.created_at` at match
time. That field already stores `createdAt` verbatim as the API returned it
(RFC 3339, UTC) — no reformatting, so nothing can drift.

The subject is composed as `format!("{} {}", code.as_str(), target.created_at)`
with the code from `OutcomeCode::as_str()`, never a literal.

**A blank `created_at` yields the bare code**, exactly as today. The field
comes from a feed that has already been observed serving unrendered templates
and absent tags, so a missing timestamp must degrade to the old behaviour
rather than produce `RE-F ` with a trailing space or, worse, block the send.
An escalation that arrives without a timestamp is worth infinitely more than
one that does not arrive.

**This folds into the on-call test button work** (`2026-08-19-oncall-test-send-design.md`).
There is no send path in the app yet, so this is not a separate change to make
— it is a requirement on the one being built.

## Pi side

The daemon extracts the timestamp from the subject and includes it, rendered in
the Pi's local timezone, in the Telegram message.

Local rather than UTC because the message is read on a phone, often at
3am, and `15:12 CDT` is worth more than `20:12Z` at that moment. This also
matches what the corporate app already does: the Alerts window renders the
feed in local time precisely because the API reports UTC.

**Degradation is one-way and never fatal:**

- No timestamp in the subject → the message sends without one.
- A timestamp that will not parse → the message sends without one, and the raw
  value is logged so the malformation is diagnosable.

A formatting problem must never cost an escalation. This is the same posture
`decode_subject` already takes on the mailbox side, where a mangled subject
still reaches the tier rules rather than being dropped.

## Compatibility, verified rather than assumed

`tier_for_subject` splits on any character that is not a letter, digit or
hyphen, then requires **exactly one** known code among the tokens. A timestamp
contributes only non-code tokens, so the rule is unaffected. Run against the
shipped parser on 2026-08-19:

```
'RE-F 2026-08-19T20:12:09Z'      -> ESCALATE
'RE-K 2026-08-19T20:12:09.482Z'  -> SUCCESS
'RE-C 2026-08-19T20:12:09Z'      -> CANARY
'2026-08-19T20:12:09Z RE-N'      -> QUIET_FAILURE
'RE-F 2026-08-19T20:12:09Z RE-K' -> ESCALATE   (two codes still ambiguous)
```

The last line matters most: adding a timestamp does not weaken the
ambiguity-escalates rule.

**Fractional seconds and a trailing `Z` are both handled** by the tokenizer, so
the exact shape JSM returns does not need pinning down in advance.

## Testing

**Corporate side**

- `Target` carries `created_at` from the matched alert.
- The subject is `<code> <created_at>`, with the code taken from
  `OutcomeCode::as_str()` and asserted against `reaper::OutcomeCode` rather
  than a literal, so the two cannot drift.
- A blank `created_at` yields the bare code with no trailing space.
- The body is still empty, and the subject still carries no instance id,
  account number, environment or product name.

**Pi side**

- A subject with a timestamp produces a Telegram message containing it,
  rendered in local time.
- A subject without one produces the message unchanged.
- An unparseable timestamp produces the message without it, and logs the raw
  value.
- The tier decision is unchanged in every case above — pinned by a test, since
  that is the property a future edit to the message format could silently
  break.

## What this does not change

The four tiers, the ladder, the acknowledgement, the canary, the collapse rule
for simultaneous escalations, and the empty body. Only the subject grew, by one
field, deliberately.

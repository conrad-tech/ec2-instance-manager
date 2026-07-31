# Vault IAM Access — design

Date: 2026-07-31
Status: approved, ready to implement

## Summary

Add a third entry to the GUI's **Scripts** menu, **Vault IAM Access**, that
creates a Vault policy and an AWS-auth role bound to an IAM role from a bastion,
then reads both back to verify.

Delivering it requires a second, larger change: several AWS accounts host **two
environments**, told apart by each instance's `MMODAL_ENV` tag. The Scripts
dialogs currently select an *account*. They must select an *environment*, and
the cached bastion pair must be keyed per environment. That applies to
`create_new_user.sh` and `delete_user.sh` as well, so it is specified here
first.

## Part 1 — Environments

### accounts.json

Accounts gain an optional `environments` array and an optional account-level
`vault_addr`:

```json
{
  "label": "Dev",
  "account_id": "123456789012",
  "region": "us-east-1",
  "environments": [
    { "name": "DEV1", "vault_addr": "https://vault.dev1.example.com:8200" },
    { "name": "DEV2", "vault_addr": "https://vault.dev2.example.com:8200" }
  ]
}
```

- `environments[].name` must match the instances' `MMODAL_ENV` tag, compared
  case-insensitively and trimmed.
- `environments[].vault_addr` is optional; it falls back to the account-level
  `vault_addr`, then to blank.
- Both fields are optional everywhere. Existing files keep parsing — no struct
  in `src/` uses `deny_unknown_fields`, so this is additive.

`ProfileConfig` (`models.rs`) is deliberately **not** extended: it flows into
config.ini persistence and the account tab UI, neither of which needs this.
`accounts.rs` instead exposes two lookups reading the same bundled blob:

```rust
pub fn environments_for(account_id: &str) -> Vec<AccountEnvironment>;
pub fn vault_addr_for(account_id: &str, env: &str) -> Option<String>;
```

### The environment list

`src/script_env.rs` (new) owns the selection model, free of egui so it is unit
testable:

```rust
pub struct ScriptEnv {
    pub account_id: String,
    pub account_label: String,
    pub env: String,      // "" = account has no environment dimension
    pub label: String,    // "Dev — DEV1", or "Prod" when the account has one env
}
```

Built as the **union** of:

1. environments declared in `accounts.json` for that account, and
2. every distinct `MMODAL_ENV` value in that account's loaded inventory.

Rules:

- Dedup case-insensitively; the declared spelling wins when both exist.
- Sort declared entries first in declaration order, then discovered ones
  alphabetically.
- If the union is **empty** (nothing declared, no tagged instances), emit a
  single entry with `env: ""` — the account itself. This is the pre-existing
  behavior for untagged accounts and must not regress.
- If the union has exactly **one** entry, `label` is the bare account label, not
  `Account — ENV`. A single-environment account gains no information from the
  suffix.

### Bastion dropdowns

`bastion_combo_ui` gains an environment parameter. An instance is offered when:

- `env` is empty (untagged account — no environment filter, as today), **or**
- its `MMODAL_ENV` tag equals `env`, case-insensitively and trimmed;

**and** it passes the existing `primary_bastion_filter` /
`secondary_bastion_filter` substring match from `features.json`.

The environment filter is **never relaxed**. The existing fallback — when the
configured substring matches nothing, retry with `"bastion"` — stays, but only
within the selected environment. An environment with no matching bastion shows
an empty dropdown. Silently offering another environment's bastion would let a
user create or delete an account on the wrong boxes.

### Cache

`config.ini` currently holds `bastion_pair.<account_id>=<primary>|<secondary>`.
New key: `bastion_pair.<account_id>.<env>`. Shared by all three scripts.

- Reads try the per-env key, then fall back to the legacy account-level key.
- Writes always use the per-env key. Accounts with `env: ""` write
  `bastion_pair.<account_id>.` — normalize to the legacy key in that case so
  untagged accounts keep exactly one entry.
- The legacy parse arm in `config.rs` stays, so existing files load unchanged
  and nobody loses a saved pair.

### Knock-on cleanup

`enqueue_user_script` currently derives `mmodal_env` (used in the PEM filename)
by looking up the primary bastion's tag after the fact. With an explicit
environment selection it becomes the selected environment. Keep the tag lookup
as a fallback for the `env: ""` case.

## Part 2 — Vault IAM Access

### Gate

`features.json` gains `vault_iam.allowed_users`, matched against the OS username
case-insensitively. `["*"]` = everyone (shipped default), `[]` = nobody. Parsing
fails closed like the other gates.

### Dialog

New modal, styled like `render_create_user_dialog`, reusing `bastion_combo_ui`.

| Field | Behavior |
|---|---|
| IAM Role | Full ARN, verbatim. Hint: `arn:aws:iam::123456789012:role/my-role` |
| Policy | Multiline HCL. Hint: `path "ctt/*" { capabilities = ["read", "write", "list"] }` |
| AWS Role Name | Defaults to the role name parsed off the ARN; stops tracking once edited |
| Policy Name | Defaults to the AWS Role Name; stops tracking once edited |
| Environment | `ScriptEnv` dropdown; changing it resets both bastions and refreshes VAULT_ADDR |
| Primary / Secondary Bastion | Env-filtered dropdowns, shared cache |
| VAULT_ADDR | Pre-filled via `vault_addr_for`; editable |
| VAULT_TOKEN | Masked, per run, never persisted |

Validation before Run: ARN matches `arn:aws:iam::<12 digits>:role/<name>`; policy
body non-empty; both names non-empty and `[A-Za-z0-9_.-]+`; environment and both
bastions chosen; VAULT_ADDR non-empty; token non-empty.

### Execution

Runs on the **primary bastion only** — Vault is a shared server, so a second
identical write is redundant. The secondary is still selected and validated, and
is used only if the primary session cannot be opened. Runs as the **logged-in SSM
user**; no `sudo su`, since Vault authenticates by token rather than by OS user.

Steps, drip-fed through the existing `PendingScriptRun` worker:

```
export HISTCONTROL=ignorespace
 export VAULT_ADDR='<addr>'; export VAULT_TOKEN="$(echo '<b64>' | base64 -d)"; clear
echo '<b64 policy>' | base64 -d | vault policy write <policy_name> -
vault write auth/aws/role/<role_name> bound_iam_principal_arn="<arn>" resolve_aws_unique_id=true policies="<policy_name>" token_ttl=0s token_max_ttl=24h max_ttl=24h
vault policy read <policy_name>
vault read auth/aws/role/<role_name>
if vault policy read <policy_name> >/dev/null 2>&1 && vault read auth/aws/role/<role_name> >/dev/null 2>&1; then echo __VAULT_IAM_OK__; else echo __VAULT_IAM_FAIL__; fi
```

The TTL flags are hardcoded, matching the reference command; changing them needs
a rebuild.

Two encoding decisions:

- **The policy body ships base64-encoded**, like `create_new_user.sh` does, so
  multi-line HCL, quotes, and braces survive the line-at-a-time drip-feed intact.
- **The token is passed base64-encoded**, not as a literal, and the export line
  is sent with a leading space under `HISTCONTROL=ignorespace` so it stays out of
  the remote shell history. `clear` runs immediately after, before the reads, so
  the verification output stays readable.

### Verification

When the tab's steps finish, capture `parser.screen().contents()` — the same hook
`create_new_user` uses — and scan for the sentinel:

- `__VAULT_IAM_OK__` → green success popup naming the role.
- `__VAULT_IAM_FAIL__`, or neither found → red failure popup.

Both carry the captured terminal text under **Details**. A sentinel is used
rather than parsing `vault` output so the verdict does not depend on Vault's
formatting.

## Module layout

`src/bin/ec2_manager_gui.rs` is ~17.8k lines. New logic goes into lib modules
with unit tests, and the GUI file gets only rendering and wiring:

| File | Contains |
|---|---|
| `src/vault_iam.rs` (new) | ARN validation, name derivation, step-plan building, verdict parsing |
| `src/script_env.rs` (new) | Declared ∪ discovered environment union, labelling, dedup, sort |
| `src/accounts.rs` | `environments_for`, `vault_addr_for` |
| `src/config.rs` | Per-environment bastion pair cache + legacy fallback |
| `src/features.rs` | `vault_iam.allowed_users` |
| `src/bin/ec2_manager_gui.rs` | Dialog rendering, env-filtered bastion combo, run/verify wiring |

## Testing

Unit tests (no GUI needed):

- `script_env` — union, case-insensitive dedup, declared-wins, single-env
  labelling, empty-union fallback to `env: ""`, ordering.
- `accounts` — environments parse, env-level `vault_addr` wins over
  account-level, missing both yields `None`, existing files without either field
  still parse.
- `config` — per-env round trip, legacy key read fallback, `env: ""` normalizes
  to the legacy key.
- `vault_iam` — ARN accept/reject cases, role name derived from ARN, step plan
  contains the base64 round-trip of a multi-line policy, verdict parsing for OK /
  FAIL / neither.
- `features` — gate defaults, `["*"]`, `[]`, named user, malformed file fails
  closed.

GUI tests follow the existing patterns for dialog validation. Full check:
`cargo test --features gui`, `cargo clippy --features gui`, and a
`cargo build --features gui`.

## Explicitly out of scope

- Environment selection anywhere outside the Scripts dialogs. The Inventory
  page, account tabs, legend, and colors stay per-account.
- Editable TTLs, a Vault-address picker, or reading Vault config from the
  bastion.
- Storing the Vault token anywhere.

## Known limitations

- `clear` wipes only the visible screen; the base64-encoded token can remain in
  that tab's scrollback. This is the same tradeoff the existing git PAT flow
  accepts.
- `vault` must be on the bastion's PATH for the SSM user. If it is not, the run
  produces "command not found" and a failure popup rather than a more specific
  diagnostic.
- Base64 is obfuscation, not encryption. It keeps the token off the visible
  command line and out of shell history; it does not protect it from anyone who
  can read the scrollback.

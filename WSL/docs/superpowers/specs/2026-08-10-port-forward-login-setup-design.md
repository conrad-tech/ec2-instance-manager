# Login setup and test for the Port Forwards window

Date: 2026-08-10
Status: approved (design)

## Problem

The Port Forwards window shows why an environment is not forwarding, and then
leaves the user stranded:

```
On   Environment   Status                                                    Bastion
[x]  AUCT          no pem saved for this environment —                       i-0785b06144b6c50b
                   open a box in VS Code once to choose one          [Clear]
```

`start_port_tunnel` needs three things beyond an authorized account — a saved
**bastion**, a **login user**, and a **pem** — and none of them can be set from
the window that reports them missing. Two of the five failure strings it
produces are dead ends of exactly this kind: the message above, and `no bastion
saved for this environment — pick one in a Scripts dialog first`. Both send the
user somewhere else to fix a thing this window already knows is wrong.

The same gap applies to a working environment that stops working. A terminated
bastion, a deleted login, a rotated key — each leaves a saved configuration that
is now wrong, and there is no way to correct it from here. So the affordance is
**not** a repair prompt on broken rows; it is an editor available on every row.

Second problem, discovered while writing this: there is no way to find out
whether a login works short of switching the tunnel on and watching the window.
The session is hidden by design, so a bad pem is indistinguishable from a bad
bastion until the tunnel dies and its stderr surfaces.

## Approach

The test must exercise the real thing. A cheap `ssh -o BatchMode=yes … true`
probe would prove the login works while proving nothing about port binding —
which is where `ExitOnForwardFailure=yes` kills tunnels — so the test spawns
**the identical session the tunnel spawns**, via `Tunnel::spawn`, with the real
`-L` forwards.

That fidelity is structural rather than a promise to keep two code paths in
sync: the branch of `start_port_tunnel` from "resolve bastion" to "spawn" is
extracted, and both callers use it.

## resolve_tunnel_launch

`start_port_tunnel` splits. Everything from resolving the bastion through
writing the managed block becomes:

```rust
fn resolve_tunnel_launch(&mut self, row: &PortForwardRow)
    -> std::result::Result<TunnelLaunch, String>
```

returning `{ alias, forwards, signature }` and producing the same five failure
strings it produces today, unchanged — they are matched on elsewhere
(`why.contains("needs authorizing")` in `render_port_forwards_window`) and are
what `tunnel_error_dismissed` keys on, so a reworded string silently un-dismisses
every error a user has cleared.

`start_port_tunnel` keeps its behaviour exactly. The test calls the same
function, so it cannot pass on a configuration the real start would reject.

## The dialog

A fifth grid column carries a `Login…` button **on every row**, whatever its
status. `PortForwardLoginDialog` holds:

```rust
struct PortForwardLoginDialog {
    key: String,            // vscode_key(account_id, env)
    account_id: String,
    env: String,
    label: String,
    primary_id: String,
    primary_label: String,
    primary_query: String,
    secondary_id: String,
    secondary_label: String,
    secondary_query: String,
    user: String,
    pem: String,
    test: TestState,
}
```

Four fields, all reusing widgets that exist:

- **Primary bastion** — `bastion_combo_ui`, given the instance list narrowed to
  this environment, exactly as the Scripts dialogs narrow theirs. **This is the
  one the tunnel connects through**; the secondary plays no part in forwarding.
- **Secondary bastion** — the same widget, seeded from
  `secondary_bastion_filter`. It is the tunnel's **failover target**, and also
  the box Bastion New User mirrors its run onto. Optional — an empty secondary
  is a valid pair and simply means no failover.

## Failover

`start_port_tunnel` walks the pair rather than trying one box, so an
environment whose primary has been terminated — or is merely refusing
connections — comes up on its secondary instead of not at all.

- **A bastion that will not resolve is skipped immediately** (terminated, not
  in the inventory, no pem) and the next one is tried in the same call.
- **A session that dies young fails the bastion over.** The test is the
  tunnel's age at the moment the death is noticed, not the reason it gave: a
  session that never connected dies within seconds, while an old one is a good
  tunnel dropping and moving it to the backup for that would be an
  overreaction. The threshold is 30s, which has to clear the 15s poll interval
  — a session that died at 2s may not be noticed for another 15.
- **It is sticky.** Once a session is happily up on the secondary it is not
  disturbed to go back; only another failure swings the preference around. The
  rotation keeps **every** bastion in the list, so a failure on the secondary
  falls back to the primary rather than leaving nothing to try.
- **The preference lives in memory, never in config.** Which box a tunnel
  happens to be on is a fact about this run. Persisting it would mean an
  outage during one session quietly re-aimed every later one.
- **The Bastion column shows the box actually carrying the session**, flagged
  when it is the secondary. An environment quietly running on its backup
  otherwise looks exactly like a healthy one.

Test login walks the same order, so a test that cannot hold a session on the
primary rolls on to the secondary exactly as forwarding would — otherwise the
test would report a failure the live tunnel recovers from on its own.
- **Login user** — plain text, prefilled from `resolve_ssh_user`, so it shows
  the effective `ec2-user` default rather than an empty box that hides it.
- **Key (pem)** — `pem_library_combo_items` at `PEM_COMBO_POPUP_H`, with a
  Browse… that calls `add_pem_to_library`. Rows are labelled by
  `pem_row_labels`, so two keys of the same filename still identify themselves.

### A saved bastion that is gone is shown, not cleared

If the saved instance id is absent from the loaded inventory, the combo shows it
as a selected, flagged row:

```
Bastion  [ i-0785b06144b6c50b — no longer in inventory   v ]
```

`retain_available_bastion` blanks such an id, which is right for the Scripts
dialogs — an unavailable box there means the dialog is aimed at nothing. Here it
is wrong: a terminated bastion is the user's whole diagnosis, and silently
emptying the field turns "this broke" into "you never configured this". The
dialog is where the cause should be legible.

## Test login

Pressing it:

1. **Saves first.** Testing unsaved edits tests nothing, and a pass that was not
   persisted is worse than no test.
2. **Stops that environment's tunnel** if one is up. A byte-identical session
   binds the same local ports, and with `ExitOnForwardFailure=yes` it would die
   instantly on `Address already in use` — a spurious failure caused by the
   thing working.
3. `resolve_tunnel_launch`, then `Tunnel::spawn`.
4. Parks the `Tunnel` as `Running` with a 5s deadline.

### Frame-polled, not threaded

egui is immediate-mode, so a blocking wait freezes the app. Each frame calls the
existing non-blocking `is_running()` (`try_wait`) — the same idiom
`poll_port_tunnels` already uses:

```rust
enum TestState {
    Idle,
    Running { tunnel: Tunnel, deadline: Instant, started: Instant },
    Passed  { forwards: usize, elapsed: Duration },
    Failed  { stderr: Vec<String>, hint: Option<String>, elapsed: Duration },
}
```

- Died before the deadline → `Failed`, carrying `errors()`.
- Deadline reached, still alive → `Passed`.

A worker thread reporting over a channel was the alternative; it adds machinery
and makes handing the live process back into `port_tunnels` awkward. A blocking
`Tunnel::test()` in `tunnel.rs` freezes the UI.

### The passing session becomes the tunnel

On `Passed`, the process moves **into `port_tunnels`** when the row is On, and
is dropped otherwise (`Drop` kills it). There is no stop-test-restart gap and no
second session — the thing that was tested is the thing that keeps running.

### Failure hints

```rust
fn classify_tunnel_failure(stderr: &str) -> Option<&'static str>
```

maps `Permission denied (publickey)` → the key or the login user is wrong,
`Address already in use` → another process holds that local port,
`Could not resolve` / SSM session errors → the bastion could not be reached.
An unrecognised failure returns `None`. **stderr is always shown verbatim
underneath**; the hint annotates it and never replaces it, because a wrong guess
about an unfamiliar error is worse than no guess.

### Refusals

The test declines to spawn, and says why, when the account is not
`AuthStatus::Ok` (the same gate `start_port_tunnel` applies — a hidden process
dying on a credentials error is invisible), when no bastion is chosen, or when
no pem is chosen. `ensure_include_directive` and `write_managed_block` errors
surface in the dialog, not only in `tunnel_errors`. Closing the dialog mid-test
drops the `Tunnel`, which kills the child.

## Storage

No new configuration format. The three values go to the keys that already hold
them:

| Value   | Call                                                       | Key                |
|---------|------------------------------------------------------------|--------------------|
| pem      | `set_vscode_defaults(account, env, pem, user)`            | `<id>.<ENV>`       |
| user     | same call                                                  | `<id>.<ENV>`       |
| bastions | `set_bastion_selection(account, env, primary, secondary)` | `bastion_pair.<id>.<env>` |

Both halves of the pair are written, because both are editable. An empty
secondary is stored as empty rather than refused — that is what the Scripts
dialogs already accept, and forcing a second bastion on an environment that has
one box would make the dialog unusable there.

Save is followed by `config.save()`, `tunnel_errors.remove(&key)` and
`clear_tunnel_dismissal`, so a corrected environment stops reporting its old
failure.

### Two cross-effects, both intentional

- The pem and login are the same values Open in VS Code resolves, so fixing a
  login here fixes VS Code too.
- The bastion is the same `bastion_pair` the Scripts dialogs share, so changing
  it re-aims Bastion New User, Bastion User Delete and Vault IAM at the new box.

One place to update is the point — the terminated-bastion case is precisely why
this exists. But silently re-aiming a delete-user script would be nasty, so the
dialog states it in a small weak line, and a bastion change is logged at warn.

## Logging

Every step logs through the existing `log_info` / `log_warn` / `log_error`,
formatted like the start log already in `start_port_tunnel`:

- **Open** — `tunnel AUCT: login dialog opened — bastion=i-0785… user=ec2-user pem=none`
- **Save** — `tunnel AUCT: login saved — bastion=… user=… pem=…`
- **Bastion changed** — warn: `tunnel AUCT: bastion changed i-old → i-new (also used by the Scripts dialogs)`
- **Test start** — `tunnel AUCT: test login — ssh -N <alias>, 4 forward(s): 127.0.0.1:5432->db…:5432 …`
- **Test refused** — warn, with the reason, no spawn
- **Test passed** — `tunnel AUCT: test login passed in 4.2s — 4 forward(s) bound; adopted as the live tunnel` (or `discarded (environment is off)`)
- **Test failed** — error, with elapsed time and **every** captured stderr line, not only the last

Elapsed time appears in both result lines deliberately: how long a connect takes
is otherwise something the user can only guess at. The dialog also counts up
live (`testing… 2.1s`), so a slow connect is visibly slow rather than looking
hung.

## Toolbar wording

`refresh_tunnel_status` reports a healthy tunnel as:

```rust
Some((format!("Forwarding ports for {}", …), ScriptState::Running))
```

`ScriptState::Running` shares its styling with an in-flight Scripts run, so a
steady state reads as an operation in progress — a tunnel that connected
minutes ago looks like a connect that has been hanging for minutes. There is no
"connecting" state in this code at all; `alive` is binary off `try_wait`, so the
message only appears once the session is up and carrying its forwards.

The status becomes `Forwarding ports for all environments (4 tunnels up)` —
same mechanism, phrased as a state, with a count that changes when something
changes. If `ScriptState` has a non-spinner variant that suits "fine, ongoing"
it uses that; otherwise only the wording changes, since adding a variant ripples
into every other status caller.

## Testing

The GUI cannot be rendered in a test, so the logic goes in free functions beside
`pem_row_labels` and `retain_available_bastion`, which is where this file
already puts its testable pieces:

- `classify_tunnel_failure` — each known signature maps to its hint; an
  unrecognised one returns `None` so stderr still shows.
- Stale-bastion prefill — a saved id absent from the inventory yields the
  flagged synthetic row; a present one yields a normal selection.
- The save helper — all three keys are written, and the secondary bastion
  survives.

`tunnel_args` and `tunnel_signature` already have tests covering the spawn
arguments; they need no change, which is the point of routing the test through
`Tunnel::spawn`.

## Out of scope

- Editing forwards themselves. `forwards.json` is compiled in and admin-owned;
  this dialog configures how to reach a bastion, not what to forward.
- Any second definition of "test". There is one test and it is a real tunnel.

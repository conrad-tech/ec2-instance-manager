# Port forwards for Open in VS Code

Date: 2026-08-06
Status: approved (design)

## Problem

Reaching an internal service from a workstation means tunnelling it through a
bastion. Today users hand-write a Host block with `LocalForward` lines and
hand-maintain matching entries in the Windows hosts file, per environment. The
app already generates a managed Host block for Open in VS Code, so it can
generate the forwards too.

The hosts file cannot be part of that automation. Verified on 2026-08-06:

```
whoami   brandon-pc\conra
Admin    False
[IO.File]::Open('C:\Windows\System32\drivers\etc\hosts','Append','Write')
         -> Access to the path is denied
```

Writing it needs elevation, most users on corporate machines are not local
admins, and programmatic writes to that path are a behaviour EDRs flag — this
app has a CrowdStrike quarantine in its history that took a rollback to rule
out. So the app **reads** the hosts file and never writes it.

That constraint is affordable because the tunnel does not depend on the hosts
file. In `LocalForward 127.200.20.1:443 uxxx.net:443` the remote name is
resolved on the bastion. A hosts entry only lets the user type the name in a
browser instead of the loopback IP. Users with no hosts entries get working
tunnels addressed by IP.

## Approach

The generated ssh block is ours to write, so it conforms to the machine rather
than asking the machine to change. Where the user's hosts file already resolves
a name, the forward binds **that** IP.

## forwards.json

New `assets/forwards.json`, compiled in and obfuscated like `accounts.json` and
`features.json` (one line added to the asset list in `build.rs`). Admins change
it by editing and rebuilding.

```json
{
  "default_port": 443,
  "port_rules": [
    { "match": "postgres", "port": 5432 },
    { "match": "solr",     "port": 8984 },
    { "match": "kafka",    "port": 9094 }
  ],
  "environments": {
    "AUCT": [
      { "ip": "127.200.20.1", "host": "uxxx.net" },
      { "ip": "127.200.20.2", "host": "pg-uyyy.net" }
    ]
  }
}
```

Environment keys match the `MMODAL_ENV` tag, compared case-insensitively — the
same key the pem cache, the bastion-pair cache and the Scripts dialogs use.

A malformed file fails closed: no forwards, a logged warning, and the rest of
Open in VS Code works unchanged. It must never block a launch.

## Port resolution

For each entry, in order:

1. An explicit `"port"` on the entry.
2. The first `port_rules` entry whose `match` is a case-insensitive substring
   of the DNS name. **First match wins**, so list order is the tiebreak for a
   name like `kafka-postgres-proxy` — this is a documented property, not an
   accident of iteration order.
3. `default_port` (443).

Both sides of the forward use the resolved port, so
`LocalForward 127.200.20.2:5432 pg-uyyy.net:5432` lets `psql -h pg-uyyy.net`
connect with no port flag.

## Hosts file

Read-only. Path comes from `forwards_hosts_file` in config.ini, defaulting to
`C:\Windows\System32\drivers\etc\hosts` on Windows and `/etc/hosts` elsewhere,
with a Browse button in the dialog. A missing or unreadable file is not an
error — it means no hosts data, and resolution falls back to forwards.json.

Format is the standard one, with `#` comments. A comment line whose text is a
**single word** is treated as a section header naming an environment — so
`# AUCT` is a header and `# Added by Docker Desktop` is an ordinary comment:

```
#####
# AUCT
#####

127.200.20.1  uxxx.net
127.200.20.2  pg-uyyy.net
```

A section runs until the next section header. Entries above any header belong
to no environment but are still available for name lookup.

## Resolution per environment

**Matching is by endpoint, not by comment.** Which endpoints belong to an
environment comes from forwards.json; the hosts file is searched by DNS name
wherever that name appears in it. Many hosts files are a bare list of
`IP name` lines with no section comments at all, and those users get the same
result as the ones who annotate.

Per declared entry:

- **The name is in the hosts file** — bind the IP the hosts file gives. This
  is what makes an existing setup work untouched, and it closes a hazard: if
  forwards.json names an IP the user's file already points a *different* name
  at, binding it would silently hijack theirs.
- **The name is absent** — use forwards.json's IP and mark it as needing a
  hosts entry. The tunnel still works, addressed by IP.

A section comment naming the environment is not required. Where one exists it
is purely additive: an entry under it that forwards.json does not declare is
offered as a forward too, since that user has said those endpoints belong to
this environment.

A name mapped to two different addresses uses the first, matching how the file
itself resolves, and logs a warning.

## Ports

Hosts entries carry an IP and a name, so the port comes from the rules above.
`parse_hosts` additionally tolerates `IP:port name:port` and honours such a
port over the rules, for a user keeping a private endpoint list — the system
hosts file cannot carry a port, since Windows' DNS client rejects the line.
The two ends may differ (`127.0.0.1:8443 svc.example.net:443`); a port on one
end mirrors to the other.

## Generated block

```
Host <name>-<user>-<instance-id>
  HostName i-0abc...
  User jane.doe
  IdentityFile "C:\keys\jane.pem"
  ServerAliveInterval 30
  ProxyCommand aws ssm start-session --profile p --region r --target %h \
    --document-name AWS-StartSSHSession --parameters portNumber=%p
  LocalForward 127.200.20.1:443 uxxx.net:443
  LocalForward 127.200.20.2:5432 pg-uyyy.net:5432
```

`ServerAliveInterval 30` is new — an idle SSM tunnel drops without it, and the
hand-written blocks this replaces all carry it.

Forwards are written into the same managed block the existing
`compose_managed_file` produces, so the (HostName, User) replacement rules,
the alias scheme and the include hoisting are unchanged.

## Dialog

Open in VS Code gains a collapsible **Port forwards — AUCT (2)** section
listing the resolved forwards, each with a checkbox, ticked by default. Each
row shows `127.200.20.1:443 -> uxxx.net:443` and where the IP came from (hosts
file or forwards.json).

Rows whose name is absent from the hosts file are flagged, with a **Copy hosts
entries** button that puts the text on the clipboard, and the hosts file path
shown for pasting:

```
#####
# AUCT
#####
127.200.20.1  uxxx.net
```

Unticked forwards are remembered per environment in config.ini, keyed with
`AppConfig::vscode_key(profile_id, env)` — the same `<id>.<ENV>` key the pem,
login and prompt opt-out already use. Stored as the set of *disabled* names, so
a forward added to forwards.json later is on by default rather than silently
absent.

## Validation

Where forwards.json and the hosts file both name an endpoint but disagree about
its address, the disagreement is logged naming both, and the hosts file wins.
This is the drift the two-file split invites; it is surfaced, not silently
merged. A name the hosts file does not mention at all is not drift — that is a
user who has not added it, which the dialog already flags per forward.

## Not building

- No write to the hosts file, no elevation prompt, no merged-file staging.
- No backup file — nothing is modified, so there is nothing to back up.
- No per-instance forward overrides. Forwards are per environment.

## Testing

Pure functions in the library, tested there:

- `forwards.json` parsing, including a malformed file yielding no forwards
  rather than an error.
- Port resolution: explicit port beats rule, rule beats default, first
  matching rule wins for a name matching two, case-insensitive matching.
- Hosts parsing: section headers, entries above any header, comments,
  tabs and multiple spaces, a name appearing in more than one section.
- The three resolution cases, including case 2 overriding the forwards.json
  IP and case 3 marking an entry as needing a hosts line.
- Block generation: forward lines present and correctly ordered, absent when
  the environment has none, `ServerAliveInterval` present.
- Disabled-forward persistence round-tripping through config.ini, and a
  forward added after the fact defaulting to enabled.

GUI-side, the existing dialog tests gain a case with forwards resolved and one
with the section empty.

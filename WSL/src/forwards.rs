//! Per-environment SSH port forwards for the Open in VS Code integration.
//!
//! Reaching an internal service from a workstation means tunnelling it
//! through a bastion. This module turns the compiled-in `assets/forwards.json`
//! plus the machine's hosts file into the `LocalForward` lines that go into
//! the managed Host block.
//!
//! **The hosts file is read, never written.** Writing
//! `C:\Windows\System32\drivers\etc\hosts` needs elevation that most
//! corporate users do not have, and programmatic writes to that path are a
//! behaviour EDR products flag — a real cost for this app, which has a
//! CrowdStrike quarantine in its history. That is affordable because a
//! forward does not depend on the hosts file: in
//! `LocalForward 127.200.20.1:443 host.net:443` the remote name is resolved
//! on the bastion. A hosts entry only lets the user type the name in a
//! browser instead of the loopback IP.
//!
//! So where the user's hosts file already resolves a name, the forward binds
//! *that* IP — the generated config conforms to the machine rather than
//! asking the machine to change.

use std::collections::BTreeMap;

use serde::Deserialize;

/// Compiled-in forward definitions from `assets/forwards.json`, obfuscated at
/// build time like the other bundled assets (see [`crate::obf_core`]).
const BUNDLED_FORWARDS_OBF: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/forwards.json.obf"));

fn bundled_forwards() -> String {
    let plain = crate::obf_core::obf_transform(BUNDLED_FORWARDS_OBF);
    String::from_utf8(plain).expect("bundled forwards.json is valid UTF-8")
}

/// Port inferred from a substring of the DNS name, e.g. `postgres` → 5432.
#[derive(Clone, Debug, Deserialize)]
pub struct PortRule {
    /// Case-insensitive substring matched against the DNS name.
    /// Spelled `match_` because `match` is a keyword; renamed on the wire.
    #[serde(rename = "match", default)]
    pub match_: String,
    /// Port to use on both ends of the forward.
    #[serde(default)]
    pub port: u16,
}

/// One forward declared in `forwards.json`.
#[derive(Clone, Debug, Deserialize)]
pub struct ForwardEntry {
    /// Loopback address to bind locally. Used only when the hosts file has
    /// nothing to say about `host`.
    #[serde(default)]
    pub ip: String,
    /// Remote DNS name, resolved on the bastion.
    #[serde(default)]
    pub host: String,
    /// Explicit port, overriding the rules. `None` means infer.
    #[serde(default)]
    pub port: Option<u16>,
}

/// Parsed `forwards.json`.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct ForwardsConfig {
    /// Port for a name matching no rule.
    pub default_port: u16,
    /// Checked in order — **first match wins**, so the list order is the
    /// tiebreak for a name like `kafka-postgres-proxy` rather than something
    /// implicit. Documented behaviour; do not sort this.
    pub port_rules: Vec<PortRule>,
    /// Forward sets keyed by `MMODAL_ENV` value.
    pub environments: BTreeMap<String, Vec<ForwardEntry>>,
}

impl Default for ForwardsConfig {
    fn default() -> Self {
        Self {
            default_port: 443,
            port_rules: Vec::new(),
            environments: BTreeMap::new(),
        }
    }
}

impl ForwardsConfig {
    /// Parse forward definitions. A malformed file yields the default (no
    /// forwards) rather than an error: forwards are a convenience, and a bad
    /// config must never block a VS Code launch.
    pub fn parse(json: &str) -> Self {
        serde_json::from_str(json).unwrap_or_default()
    }

    /// The compiled-in configuration.
    pub fn bundled() -> Self {
        Self::parse(&bundled_forwards())
    }

    /// Entries declared for an environment, matched case-insensitively —
    /// `MMODAL_ENV` is free text and `forwards.json` is typed by hand.
    pub fn entries_for(&self, env: &str) -> &[ForwardEntry] {
        let env = env.trim();
        if env.is_empty() {
            return &[];
        }
        self.environments
            .iter()
            .find(|(key, _)| key.trim().eq_ignore_ascii_case(env))
            .map(|(_, v)| v.as_slice())
            .unwrap_or(&[])
    }

    /// Port for a DNS name: an explicit port wins, then the first matching
    /// rule, then `default_port`.
    pub fn port_for(&self, host: &str, explicit: Option<u16>) -> u16 {
        if let Some(port) = explicit {
            return port;
        }
        let lower = host.to_ascii_lowercase();
        self.port_rules
            .iter()
            .find(|rule| {
                let needle = rule.match_.trim().to_ascii_lowercase();
                !needle.is_empty() && lower.contains(&needle)
            })
            .map(|rule| rule.port)
            .unwrap_or(self.default_port)
    }
}

/// One `IP  name` line from the hosts file.
#[derive(Clone, Debug, PartialEq)]
pub struct HostsEntry {
    pub ip: String,
    pub host: String,
    /// Port written on the address, as in `127.0.0.1:8080`.
    ///
    /// The system hosts file cannot carry a port — Windows' DNS client
    /// rejects such a line — but a user keeping their own endpoint list and
    /// pointing us at it can, and it is more specific than anything we would
    /// infer, so it wins.
    pub local_port: Option<u16>,
    /// Port written on the name, as in `test.example.com:8080`.
    pub remote_port: Option<u16>,
    /// Section this line sits under, i.e. the environment named by the
    /// nearest preceding single-word comment. Empty for lines above any.
    /// Only ever additive — resolution matches on the endpoint.
    pub section: String,
}

/// Split `host:port` into its parts, tolerating a bare host, a bracketed
/// IPv6 literal (`[::1]:8080`) and a bare IPv6 literal (`::1`, no port —
/// the colons are the address, not a separator).
fn split_endpoint(token: &str) -> (String, Option<u16>) {
    if let Some(rest) = token.strip_prefix('[') {
        if let Some((addr, tail)) = rest.split_once(']') {
            let port = tail.strip_prefix(':').and_then(|p| p.parse().ok());
            return (addr.to_string(), port);
        }
    }
    if token.matches(':').count() == 1 {
        if let Some((host, port)) = token.split_once(':') {
            if let Ok(port) = port.parse::<u16>() {
                return (host.to_string(), Some(port));
            }
        }
    }
    (token.to_string(), None)
}

/// Parse a hosts-format file.
///
/// A comment whose text is a **single word** names a section (`# AUCT`);
/// anything longer is an ordinary comment (`# Added by Docker Desktop`).
/// Real hosts files are full of prose comments and none of them should
/// become a phantom environment.
pub fn parse_hosts(text: &str) -> Vec<HostsEntry> {
    let mut entries = Vec::new();
    let mut section = String::new();

    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(comment) = trimmed.strip_prefix('#') {
            let comment = comment.trim();
            // `#####` rules and blank comments leave the section alone: they
            // decorate a header rather than replacing it.
            if comment.is_empty() || comment.chars().all(|c| c == '#') {
                continue;
            }
            if !comment.contains(char::is_whitespace) {
                section = comment.trim_matches('#').to_string();
            }
            continue;
        }
        // `IP[:port] name[:port] [alias...]` — every name on the line maps
        // to that address.
        let mut parts = trimmed.split_whitespace();
        let Some(ip_token) = parts.next() else { continue };
        let (ip, local_port) = split_endpoint(ip_token);
        for host_token in parts {
            if host_token.starts_with('#') {
                break;
            }
            let (host, remote_port) = split_endpoint(host_token);
            entries.push(HostsEntry {
                ip: ip.clone(),
                host,
                local_port,
                remote_port,
                section: section.clone(),
            });
        }
    }

    entries
}

/// Where a resolved forward's local IP came from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ForwardSource {
    /// The environment's section in the hosts file defined this forward.
    HostsSection,
    /// Declared in forwards.json, but bound to the IP the hosts file
    /// already maps that name to.
    HostsIp,
    /// Declared in forwards.json, with no hosts entry for the name — the
    /// tunnel works, addressed by IP, but the name will not resolve in a
    /// browser until the user adds a line.
    ConfigOnly,
}

/// A forward ready to be written into the managed Host block.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedForward {
    /// Local address to bind.
    pub ip: String,
    /// Remote DNS name, resolved on the bastion.
    pub host: String,
    /// Port bound locally. Usually the same as `remote_port`, but a hosts
    /// entry may name them separately (`127.0.0.1:8443 svc.example.net:443`).
    pub local_port: u16,
    /// Port connected to on the far side.
    pub remote_port: u16,
    pub source: ForwardSource,
}

impl ResolvedForward {
    /// The ssh directive for this forward.
    pub fn directive(&self) -> String {
        format!(
            "LocalForward {}:{} {}:{}",
            self.ip, self.local_port, self.host, self.remote_port
        )
    }

    /// True when the user's hosts file has no entry for this name, so the
    /// name will not resolve locally until they add one.
    pub fn needs_hosts_entry(&self) -> bool {
        self.source == ForwardSource::ConfigOnly
    }
}

/// Resolve the forwards for one environment.
///
/// **Matching is by endpoint, not by comment.** Which endpoints belong to an
/// environment comes from forwards.json; the hosts file is searched by DNS
/// name wherever that name appears in it. Plenty of hosts files are a bare
/// list of `IP name` lines with no section comments at all, and those users
/// must get the same result as the ones who annotate.
///
/// Per declared entry:
///
/// - **The name is in the hosts file** — bind the IP the hosts file gives
///   ([`ForwardSource::HostsIp`]). This is what lets an existing setup work
///   untouched, and it closes a hazard: if forwards.json names an IP the
///   user already points a *different* name at, binding it would silently
///   hijack theirs.
/// - **The name is absent** — use the entry as written and mark it
///   [`ForwardSource::ConfigOnly`]. The tunnel still works, addressed by IP.
///
/// A section comment naming the environment is not required, but where one
/// exists any entry under it that forwards.json does not declare is added
/// too ([`ForwardSource::HostsSection`]) — that user has told us those
/// endpoints belong to this environment, and dropping them would lose a
/// forward they had before.
pub fn resolve_forwards(
    config: &ForwardsConfig,
    hosts: &[HostsEntry],
    env: &str,
) -> Vec<ResolvedForward> {
    let env = env.trim();
    let declared = config.entries_for(env);

    let mut out: Vec<ResolvedForward> = declared
        .iter()
        .filter(|entry| !entry.host.trim().is_empty())
        .map(|entry| {
            let host = entry.host.trim();
            // First match wins, matching how a hosts file resolves.
            let matching = hosts.iter().find(|h| h.host.eq_ignore_ascii_case(host));
            let (ip, source) = match matching {
                Some(h) => (h.ip.clone(), ForwardSource::HostsIp),
                None => (entry.ip.trim().to_string(), ForwardSource::ConfigOnly),
            };
            // A port the user wrote in their own endpoint list is the most
            // specific thing available, so it beats the declared port and
            // the name rules alike.
            let remote_port = matching
                .and_then(|h| h.remote_port)
                .unwrap_or_else(|| config.port_for(host, entry.port));
            let local_port = matching
                .and_then(|h| h.local_port)
                .unwrap_or(remote_port);
            ResolvedForward {
                ip,
                host: host.to_string(),
                local_port,
                remote_port,
                source,
            }
        })
        .filter(|f| !f.ip.is_empty())
        .collect();

    // Extras the user put under a section comment for this environment.
    if !env.is_empty() {
        for entry in hosts
            .iter()
            .filter(|h| h.section.eq_ignore_ascii_case(env))
        {
            if out
                .iter()
                .any(|f| f.host.eq_ignore_ascii_case(&entry.host))
            {
                continue;
            }
            let remote_port = entry
                .remote_port
                .unwrap_or_else(|| config.port_for(&entry.host, None));
            out.push(ResolvedForward {
                ip: entry.ip.clone(),
                host: entry.host.clone(),
                local_port: entry.local_port.unwrap_or(remote_port),
                remote_port,
                source: ForwardSource::HostsSection,
            });
        }
    }

    out
}

/// Names the hosts file maps to more than one address.
///
/// Only the first is used, matching how the file itself resolves — but a
/// duplicate usually means a stale line left above the live one, and
/// binding a forward to an address the machine does not resolve the name to
/// fails in a way that looks like the tunnel is broken.
pub fn duplicate_host_warnings(hosts: &[HostsEntry], used: &[ResolvedForward]) -> Vec<String> {
    let mut out = Vec::new();
    for forward in used {
        let addresses: Vec<&str> = hosts
            .iter()
            .filter(|h| h.host.eq_ignore_ascii_case(&forward.host))
            .map(|h| h.ip.as_str())
            .collect();
        if addresses.len() > 1 && addresses.iter().any(|ip| *ip != addresses[0]) {
            out.push(format!(
                "forwards: {} appears {} times in the hosts file ({}) — using {}",
                forward.host,
                addresses.len(),
                addresses.join(", "),
                addresses[0]
            ));
        }
    }
    out
}

/// The hosts-file text for the forwards that have no entry yet, in the
/// sectioned form the user's file already uses. Empty when nothing is
/// missing. This is offered for pasting — the app never writes the file.
pub fn hosts_snippet(env: &str, forwards: &[ResolvedForward]) -> String {
    let missing: Vec<&ResolvedForward> =
        forwards.iter().filter(|f| f.needs_hosts_entry()).collect();
    if missing.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let env = env.trim();
    if !env.is_empty() {
        out.push_str("#####\n");
        out.push_str(&format!("# {}\n", env.to_uppercase()));
        out.push_str("#####\n\n");
    }
    for f in missing {
        out.push_str(&format!("{}  {}\n", f.ip, f.host));
    }
    out
}

/// `ssh` arguments for a background tunnel carrying these forwards.
///
/// The forwards go on the command line rather than into the managed Host
/// block, because VS Code connects to that same alias: if both carried them,
/// whichever connected second would fail to bind every port and the forwards
/// would exist on only one of the two connections, decided by timing.
///
/// `ExitOnForwardFailure=yes` is deliberate. The window is hidden, so a
/// half-forwarded session that keeps running is invisible and looks exactly
/// like a working one until something fails to connect much later. Better to
/// die and say so in the log.
pub fn tunnel_args(alias: &str, forwards: &[ResolvedForward]) -> Vec<String> {
    let mut args = vec![
        // Verbose. These sessions are invisible and their only account of
        // themselves is the stderr the window shows, so the connection
        // handshake belongs in it: a session that starts, never finishes
        // connecting through the SSM ProxyCommand and therefore binds
        // nothing looks identical to a healthy one without it — alive, no
        // output, no exit. The same `ssh -v` run by hand is what diagnoses
        // it, so run it that way in the first place.
        "-v".to_string(),
        // This session can never answer a question. It is spawned with no
        // console and a null stdin, so ssh's default
        // `StrictHostKeyChecking=ask` is a dead end: every instance id is a
        // new host name, and the first connection to one stops dead on
        // "The authenticity of host … can't be established" with nobody to
        // type yes. It does not fail loudly either — it sits there
        // authenticated to nothing, binding nothing, writing nothing, which
        // is indistinguishable from a healthy tunnel from the outside.
        //
        // `accept-new` rather than `no`: an unknown host is trusted on
        // first sight, which is what an interactive user would have done
        // anyway, but a host whose key has *changed* is still refused. `no`
        // would silently accept a substituted key.
        //
        // `BatchMode=yes` covers the rest of the same class — a password or
        // passphrase prompt fails immediately instead of hanging.
        "-o".to_string(),
        "StrictHostKeyChecking=accept-new".to_string(),
        "-o".to_string(),
        "BatchMode=yes".to_string(),
        // No remote command: the forwards are the entire point, and a login
        // shell is a liability. A shell brings a TMOUT that logs an idle
        // session out however healthy the connection is, and gives us a
        // stdin that anything we write lands in. `ServerAliveInterval` keeps
        // the transport up without one.
        "-N".to_string(),
        "-o".to_string(),
        "ExitOnForwardFailure=yes".to_string(),
        // The block sets this too, but a user editing their own config
        // should not be able to silently disable the keepalive.
        "-o".to_string(),
        "ServerAliveInterval=30".to_string(),
    ];
    for f in forwards {
        args.push("-L".to_string());
        args.push(format!(
            "{}:{}:{}:{}",
            f.ip, f.local_port, f.host, f.remote_port
        ));
    }
    args.push(alias.to_string());
    args
}

/// Signature of a tunnel's forward set, used to notice that the resolved
/// forwards changed and the running tunnel is now stale.
pub fn tunnel_signature(forwards: &[ResolvedForward]) -> String {
    let mut parts: Vec<String> = forwards.iter().map(|f| f.directive()).collect();
    parts.sort();
    parts.join("|")
}

/// A local `ip:port` claimed by two different environments.
#[derive(Clone, Debug, PartialEq)]
pub struct Collision {
    pub bind: String,
    /// Environment that claims it first, in declaration order — the one
    /// allowed to keep it.
    pub kept_env: String,
    pub kept_host: String,
    /// Environment whose forward has to be dropped.
    pub dropped_env: String,
    pub dropped_host: String,
}

impl Collision {
    pub fn message(&self) -> String {
        format!(
            "forwards: {} is claimed by both {} ({}) and {} ({}) — keeping {}, \
             dropping the other. Give each environment its own address.",
            self.bind,
            self.kept_env,
            self.kept_host,
            self.dropped_env,
            self.dropped_host,
            self.kept_env
        )
    }
}

/// Local binds claimed by more than one environment.
///
/// Every environment's tunnel runs at once, so two of them binding the same
/// `ip:port` cannot both work: the second `ssh` fails to bind and, because
/// tunnels run with `ExitOnForwardFailure=yes`, that kills the whole
/// environment's tunnel rather than the one forward. Since the window is
/// hidden the user would just see an environment quietly not working, so the
/// clash is found before anything is spawned.
pub fn collisions(config: &ForwardsConfig) -> Vec<Collision> {
    let mut seen: Vec<(String, String, String)> = Vec::new(); // bind, env, host
    let mut out = Vec::new();
    for (env, entries) in &config.environments {
        for entry in entries {
            let host = entry.host.trim();
            let ip = entry.ip.trim();
            if host.is_empty() || ip.is_empty() {
                continue;
            }
            let bind = format!("{ip}:{}", config.port_for(host, entry.port));
            match seen.iter().find(|(b, _, _)| *b == bind) {
                Some((_, kept_env, kept_host)) => out.push(Collision {
                    bind,
                    kept_env: kept_env.clone(),
                    kept_host: kept_host.clone(),
                    dropped_env: env.clone(),
                    dropped_host: host.to_string(),
                }),
                None => seen.push((bind, env.clone(), host.to_string())),
            }
        }
    }
    out
}

/// Drop forwards whose local bind another environment already claimed, so
/// one bad entry costs a single forward instead of the whole tunnel.
pub fn without_collisions(
    forwards: &[ResolvedForward],
    env: &str,
    collisions: &[Collision],
) -> Vec<ResolvedForward> {
    forwards
        .iter()
        .filter(|f| {
            !collisions.iter().any(|c| {
                c.dropped_env.eq_ignore_ascii_case(env.trim())
                    && c.dropped_host.eq_ignore_ascii_case(&f.host)
            })
        })
        .cloned()
        .collect()
}

/// Default path of the machine's hosts file.
pub fn default_hosts_path() -> String {
    if cfg!(windows) {
        let root = std::env::var("SystemRoot")
            .unwrap_or_else(|_| "C:\\Windows".to_string());
        format!("{root}\\System32\\drivers\\etc\\hosts")
    } else {
        "/etc/hosts".to_string()
    }
}

/// Read and parse a hosts file. A missing or unreadable file is not an
/// error — it means no hosts data, and resolution falls back to
/// forwards.json.
pub fn load_hosts(path: &str) -> Vec<HostsEntry> {
    std::fs::read_to_string(path)
        .map(|text| parse_hosts(&text))
        .unwrap_or_default()
}

/// Warnings where forwards.json and the hosts file disagree about an
/// endpoint's address. Matched by DNS name anywhere in the file, so a hosts
/// file with no section comments is checked the same as an annotated one.
///
/// This is the drift the two-file split invites; it gets surfaced in the log
/// rather than silently merged. A name the hosts file does not mention at
/// all is *not* drift — that is a user who has not added it, which the
/// dialog already flags per forward.
pub fn drift_warnings(config: &ForwardsConfig, hosts: &[HostsEntry]) -> Vec<String> {
    let mut out = Vec::new();
    for (env, entries) in &config.environments {
        for entry in entries {
            let host = entry.host.trim();
            let declared_ip = entry.ip.trim();
            if host.is_empty() || declared_ip.is_empty() {
                continue;
            }
            if let Some(h) = hosts.iter().find(|h| h.host.eq_ignore_ascii_case(host)) {
                if h.ip != declared_ip {
                    out.push(format!(
                        "forwards: {env} {host} is {declared_ip} in forwards.json \
                         but {} in the hosts file — using the hosts file",
                        h.ip
                    ));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ForwardsConfig {
        ForwardsConfig::parse(
            r#"{
                "default_port": 443,
                "port_rules": [
                    { "match": "postgres", "port": 5432 },
                    { "match": "solr", "port": 8984 },
                    { "match": "kafka", "port": 9094 }
                ],
                "environments": {
                    "AUCT": [
                        { "ip": "127.200.20.1", "host": "uweb.example.net" },
                        { "ip": "127.200.20.2", "host": "pg-postgres.example.net" }
                    ]
                }
            }"#,
        )
    }

    #[test]
    fn malformed_json_yields_no_forwards_rather_than_an_error() {
        let cfg = ForwardsConfig::parse("{ not json");
        assert!(cfg.environments.is_empty());
        // Still usable: a bad config must never block a launch.
        assert_eq!(cfg.default_port, 443);
        assert_eq!(cfg.port_for("anything", None), 443);
    }

    #[test]
    fn port_precedence_explicit_then_rule_then_default() {
        let cfg = config();
        assert_eq!(cfg.port_for("pg-postgres.example.net", Some(6000)), 6000);
        assert_eq!(cfg.port_for("pg-postgres.example.net", None), 5432);
        assert_eq!(cfg.port_for("SOLR-search.example.net", None), 8984);
        assert_eq!(cfg.port_for("kafka1.example.net", None), 9094);
        assert_eq!(cfg.port_for("uweb.example.net", None), 443);
    }

    /// Order is the documented tiebreak, so a name matching two rules is
    /// answered by the earlier one rather than by iteration luck.
    #[test]
    fn first_matching_rule_wins() {
        let cfg = config();
        assert_eq!(cfg.port_for("kafka-postgres-proxy.example.net", None), 5432);
    }

    #[test]
    fn environments_match_case_insensitively() {
        let cfg = config();
        assert_eq!(cfg.entries_for("auct").len(), 2);
        assert_eq!(cfg.entries_for(" AUCT ").len(), 2);
        assert!(cfg.entries_for("").is_empty());
        assert!(cfg.entries_for("NOPE").is_empty());
    }

    #[test]
    fn hosts_sections_come_from_single_word_comments() {
        let entries = parse_hosts(
            "# Copyright (c) 1993-2009 Microsoft Corp.\n\
             127.0.0.1  localhost\n\
             #####\n\
             # AUCT\n\
             #####\n\
             \n\
             127.200.20.1\tuweb.example.net\n\
             127.200.20.2  pg-postgres.example.net  pg-alias\n\
             # Added by Docker Desktop\n\
             127.200.20.3  extra.example.net\n\
             # DEV1\n\
             127.200.10.1  dev.example.net\n",
        );
        // Prose comments do not open a section; the decorative ##### rules
        // do not close one.
        assert_eq!(entries[0].host, "localhost");
        assert_eq!(entries[0].section, "");
        assert_eq!(entries[1].section, "AUCT");
        assert_eq!(entries[2].host, "pg-postgres.example.net");
        assert_eq!(entries[3].host, "pg-alias");
        assert_eq!(entries[3].section, "AUCT");
        assert_eq!(entries[4].host, "extra.example.net");
        assert_eq!(entries[4].section, "AUCT");
        assert_eq!(entries[5].section, "DEV1");
    }

    /// The case that matters most: plenty of hosts files are a bare list of
    /// `IP name` lines with no section comments anywhere. Matching on the
    /// endpoint has to give those users the same result.
    #[test]
    fn endpoints_match_without_any_section_comments() {
        let hosts = parse_hosts(
            "127.0.0.1  localhost\n\
             127.9.9.1  uweb.example.net\n\
             127.9.9.2  pg-postgres.example.net\n",
        );
        let out = resolve_forwards(&config(), &hosts, "AUCT");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].ip, "127.9.9.1");
        assert_eq!(out[0].source, ForwardSource::HostsIp);
        assert_eq!(out[1].ip, "127.9.9.2");
        assert_eq!(out[1].remote_port, 5432);
        assert!(out.iter().all(|f| !f.needs_hosts_entry()));
    }

    /// A port written in the user's own endpoint list is the most specific
    /// thing available, so it beats both the declared port and the rules.
    #[test]
    fn a_port_in_the_hosts_entry_wins() {
        let hosts = parse_hosts(
            "172.0.0.1:8080  uweb.example.net:8080\n\
             127.9.9.2:9999  pg-postgres.example.net:9999\n",
        );
        let out = resolve_forwards(&config(), &hosts, "AUCT");
        assert_eq!(
            out[0].directive(),
            "LocalForward 172.0.0.1:8080 uweb.example.net:8080"
        );
        // Beats the postgres rule, which would otherwise say 5432.
        assert_eq!(
            out[1].directive(),
            "LocalForward 127.9.9.2:9999 pg-postgres.example.net:9999"
        );
    }

    /// The two ends need not agree, and a port on only one end mirrors.
    #[test]
    fn local_and_remote_ports_can_differ() {
        let hosts = parse_hosts("127.0.0.1:8443  uweb.example.net:443\n");
        let out = resolve_forwards(&config(), &hosts, "AUCT");
        assert_eq!(out[0].local_port, 8443);
        assert_eq!(out[0].remote_port, 443);
        assert_eq!(
            out[0].directive(),
            "LocalForward 127.0.0.1:8443 uweb.example.net:443"
        );

        // Port on the name only: the local end mirrors it.
        let hosts = parse_hosts("127.0.0.1  uweb.example.net:8080\n");
        let out = resolve_forwards(&config(), &hosts, "AUCT");
        assert_eq!(out[0].local_port, 8080);
        assert_eq!(out[0].remote_port, 8080);
    }

    /// An IPv6 literal is colons all the way down; only a bracketed form
    /// carries a port.
    #[test]
    fn split_endpoint_handles_ipv6_and_bare_hosts() {
        assert_eq!(split_endpoint("127.0.0.1"), ("127.0.0.1".into(), None));
        assert_eq!(
            split_endpoint("127.0.0.1:8080"),
            ("127.0.0.1".into(), Some(8080))
        );
        assert_eq!(split_endpoint("::1"), ("::1".into(), None));
        assert_eq!(split_endpoint("[::1]:8080"), ("::1".into(), Some(8080)));
        // Not a port — left alone rather than mangled.
        assert_eq!(
            split_endpoint("host:notaport"),
            ("host:notaport".into(), None)
        );
    }

    /// A name under some *other* environment's comment still matches: the
    /// comment is a hint, never a filter.
    #[test]
    fn a_misfiled_section_comment_does_not_hide_an_endpoint() {
        let hosts = parse_hosts(
            "# SOMETHINGELSE\n\
             127.9.9.1  uweb.example.net\n",
        );
        let out = resolve_forwards(&config(), &hosts, "AUCT");
        assert_eq!(out[0].ip, "127.9.9.1");
        assert_eq!(out[0].source, ForwardSource::HostsIp);
    }

    /// Where a user *has* annotated, an endpoint under that environment's
    /// comment that forwards.json does not declare is still offered — they
    /// have said it belongs here, and dropping it loses a forward.
    #[test]
    fn section_adds_endpoints_forwards_json_does_not_declare() {
        let hosts = parse_hosts(
            "# AUCT\n\
             127.9.9.1  uweb.example.net\n\
             127.9.9.9  solr-search.example.net\n",
        );
        let out = resolve_forwards(&config(), &hosts, "AUCT");
        assert_eq!(out.len(), 3, "{out:?}");
        // Declared entries keep their order, extras follow.
        assert_eq!(out[0].host, "uweb.example.net");
        assert_eq!(out[1].host, "pg-postgres.example.net");
        assert_eq!(out[2].host, "solr-search.example.net");
        assert_eq!(out[2].source, ForwardSource::HostsSection);
        // The extra still gets its port inferred from the name.
        assert_eq!(out[2].remote_port, 8984);
    }

    /// Only the first address is used, as the file itself resolves — but a
    /// stale duplicate above the live line is worth saying out loud.
    #[test]
    fn duplicate_names_use_the_first_and_warn() {
        let hosts = parse_hosts(
            "127.9.9.1  uweb.example.net\n\
             127.9.9.7  uweb.example.net\n",
        );
        let out = resolve_forwards(&config(), &hosts, "AUCT");
        assert_eq!(out[0].ip, "127.9.9.1");
        let warnings = duplicate_host_warnings(&hosts, &out);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("127.9.9.1"), "{warnings:?}");
        assert!(warnings[0].contains("127.9.9.7"), "{warnings:?}");
        // A name repeated with the same address is just noise, not a clash.
        let same = parse_hosts("127.9.9.1  uweb.example.net\n127.9.9.1  uweb.example.net\n");
        let out = resolve_forwards(&config(), &same, "AUCT");
        assert!(duplicate_host_warnings(&same, &out).is_empty());
    }

    /// The case that makes an existing setup work untouched: the name is in
    /// the hosts file, so the forward binds the user's IP, not ours.
    #[test]
    fn hosts_ip_overrides_the_configured_ip() {
        let hosts = parse_hosts("127.55.55.55  uweb.example.net\n");
        let out = resolve_forwards(&config(), &hosts, "AUCT");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].ip, "127.55.55.55");
        assert_eq!(out[0].source, ForwardSource::HostsIp);
        assert!(!out[0].needs_hosts_entry());
        // The other name is absent from the hosts file entirely.
        assert_eq!(out[1].ip, "127.200.20.2");
        assert_eq!(out[1].source, ForwardSource::ConfigOnly);
        assert!(out[1].needs_hosts_entry());
    }

    #[test]
    fn no_hosts_file_still_yields_working_forwards() {
        let out = resolve_forwards(&config(), &[], "AUCT");
        assert_eq!(out.len(), 2);
        assert_eq!(
            out[0].directive(),
            "LocalForward 127.200.20.1:443 uweb.example.net:443"
        );
        assert_eq!(
            out[1].directive(),
            "LocalForward 127.200.20.2:5432 pg-postgres.example.net:5432"
        );
        assert!(out.iter().all(|f| f.needs_hosts_entry()));
    }

    #[test]
    fn unknown_or_empty_environment_yields_nothing() {
        assert!(resolve_forwards(&config(), &[], "NOPE").is_empty());
        assert!(resolve_forwards(&config(), &[], "").is_empty());
    }

    #[test]
    fn snippet_lists_only_the_missing_entries() {
        let hosts = parse_hosts("127.55.55.55  uweb.example.net\n");
        let out = resolve_forwards(&config(), &hosts, "auct");
        let snippet = hosts_snippet("auct", &out);
        assert!(snippet.contains("# AUCT"));
        assert!(snippet.contains("127.200.20.2  pg-postgres.example.net"));
        // Already resolved by the user's file — nothing to paste for it.
        assert!(!snippet.contains("uweb.example.net"));
        // Nothing missing at all means nothing to show.
        assert!(hosts_snippet("auct", &resolve_forwards(&config(), &hosts, "auct"))
            .lines()
            .count()
            > 0);
        assert!(hosts_snippet("x", &[]).is_empty());
    }

    /// Drift is a *disagreement* about an address, found by name with no
    /// help from comments. A name the file does not mention is not drift —
    /// that is a user who has not added it, flagged per forward instead.
    #[test]
    fn drift_warnings_flag_only_a_disagreement() {
        let hosts = parse_hosts("127.9.9.1  uweb.example.net\n");
        let warnings = drift_warnings(&config(), &hosts);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("127.200.20.1"), "{warnings:?}");
        assert!(warnings[0].contains("127.9.9.1"), "{warnings:?}");

        // Agreement is silent, and so is an empty hosts file.
        let agreed = parse_hosts("127.200.20.1  uweb.example.net\n");
        assert!(drift_warnings(&config(), &agreed).is_empty());
        assert!(drift_warnings(&config(), &[]).is_empty());
    }

    #[test]
    /// Vault reaches `default_port` like any other web service, because the
    /// deployment this ships to fronts it with TLS on 443 — confirmed
    /// against a working `LocalForward 127.200.20.4:443 vault.…:443`.
    ///
    /// A `vault → 8200` rule was added once, inferred from the `vault_addr`
    /// values in `accounts.json`. That file ships as template data
    /// (`vault.dev1.YOUR-COMPANY.com:8200`) and its ports describe nobody's
    /// deployment, so the rule moved a working forward off 443 and broke it.
    /// This test exists to stop that being re-derived from the same
    /// non-evidence: change it only against a LocalForward line known to
    /// work.
    #[test]
    fn the_shipped_rules_leave_vault_on_the_default_port() {
        let config = ForwardsConfig::bundled();
        assert_eq!(config.port_for("vault.scpp-ct.example.net", None), 443);
        assert_eq!(config.port_for("VAULT.PROD.EXAMPLE.COM", None), 443);
    }

    /// An explicit port always wins, so a site whose Vault really is on 8200
    /// says so per entry without needing a rule that re-ports everyone's.
    #[test]
    fn an_explicit_port_still_overrides_the_default_for_vault() {
        let config = ForwardsConfig::bundled();
        assert_eq!(config.port_for("vault.example.net", Some(8200)), 8200);
    }

    /// The named services keep their ports — these are substring matches, so
    /// any new rule risks silently re-porting a name it does not own.
    #[test]
    fn the_shipped_rules_keep_the_named_services_on_their_ports() {
        let config = ForwardsConfig::bundled();
        assert_eq!(config.port_for("pg-postgres01.auct.example.net", None), 5432);
        assert_eq!(config.port_for("solr01.dev1.example.net", None), 8984);
        assert_eq!(config.port_for("uweb01.dev1.example.net", None), 443);
    }

    #[test]
    fn tunnel_args_carry_every_forward_and_fail_loudly() {
        let hosts = parse_hosts("127.9.9.1  uweb.example.net\n");
        let out = resolve_forwards(&config(), &hosts, "AUCT");
        let args = tunnel_args("web-jane-i-1", &out);
        // No remote shell: nothing to time out, nothing to type into.
        assert!(args.contains(&"-N".to_string()));
        // Fails rather than half-forwarding: the window is hidden, so a
        // partly working tunnel would be invisible.
        assert!(args.windows(2).any(|w| w
            == ["-o".to_string(), "ExitOnForwardFailure=yes".to_string()]));
        assert!(args.contains(&"-L".to_string()));
        assert!(args.contains(&"127.9.9.1:443:uweb.example.net:443".to_string()));
        assert!(args.contains(&"127.200.20.2:5432:pg-postgres.example.net:5432".to_string()));
        // The alias is last, as ssh expects.
        assert_eq!(args.last().unwrap(), "web-jane-i-1");
    }

    /// The tunnel is spawned with no console and a null stdin, so anything
    /// ssh would *ask* is a hang, not a failure.
    ///
    /// Every instance id is a new host name, so the first connection to one
    /// hits `StrictHostKeyChecking=ask` and stops dead on "The authenticity
    /// of host … can't be established" with nobody to answer — sitting
    /// alive, bound to nothing, writing nothing, which from outside is
    /// indistinguishable from a working tunnel. That is the bug this
    /// prevents, and it cost a long afternoon to find.
    ///
    /// `accept-new`, never `no`: a first sighting is trusted, a *changed*
    /// key is still refused.
    #[test]
    fn tunnel_args_can_never_stop_to_ask_a_question() {
        let hosts = parse_hosts("127.9.9.1  uweb.example.net\n");
        let out = resolve_forwards(&config(), &hosts, "AUCT");
        let args = tunnel_args("web-jane-i-1", &out);
        assert!(args.windows(2).any(|w| w
            == ["-o".to_string(), "StrictHostKeyChecking=accept-new".to_string()]));
        assert!(args.windows(2).any(|w| w
            == ["-o".to_string(), "BatchMode=yes".to_string()]));
        assert!(
            !args.iter().any(|a| a.contains("StrictHostKeyChecking=no")),
            "`no` would accept a substituted host key silently"
        );
    }

    /// Verbose, because the session pane is the only account these
    /// invisible processes give of themselves — and the handshake is where
    /// a session that never finishes connecting shows it.
    #[test]
    fn tunnel_args_are_verbose() {
        let out = resolve_forwards(&config(), &[], "AUCT");
        assert!(tunnel_args("web-jane-i-1", &out).contains(&"-v".to_string()));
    }

    /// The signature has to change when the forward set does, so a stale
    /// tunnel gets replaced, and stay put when only the order differs.
    #[test]
    fn tunnel_signature_tracks_the_forward_set() {
        let a = resolve_forwards(&config(), &[], "AUCT");
        let same = resolve_forwards(&config(), &[], "auct");
        assert_eq!(tunnel_signature(&a), tunnel_signature(&same));

        let mut reordered = a.clone();
        reordered.reverse();
        assert_eq!(tunnel_signature(&a), tunnel_signature(&reordered));

        let hosts = parse_hosts("127.9.9.1  uweb.example.net\n");
        let moved = resolve_forwards(&config(), &hosts, "AUCT");
        assert_ne!(tunnel_signature(&a), tunnel_signature(&moved));

        assert_ne!(tunnel_signature(&a), tunnel_signature(&a[..1]));
    }

    /// Every environment's tunnel runs at once, so a shared bind is not a
    /// style problem — with ExitOnForwardFailure it kills a whole tunnel.
    #[test]
    fn collisions_across_environments_are_found_before_spawning() {
        let cfg = ForwardsConfig::parse(
            r#"{
                "environments": {
                    "AUCT": [{ "ip": "127.200.20.1", "host": "a.example.net" }],
                    "DEV1": [
                        { "ip": "127.200.20.1", "host": "b.example.net" },
                        { "ip": "127.200.10.1", "host": "c.example.net" }
                    ]
                }
            }"#,
        );
        let clashes = collisions(&cfg);
        assert_eq!(clashes.len(), 1, "{clashes:?}");
        assert_eq!(clashes[0].bind, "127.200.20.1:443");
        // Declaration order decides who keeps it; BTreeMap orders AUCT first.
        assert_eq!(clashes[0].kept_env, "AUCT");
        assert_eq!(clashes[0].dropped_env, "DEV1");
        assert!(clashes[0].message().contains("127.200.20.1:443"));

        // The clashing forward is dropped; the environment's others survive,
        // so one bad entry costs a forward and not the whole tunnel.
        let dev1 = resolve_forwards(&cfg, &[], "DEV1");
        assert_eq!(dev1.len(), 2);
        let kept = without_collisions(&dev1, "DEV1", &clashes);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].host, "c.example.net");
        // AUCT keeps its claim.
        let auct = resolve_forwards(&cfg, &[], "AUCT");
        assert_eq!(without_collisions(&auct, "AUCT", &clashes).len(), 1);
    }

    /// The same address on a different port is not a clash, and neither is
    /// the same port on a different address.
    #[test]
    fn collisions_compare_the_whole_bind() {
        let cfg = ForwardsConfig::parse(
            r#"{
                "port_rules": [{ "match": "postgres", "port": 5432 }],
                "environments": {
                    "A": [{ "ip": "127.0.0.1", "host": "pg-postgres.net" }],
                    "B": [
                        { "ip": "127.0.0.1", "host": "web.net" },
                        { "ip": "127.0.0.2", "host": "other-postgres.net" }
                    ]
                }
            }"#,
        );
        assert!(collisions(&cfg).is_empty());
    }

    #[test]
    fn bundled_forwards_parses() {
        let cfg = ForwardsConfig::bundled();
        assert_eq!(cfg.default_port, 443);
        assert!(
            cfg.port_rules.iter().any(|r| r.match_ == "postgres"),
            "bundled forwards.json should ship the port rules"
        );
    }
}

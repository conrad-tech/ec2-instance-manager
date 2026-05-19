//! Scanner and validation helpers for the user's OpenSSH client config
//! (`~/.ssh/config`). Used by the VS Code Remote-SSH integration to
//! auto-discover which private key (pem) the user already uses for a
//! given AWS profile, and to validate pem paths before launching.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::util::home_dir;

/// A single `Host` block discovered while scanning ssh config files.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DiscoveredHost {
    /// The `Host` alias (first pattern on the Host line).
    pub alias: String,
    /// `User` directive, if present.
    pub user: Option<String>,
    /// `IdentityFile` directive (path expanded for `~`), if present.
    pub identity_file: Option<String>,
    /// AWS profile parsed from a `--profile X` token in the ProxyCommand.
    pub aws_profile: Option<String>,
    /// AWS region parsed from a `--region X` token in the ProxyCommand.
    pub region: Option<String>,
    /// Instance ID parsed from a `--target X` token in the ProxyCommand.
    pub target: Option<String>,
}

/// Return the path to the user's primary ssh config file (`~/.ssh/config`).
pub fn ssh_config_path() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".ssh").join("config"))
}

/// Directory used for our managed, quarantined ssh config include file.
pub fn managed_config_path() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".ssh").join("config.d").join("ec2-manager"))
}

/// Expand a leading `~` in a path against the user's home directory.
fn expand_tilde(raw: &str) -> String {
    let trimmed = raw.trim().trim_matches('"');
    if let Some(rest) = trimmed.strip_prefix("~/").or_else(|| trimmed.strip_prefix("~\\")) {
        if let Some(home) = home_dir() {
            return home.join(rest).to_string_lossy().to_string();
        }
    }
    trimmed.to_string()
}

/// Split an ssh config line into (keyword_lowercased, rest_of_line).
/// Keywords are case-insensitive; the separator may be whitespace or `=`.
fn split_directive(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let bytes = trimmed.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() && !bytes[idx].is_ascii_whitespace() && bytes[idx] != b'=' {
        idx += 1;
    }
    let keyword = trimmed[..idx].to_ascii_lowercase();
    let rest = trimmed[idx..].trim_start_matches([' ', '\t', '=']).trim();
    Some((keyword, rest.to_string()))
}

/// Pull the token following `--<flag>` (or `--<flag>=value`) out of an
/// ssh ProxyCommand string. Tolerates surrounding quotes.
fn extract_flag(proxy_command: &str, flag: &str) -> Option<String> {
    let needle = format!("--{flag}");
    let tokens: Vec<&str> = proxy_command.split_whitespace().collect();
    for (i, tok) in tokens.iter().enumerate() {
        let cleaned = tok.trim_matches(['"', '\'']);
        if let Some(eq_val) = cleaned.strip_prefix(&format!("{needle}=")) {
            let v = eq_val.trim_matches(['"', '\'']);
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
        if cleaned == needle {
            if let Some(next) = tokens.get(i + 1) {
                let v = next.trim_matches(['"', '\'']);
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

/// Parse the text of a single ssh config file into discovered Host blocks.
/// `include_dirs` collects any `Include` paths encountered, for the caller
/// to recurse into.
fn parse_config_text(text: &str, includes: &mut Vec<String>) -> Vec<DiscoveredHost> {
    let mut hosts: Vec<DiscoveredHost> = Vec::new();
    let mut current: Option<DiscoveredHost> = None;

    for line in text.lines() {
        let Some((keyword, rest)) = split_directive(line) else {
            continue;
        };
        match keyword.as_str() {
            "host" => {
                if let Some(done) = current.take() {
                    hosts.push(done);
                }
                // First pattern only; ignore wildcard-only blocks.
                let alias = rest.split_whitespace().next().unwrap_or("").to_string();
                current = Some(DiscoveredHost {
                    alias,
                    ..Default::default()
                });
            }
            "match" => {
                // Match blocks are not Host blocks; close any open Host.
                if let Some(done) = current.take() {
                    hosts.push(done);
                }
            }
            "include" => {
                for path in rest.split_whitespace() {
                    includes.push(expand_tilde(path));
                }
            }
            "user" => {
                if let Some(h) = current.as_mut() {
                    h.user = Some(rest.clone());
                }
            }
            "identityfile" => {
                if let Some(h) = current.as_mut() {
                    h.identity_file = Some(expand_tilde(&rest));
                }
            }
            "proxycommand" => {
                if let Some(h) = current.as_mut() {
                    h.aws_profile = extract_flag(&rest, "profile");
                    h.region = extract_flag(&rest, "region");
                    h.target = extract_flag(&rest, "target");
                }
            }
            _ => {}
        }
    }
    if let Some(done) = current.take() {
        hosts.push(done);
    }
    hosts
}

/// Scan the user's ssh config (following `Include` directives one level
/// of recursion deep, with cycle protection) and return every Host block
/// found. Missing files are silently skipped.
pub fn scan() -> Vec<DiscoveredHost> {
    let mut results: Vec<DiscoveredHost> = Vec::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    let mut queue: Vec<PathBuf> = Vec::new();

    if let Some(primary) = ssh_config_path() {
        queue.push(primary);
    }

    while let Some(path) = queue.pop() {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
        if !visited.insert(canonical) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let mut includes: Vec<String> = Vec::new();
        results.extend(parse_config_text(&text, &mut includes));

        for inc in includes {
            for resolved in resolve_include(&inc) {
                queue.push(resolved);
            }
        }
    }

    results
}

/// Resolve an `Include` value into concrete files. Relative paths are
/// taken against `~/.ssh/`. A trailing `*` glob lists the directory.
fn resolve_include(raw: &str) -> Vec<PathBuf> {
    let base: PathBuf = if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else if let Some(home) = home_dir() {
        home.join(".ssh").join(raw)
    } else {
        PathBuf::from(raw)
    };

    let as_str = base.to_string_lossy().to_string();
    if let Some(dir_part) = as_str.strip_suffix('*') {
        let dir = PathBuf::from(dir_part);
        let mut found = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_file() {
                    found.push(p);
                }
            }
        }
        return found;
    }
    vec![base]
}

/// Find the most relevant pem for an AWS profile among discovered hosts:
/// prefer a Host whose ProxyCommand names that profile. Returns all
/// matching distinct pem paths so the caller can disambiguate.
pub fn pems_for_profile(hosts: &[DiscoveredHost], profile: &str) -> Vec<String> {
    let mut pems: Vec<String> = Vec::new();
    for h in hosts {
        if h.aws_profile.as_deref() == Some(profile) {
            if let Some(pem) = &h.identity_file {
                if !pem.is_empty() && !pems.iter().any(|p| p == pem) {
                    pems.push(pem.clone());
                }
            }
        }
    }
    pems
}

/// Every distinct pem path mentioned anywhere in the discovered hosts.
pub fn all_pems(hosts: &[DiscoveredHost]) -> Vec<String> {
    let mut pems: Vec<String> = Vec::new();
    for h in hosts {
        if let Some(pem) = &h.identity_file {
            if !pem.is_empty() && !pems.iter().any(|p| p == pem) {
                pems.push(pem.clone());
            }
        }
    }
    pems
}

/// Outcome of validating a pem path.
#[derive(Clone, Debug, PartialEq)]
pub enum PemValidation {
    /// File exists and looks like a private key.
    Ok,
    /// File does not exist. Carries the parent directory (if it exists)
    /// so the UI can offer a fuzzy "did you mean" dropdown.
    Missing { parent_exists: bool },
    /// File exists but does not look like a PEM private key.
    NotAKey,
}

/// Validate a pem path: existence + a cheap "-----BEGIN" content sniff.
pub fn validate_pem(path: &str) -> PemValidation {
    let p = Path::new(path);
    if !p.is_file() {
        let parent_exists = p.parent().map(|d| d.is_dir()).unwrap_or(false);
        return PemValidation::Missing { parent_exists };
    }
    // Sniff the first non-empty line for a PEM header.
    match std::fs::read_to_string(p) {
        Ok(content) => {
            let looks_like_key = content
                .lines()
                .map(|l| l.trim())
                .find(|l| !l.is_empty())
                .map(|first| first.starts_with("-----BEGIN"))
                .unwrap_or(false);
            if looks_like_key {
                PemValidation::Ok
            } else {
                PemValidation::NotAKey
            }
        }
        // Binary or unreadable-as-text: can't confirm, treat as not-a-key.
        Err(_) => PemValidation::NotAKey,
    }
}

/// List candidate key files in a directory: anything ending in `.pem` or
/// `.key`, plus extensionless `id_*` files. Returns full paths.
pub fn scan_dir_for_keys(dir: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let lower = name.to_ascii_lowercase();
        let is_key = lower.ends_with(".pem")
            || lower.ends_with(".key")
            || (lower.starts_with("id_") && !lower.ends_with(".pub"));
        if is_key {
            found.push(path.to_string_lossy().to_string());
        }
    }
    found
}

/// Classic Levenshtein edit distance, used to rank "did you mean" key
/// suggestions against the filename the user typed.
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr: Vec<usize> = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr[j + 1] = (prev[j + 1] + 1)
                .min(curr[j] + 1)
                .min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

/// Given a (possibly wrong) pem path, list key files in its parent
/// directory sorted by filename similarity to what the user typed —
/// the closest match first, for a "did you mean" dropdown.
pub fn suggest_keys_near(path: &str) -> Vec<String> {
    let p = Path::new(path);
    let Some(parent) = p.parent() else {
        return Vec::new();
    };
    let typed_name = p
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let mut candidates = scan_dir_for_keys(&parent.to_string_lossy());
    candidates.sort_by_key(|cand| {
        let cand_name = Path::new(cand)
            .file_name()
            .map(|n| n.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        levenshtein(&typed_name, &cand_name)
    });
    candidates
}

/// Sanitize a string for use as part of an ssh `Host` alias: keep only
/// alphanumerics and `.`/`-`/`_`, collapsing any other run to a single
/// dash and trimming leading/trailing dashes.
pub fn sanitize_alias_part(raw: &str) -> String {
    let mut out: String = raw
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .collect();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    out.trim_matches('-').to_string()
}

/// Stable Host alias used for our managed VS Code Remote-SSH entries:
/// `<ec2-name>-<instance-id>`, falling back to `ec2-<instance-id>` when
/// the instance has no usable Name tag.
pub fn managed_alias(name: &str, instance_id: &str) -> String {
    let prefix = sanitize_alias_part(name);
    if prefix.is_empty() {
        format!("ec2-{instance_id}")
    } else {
        format!("{prefix}-{instance_id}")
    }
}

/// Build a single managed `Host` block for VS Code Remote-SSH. The
/// ProxyCommand tunnels SSH through SSM Session Manager so no inbound
/// port 22 is required. `%h`/`%p` are filled in by ssh at connect time.
pub fn vscode_host_block(
    instance_id: &str,
    name: &str,
    user: &str,
    pem: &str,
    profile: &str,
    region: &str,
) -> String {
    let alias = managed_alias(name, instance_id);
    format!(
        "Host {alias}\n  \
         HostName {instance_id}\n  \
         User {user}\n  \
         IdentityFile \"{pem}\"\n  \
         ProxyCommand aws ssm start-session --profile {profile} --region {region} \
         --target %h --document-name AWS-StartSSHSession \
         --parameters portNumber=%p\n"
    )
}

/// Split managed-file text into (alias, block_text) pairs.
fn split_blocks(text: &str) -> Vec<(String, String)> {
    let mut blocks: Vec<(String, String)> = Vec::new();
    let mut cur_alias: Option<String> = None;
    let mut cur_lines: Vec<String> = Vec::new();
    for line in text.lines() {
        if let Some((kw, rest)) = split_directive(line) {
            if kw == "host" {
                if let Some(alias) = cur_alias.take() {
                    blocks.push((alias, cur_lines.join("\n")));
                }
                cur_alias = Some(rest.split_whitespace().next().unwrap_or("").to_string());
                cur_lines = vec![line.to_string()];
                continue;
            }
        }
        if cur_alias.is_some() {
            cur_lines.push(line.to_string());
        }
    }
    if let Some(alias) = cur_alias.take() {
        blocks.push((alias, cur_lines.join("\n")));
    }
    blocks
}

/// Write (or replace) the managed Host block for an instance in the
/// quarantined `~/.ssh/config.d/ec2-manager` file. Other blocks are
/// preserved. Returns the Host alias on success.
pub fn write_managed_block(
    instance_id: &str,
    name: &str,
    user: &str,
    pem: &str,
    profile: &str,
    region: &str,
) -> Result<String, String> {
    let path = managed_config_path()
        .ok_or_else(|| "could not resolve home directory".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }

    let alias = managed_alias(name, instance_id);
    // Drop any prior block for this alias *or* an older block for the
    // same instance under a different name, so a renamed instance does
    // not leave a stale duplicate entry behind.
    let suffix = format!("-{instance_id}");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut kept: Vec<String> = Vec::new();
    for (block_alias, block_text) in split_blocks(&existing) {
        if block_alias != alias && !block_alias.ends_with(&suffix) {
            kept.push(block_text.trim_end().to_string());
        }
    }
    kept.push(
        vscode_host_block(instance_id, name, user, pem, profile, region)
            .trim_end()
            .to_string(),
    );

    let header = "# Managed by ec2-manager — do not edit by hand.\n\n";
    let body = format!("{header}{}\n", kept.join("\n\n"));
    std::fs::write(&path, body)
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    Ok(alias)
}

const INCLUDE_BEGIN: &str = "# >>> ec2-manager managed include >>>";
const INCLUDE_END: &str = "# <<< ec2-manager managed include <<<";

/// Ensure `~/.ssh/config` contains an `Include config.d/ec2-manager`
/// directive so VS Code's Remote-SSH picks up our managed blocks. The
/// directive is wrapped in marker comments and added only once; the
/// user's own content is otherwise untouched.
pub fn ensure_include_directive() -> Result<(), String> {
    let path = ssh_config_path()
        .ok_or_else(|| "could not resolve home directory".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    // Already present (either our marked block or a hand-written Include).
    let already = existing.lines().any(|l| {
        let lower = l.trim().to_ascii_lowercase();
        lower.starts_with("include") && lower.contains("config.d/ec2-manager")
    });
    if already {
        return Ok(());
    }
    // Include must precede Host blocks to apply globally — prepend it.
    let snippet = format!(
        "{INCLUDE_BEGIN}\nInclude config.d/ec2-manager\n{INCLUDE_END}\n\n"
    );
    let updated = format!("{snippet}{existing}");
    std::fs::write(&path, updated)
        .map_err(|e| format!("could not update {}: {e}", path.display()))?;
    Ok(())
}

/// Best-effort detection of the VS Code `code` CLI on PATH.
pub fn vscode_cli_available() -> bool {
    let candidates: &[&str] = if cfg!(windows) {
        &["code.cmd", "code"]
    } else {
        &["code"]
    };
    for cmd in candidates {
        let probe = if cfg!(windows) { "where" } else { "which" };
        if let Ok(status) = std::process::Command::new(probe)
            .arg(cmd)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
        {
            if status.success() {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_host_block_with_proxycommand() {
        let text = "Host pa_bastion\n  \
            User ec2-user\n  \
            ProxyCommand powershell.exe \"aws ssm start-session --profile pa \
            --region us-east-1 --target i-0c7b5d62a1a086476 \
            --document-name AWS-StartSSHSession --parameters portNumber=%p\"\n  \
            IdentityFile /home/u/.ssh/pa-dev.pem\n";
        let mut includes = Vec::new();
        let hosts = parse_config_text(text, &mut includes);
        assert_eq!(hosts.len(), 1);
        let h = &hosts[0];
        assert_eq!(h.alias, "pa_bastion");
        assert_eq!(h.user.as_deref(), Some("ec2-user"));
        assert_eq!(h.aws_profile.as_deref(), Some("pa"));
        assert_eq!(h.region.as_deref(), Some("us-east-1"));
        assert_eq!(h.target.as_deref(), Some("i-0c7b5d62a1a086476"));
        assert_eq!(h.identity_file.as_deref(), Some("/home/u/.ssh/pa-dev.pem"));
    }

    #[test]
    fn extract_flag_handles_equals_and_quotes() {
        assert_eq!(
            extract_flag("aws ssm --profile=pa --region us-east-1", "profile"),
            Some("pa".to_string())
        );
        assert_eq!(
            extract_flag("aws ssm --profile \"pa\" --target i-1", "target"),
            Some("i-1".to_string())
        );
        assert_eq!(extract_flag("aws ssm --profile pa", "region"), None);
    }

    #[test]
    fn pems_for_profile_dedups_and_filters() {
        let hosts = vec![
            DiscoveredHost {
                alias: "a".into(),
                aws_profile: Some("pa".into()),
                identity_file: Some("/k/pa.pem".into()),
                ..Default::default()
            },
            DiscoveredHost {
                alias: "b".into(),
                aws_profile: Some("pa".into()),
                identity_file: Some("/k/pa.pem".into()),
                ..Default::default()
            },
            DiscoveredHost {
                alias: "c".into(),
                aws_profile: Some("pb".into()),
                identity_file: Some("/k/pb.pem".into()),
                ..Default::default()
            },
        ];
        assert_eq!(pems_for_profile(&hosts, "pa"), vec!["/k/pa.pem".to_string()]);
        assert_eq!(pems_for_profile(&hosts, "pb"), vec!["/k/pb.pem".to_string()]);
        assert!(pems_for_profile(&hosts, "zz").is_empty());
    }

    #[test]
    fn vscode_host_block_has_required_directives() {
        let block = vscode_host_block(
            "i-0c7b5d62a1a086476",
            "web server 01",
            "ec2-user",
            "C:\\keys\\pa.pem",
            "pa",
            "us-east-1",
        );
        // Name is sanitized and prefixed onto the instance id.
        assert!(block.starts_with("Host web-server-01-i-0c7b5d62a1a086476"));
        assert!(block.contains("HostName i-0c7b5d62a1a086476"));
        assert!(block.contains("User ec2-user"));
        assert!(block.contains("IdentityFile \"C:\\keys\\pa.pem\""));
        assert!(block.contains("--profile pa"));
        assert!(block.contains("--region us-east-1"));
        assert!(block.contains("AWS-StartSSHSession"));
    }

    #[test]
    fn managed_alias_uses_name_and_falls_back() {
        assert_eq!(
            managed_alias("Web Server", "i-123"),
            "Web-Server-i-123"
        );
        assert_eq!(managed_alias("", "i-123"), "ec2-i-123");
        assert_eq!(managed_alias("  ", "i-123"), "ec2-i-123");
        assert_eq!(
            sanitize_alias_part("prod/app (east)"),
            "prod-app-east"
        );
    }

    #[test]
    fn split_blocks_separates_host_entries() {
        let text = "# header\n\nHost a\n  User x\n\nHost b\n  User y\n";
        let blocks = split_blocks(text);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].0, "a");
        assert_eq!(blocks[1].0, "b");
        assert!(blocks[0].1.contains("User x"));
    }

    #[test]
    fn levenshtein_basic() {
        assert_eq!(levenshtein("pa-dev.pem", "pa-dev.pem"), 0);
        assert_eq!(levenshtein("pa-dvev.pem", "pa-dev.pem"), 1);
        assert!(levenshtein("pa-dev.pem", "totally-different") > 5);
    }
}

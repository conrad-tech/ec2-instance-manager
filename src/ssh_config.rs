use std::fs;

use crate::util::home_dir;

const EXCLUDED_USERS: &[&str] = &["ec2-user", "ssm-user", "root", "ubuntu", "admin", "centos"];

/// Discover candidate remote usernames from the local `~/.ssh/config`.
///
/// Returns distinct `User <value>` entries, preserving first-seen order, with
/// common default accounts (ec2-user, ssm-user, root, etc.) filtered out so the
/// caller can fall back to those explicitly.
pub fn discover_ssh_users() -> Vec<String> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    let path = home.join(".ssh").join("config");
    let Ok(raw) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    parse_users(&raw)
}

fn parse_users(raw: &str) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = match line.split_once(|c: char| c.is_whitespace() || c == '=') {
            Some((k, v)) => (k.trim(), v.trim().trim_start_matches('=').trim()),
            None => continue,
        };
        if !key.eq_ignore_ascii_case("user") || value.is_empty() {
            continue;
        }
        let user = value.trim_matches('"').trim_matches('\'').to_string();
        if EXCLUDED_USERS.iter().any(|u| u.eq_ignore_ascii_case(&user)) {
            continue;
        }
        if seen.insert(user.clone()) {
            out.push(user);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_users() {
        let raw = "\
            Host foo\n\
              User jane.doe\n\
            Host bar\n\
              User john.smith\n\
            Host default\n\
              User ec2-user\n\
        ";
        let users = parse_users(raw);
        assert_eq!(users, vec!["jane.doe".to_string(), "john.smith".to_string()]);
    }

    #[test]
    fn dedupes_and_skips_comments() {
        let raw = "\
            # comment\n\
            Host a\n\
              User alice\n\
            Host b\n\
              User alice\n\
            Host c\n\
              User=bob\n\
        ";
        let users = parse_users(raw);
        assert_eq!(users, vec!["alice".to_string(), "bob".to_string()]);
    }

    #[test]
    fn case_insensitive_key() {
        let raw = "Host x\n  USER carol\n";
        assert_eq!(parse_users(raw), vec!["carol".to_string()]);
    }

    #[test]
    fn strips_quotes() {
        let raw = "Host x\n  User \"quoted.user\"\n";
        assert_eq!(parse_users(raw), vec!["quoted.user".to_string()]);
    }
}

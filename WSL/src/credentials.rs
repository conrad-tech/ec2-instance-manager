use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;

use crate::models::{AuthStatus, ProfileAuthInfo, ProfileConfig};
use crate::util::home_dir;

pub fn credentials_path() -> Option<PathBuf> {
    home_dir().map(|p| p.join(".aws").join("credentials"))
}

pub fn read_fed_expire(profile: &str) -> Option<u64> {
    let path = credentials_path()?;
    let content = fs::read_to_string(path).ok()?;
    parse_fed_expire(&content, profile)
}

fn parse_fed_expire(content: &str, profile: &str) -> Option<u64> {
    if profile.trim().is_empty() {
        return None;
    }

    let header = format!("[{profile}]");
    let mut in_section = false;

    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_section = line.eq_ignore_ascii_case(&header);
            continue;
        }

        if !in_section || line.starts_with('#') || line.is_empty() {
            continue;
        }

        if let Some((k, v)) = line.split_once('=') {
            if k.trim().eq_ignore_ascii_case("fed_expire") {
                let val = v.trim();
                // Try Unix epoch first, then ISO 8601 (e.g. "2026-03-21T04:14:59Z")
                if let Ok(epoch) = val.parse::<u64>() {
                    return Some(epoch);
                }
                return parse_iso8601_to_epoch(val);
            }
        }
    }

    None
}

pub fn check_profile_auth(profile: &str) -> AuthStatus {
    let Some(expire) = read_fed_expire(profile) else {
        return AuthStatus::Missing;
    };

    let now = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    if expire > now {
        AuthStatus::Ok
    } else {
        AuthStatus::Expired
    }
}

/// Reads `fed_role` from the named credentials section.
pub fn read_fed_role(profile: &str) -> Option<String> {
    let path = credentials_path()?;
    let content = fs::read_to_string(path).ok()?;
    parse_fed_role(&content, profile)
}

fn parse_fed_role(content: &str, profile: &str) -> Option<String> {
    if profile.trim().is_empty() {
        return None;
    }

    let header = format!("[{profile}]");
    let mut in_section = false;

    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_section = line.eq_ignore_ascii_case(&header);
            continue;
        }

        if !in_section || line.starts_with('#') || line.is_empty() {
            continue;
        }

        if let Some((k, v)) = line.split_once('=') {
            if k.trim().eq_ignore_ascii_case("fed_role") {
                return Some(v.trim().to_string());
            }
        }
    }

    None
}

/// Parses an ISO 8601 datetime string (e.g. "2026-03-21T04:14:59Z") to a Unix epoch.
fn parse_iso8601_to_epoch(s: &str) -> Option<u64> {
    // Expected format: YYYY-MM-DDTHH:MM:SSZ or YYYY-MM-DDTHH:MM:SS
    let s = s.trim_end_matches('Z');
    let (date, time) = s.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: u64 = date_parts.next()?.parse().ok()?;
    let month: u64 = date_parts.next()?.parse().ok()?;
    let day: u64 = date_parts.next()?.parse().ok()?;

    let mut time_parts = time.split(':');
    let hour: u64 = time_parts.next()?.parse().ok()?;
    let min: u64 = time_parts.next()?.parse().ok()?;
    let sec: u64 = time_parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);

    // Days from year 1970 to the given year/month/day (UTC).
    let mut days: u64 = 0;
    for y in 1970..year {
        days += if is_leap(y) { 366 } else { 365 };
    }
    let month_days = [31, 28 + if is_leap(year) { 1 } else { 0 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    for m in 0..(month.saturating_sub(1) as usize) {
        days += month_days.get(m).copied().unwrap_or(30) as u64;
    }
    days += day.saturating_sub(1);

    Some(days * 86400 + hour * 3600 + min * 60 + sec)
}

fn is_leap(y: u64) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

/// Extracts the account ID from a role ARN.
/// `arn:aws:iam::123456789012:role/MyFedRole` → `Some("123456789012")`
pub fn account_id_from_role_arn(arn: &str) -> Option<String> {
    arn.split(':')
        .nth(4)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Scans all sections in `~/.aws/credentials` for a `fed_role` ARN that
/// contains `account_id`, and returns the matching section name.
pub fn find_profile_by_account_id(account_id: &str) -> Option<String> {
    if account_id.trim().is_empty() {
        return None;
    }
    let path = credentials_path()?;
    let content = fs::read_to_string(path).ok()?;
    parse_profile_by_account_id(&content, account_id)
}

/// Returns all credential section names whose `fed_role` ARN matches the given account ID.
fn parse_all_profiles_by_account_id(content: &str, account_id: &str) -> Vec<String> {
    let mut current_section: Option<String> = None;
    let mut matches = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            current_section = Some(line[1..line.len() - 1].to_string());
            continue;
        }

        if line.starts_with('#') || line.is_empty() {
            continue;
        }

        if let Some((k, v)) = line.split_once('=') {
            if k.trim().eq_ignore_ascii_case("fed_role") {
                if let Some(ref section) = current_section {
                    if let Some(arn_account) = account_id_from_role_arn(v.trim()) {
                        if arn_account == account_id {
                            matches.push(section.clone());
                        }
                    }
                }
            }
        }
    }

    matches
}

fn parse_profile_by_account_id(content: &str, account_id: &str) -> Option<String> {
    let matches = parse_all_profiles_by_account_id(content, account_id);
    if matches.is_empty() {
        return None;
    }
    if matches.len() == 1 {
        return Some(matches.into_iter().next().unwrap());
    }

    // Multiple sections match — pick the one with valid auth (latest expiry).
    let now = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut best: Option<(String, u64)> = None;
    for section in &matches {
        let expire = parse_fed_expire(content, section).unwrap_or(0);
        if expire > now {
            // Valid auth — prefer the one with the latest expiry.
            match &best {
                Some((_, best_exp)) if *best_exp >= expire => {}
                _ => best = Some((section.clone(), expire)),
            }
        } else if best.is_none() {
            // No valid auth found yet — track the latest expired as fallback.
            best = Some((section.clone(), expire));
        } else if let Some((_, best_exp)) = &best {
            if *best_exp < now && expire > *best_exp {
                // Both expired — prefer the one that expired most recently.
                best = Some((section.clone(), expire));
            }
        }
    }

    best.map(|(section, _)| section)
}

pub fn check_all_profiles_auth(profiles: &[ProfileConfig]) -> Vec<ProfileAuthInfo> {
    let content = credentials_path()
        .and_then(|p| fs::read_to_string(p).ok())
        .unwrap_or_default();

    profiles
        .iter()
        .map(|p| {
            let sections = parse_all_profiles_by_account_id(&content, &p.account_id);
            if sections.is_empty() {
                return ProfileAuthInfo {
                    profile_id: p.profile_id.clone(),
                    auth_status: AuthStatus::Missing,
                    expires_at: None,
                };
            }

            // Check all matching sections — use the best auth status.
            let mut best_status = AuthStatus::Missing;
            let mut best_expire: Option<u64> = None;
            for section in &sections {
                let status = check_profile_auth(section);
                let expire = parse_fed_expire(&content, section);
                if status == AuthStatus::Ok {
                    // Valid auth found — use it (pick latest expiry).
                    if best_status != AuthStatus::Ok
                        || expire.unwrap_or(0) > best_expire.unwrap_or(0)
                    {
                        best_status = AuthStatus::Ok;
                        best_expire = expire;
                    }
                } else if best_status != AuthStatus::Ok {
                    // No valid auth yet — track the latest expired.
                    if expire.unwrap_or(0) > best_expire.unwrap_or(0) {
                        best_status = status;
                        best_expire = expire;
                    }
                }
            }

            ProfileAuthInfo {
                profile_id: p.profile_id.clone(),
                auth_status: best_status,
                expires_at: best_expire,
            }
        })
        .collect()
}

/// AWS credential keys for a profile, read from the fed_aws_* fields
/// in ~/.aws/credentials.
#[derive(Clone, Debug)]
pub struct AwsCredentialKeys {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
}

/// Read the fed_aws_* credential keys for a given profile section.
/// Returns None if the profile or keys are not found.
pub fn read_profile_credentials(profile: &str) -> Option<AwsCredentialKeys> {
    let path = credentials_path()?;
    let content = fs::read_to_string(path).ok()?;
    parse_profile_credentials(&content, profile)
}

fn parse_profile_credentials(content: &str, profile: &str) -> Option<AwsCredentialKeys> {
    if profile.trim().is_empty() {
        return None;
    }
    let header = format!("[{profile}]");
    let mut in_section = false;
    let mut access_key = None;
    let mut secret_key = None;
    let mut session_token = None;

    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            if in_section {
                break; // past our section
            }
            in_section = line.eq_ignore_ascii_case(&header);
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let k = k.trim();
            let v = v.trim().to_string();
            match k {
                "fed_aws_access_key_id" | "aws_access_key_id" => access_key = Some(v),
                "fed_aws_secret_access_key" | "aws_secret_access_key" => secret_key = Some(v),
                "fed_aws_session_token" | "aws_session_token" => session_token = Some(v),
                _ => {}
            }
        }
    }

    Some(AwsCredentialKeys {
        access_key_id: access_key?,
        secret_access_key: secret_key?,
        session_token,
    })
}

pub fn credentials_mtime() -> Option<SystemTime> {
    let path = credentials_path()?;
    fs::metadata(path).ok()?.modified().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_fed_expire_finds_value() {
        let content = "\
[default]
aws_access_key_id=AKIA123

[aupk168]
fed_aws_access_key_id=AKIA456
fed_aws_secret_access_key=secret
fed_aws_session_token=token
fed_expire=1742000000
";
        assert_eq!(parse_fed_expire(content, "aupk168"), Some(1742000000));
    }

    #[test]
    fn parse_fed_expire_missing_section() {
        let content = "[default]\naws_access_key_id=AKIA123\n";
        assert_eq!(parse_fed_expire(content, "nonexistent"), None);
    }

    #[test]
    fn parse_fed_expire_missing_key() {
        let content = "[aupk168]\nfed_aws_access_key_id=AKIA456\n";
        assert_eq!(parse_fed_expire(content, "aupk168"), None);
    }

    #[test]
    fn parse_fed_expire_empty_profile() {
        let content = "[aupk168]\nfed_expire=1742000000\n";
        assert_eq!(parse_fed_expire(content, ""), None);
    }

    #[test]
    fn parse_fed_expire_case_insensitive_section() {
        let content = "[AUPK168]\nfed_expire=1742000000\n";
        assert_eq!(parse_fed_expire(content, "aupk168"), Some(1742000000));
    }

    #[test]
    fn parse_fed_expire_multiple_sections() {
        let content = "\
[profile1]
fed_expire=1000000000

[profile2]
fed_expire=2000000000
";
        assert_eq!(parse_fed_expire(content, "profile1"), Some(1000000000));
        assert_eq!(parse_fed_expire(content, "profile2"), Some(2000000000));
    }

    #[test]
    fn parse_fed_role_finds_value() {
        let content = "\
[aupk168]
fed_role=arn:aws:iam::123456789012:role/MyFedRole
fed_expire=9999999999
";
        assert_eq!(
            parse_fed_role(content, "aupk168"),
            Some("arn:aws:iam::123456789012:role/MyFedRole".to_string())
        );
    }

    #[test]
    fn parse_fed_role_missing() {
        let content = "[aupk168]\nfed_expire=9999999999\n";
        assert_eq!(parse_fed_role(content, "aupk168"), None);
    }

    #[test]
    fn account_id_from_arn_standard() {
        assert_eq!(
            account_id_from_role_arn("arn:aws:iam::123456789012:role/MyFedRole"),
            Some("123456789012".to_string())
        );
    }

    #[test]
    fn account_id_from_arn_malformed() {
        assert_eq!(account_id_from_role_arn("not-an-arn"), None);
        assert_eq!(account_id_from_role_arn("arn:aws:iam"), None);
    }

    #[test]
    fn parse_profile_by_account_id_finds_section() {
        let content = "\
[default]
aws_access_key_id=AKIA000

[aupk168]
fed_role=arn:aws:iam::123456789012:role/MyFedRole
fed_expire=9999999999

[staging]
fed_role=arn:aws:iam::234567890123:role/StagingRole
fed_expire=1000000000
";
        assert_eq!(
            parse_profile_by_account_id(content, "123456789012"),
            Some("aupk168".to_string())
        );
        assert_eq!(
            parse_profile_by_account_id(content, "234567890123"),
            Some("staging".to_string())
        );
    }

    #[test]
    fn parse_profile_by_account_id_not_found() {
        let content = "[aupk168]\nfed_role=arn:aws:iam::123456789012:role/MyFedRole\n";
        assert_eq!(parse_profile_by_account_id(content, "999999999999"), None);
    }

    #[test]
    fn parse_profile_by_account_id_empty_account() {
        let content = "[aupk168]\nfed_role=arn:aws:iam::123456789012:role/MyFedRole\n";
        assert_eq!(parse_profile_by_account_id(content, ""), None);
    }

    #[test]
    fn parse_profile_by_account_id_no_fed_role() {
        let content = "[aupk168]\nfed_expire=9999999999\n";
        assert_eq!(parse_profile_by_account_id(content, "123456789012"), None);
    }
}

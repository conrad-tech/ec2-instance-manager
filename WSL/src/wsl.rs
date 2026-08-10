//! Running the port-forward tunnels on the Windows side from a WSL build.
//!
//! The GUI is usable from WSL, but a tunnel started there is useless for a
//! browser on Windows. WSL2 has its own network namespace: `ssh -L
//! 127.200.20.4:443 …` inside it binds WSL's loopback, and Windows cannot
//! reach that address. WSL2's localhost forwarding only mirrors `127.0.0.1`,
//! never the distinct `127.200.x` addresses the forwards depend on to give
//! every service its own name.
//!
//! The failure is silent and looks like success from inside the app: the
//! process is alive, the binds succeeded — in WSL — and nothing ever arrives,
//! so there is no stderr either.
//!
//! So under WSL the tunnel is spawned with the **Windows** ssh client, which
//! binds on the Windows loopback where the browser is. Two details make that
//! work:
//!
//! - **`ssh.exe` reads Windows paths.** The managed block is written with the
//!   pem translated to `C:\…`, and handed over with `-F` rather than by
//!   editing the user's own Windows ssh config — that file is hand-maintained
//!   and our block is self-contained anyway.
//! - **Credentials are already shared.** `wsl_setup` symlinks the WSL AWS
//!   config directory to the Windows one, so the `aws ssm start-session` in
//!   the ProxyCommand authenticates the same either side.

use std::path::PathBuf;

/// Windows OpenSSH client, as reached from WSL.
pub const WINDOWS_SSH: &str = "/mnt/c/Windows/System32/OpenSSH/ssh.exe";

/// Whether this build is running under WSL.
///
/// `WSL_DISTRO_NAME` is set in every WSL shell but not necessarily inherited
/// by a GUI launched another way, so the kernel release string is the
/// fallback — it carries `microsoft` on both WSL 1 and 2.
pub fn is_wsl() -> bool {
    if std::env::var_os("WSL_DISTRO_NAME").is_some() {
        return true;
    }
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| s.to_ascii_lowercase().contains("microsoft"))
        .unwrap_or(false)
}

/// Translate a WSL path to the Windows form `ssh.exe` understands.
///
/// `/mnt/c/Users/x/k.pem` becomes `C:\Users\x\k.pem`. A path outside `/mnt`
/// lives on the WSL filesystem, which Windows reaches over the
/// `\\wsl.localhost\<distro>` share — usable, but see
/// [`pem_is_windows_native`]: for a **private key** it is the wrong place to
/// leave one, so callers warn rather than silently depending on it.
///
/// Returns `None` when the distro name is unknown and the path needs it.
pub fn to_windows_path(path: &str) -> Option<String> {
    let path = path.trim();
    if path.is_empty() {
        return None;
    }
    // Already a Windows path — a user may have typed one in.
    if path.len() >= 2 && path.as_bytes()[1] == b':' {
        return Some(path.replace('/', "\\"));
    }
    if let Some(rest) = path.strip_prefix("/mnt/") {
        let mut parts = rest.splitn(2, '/');
        let drive = parts.next().unwrap_or("");
        let tail = parts.next().unwrap_or("");
        if drive.len() == 1 && drive.chars().all(|c| c.is_ascii_alphabetic()) {
            let drive = drive.to_ascii_uppercase();
            return Some(format!("{drive}:\\{}", tail.replace('/', "\\")));
        }
    }
    let distro = std::env::var("WSL_DISTRO_NAME").ok()?;
    Some(format!(
        "\\\\wsl.localhost\\{distro}{}",
        path.replace('/', "\\")
    ))
}

/// Whether a pem lives on the Windows filesystem, where `ssh.exe` can read it
/// without crossing the `\\wsl.localhost` share.
///
/// Windows OpenSSH refuses a private key whose ACL is too permissive, and a
/// file reached over that share does not present the ownership it wants. A
/// key kept under `/mnt/<drive>/…` avoids the question entirely, so the
/// caller can say so plainly instead of the tunnel failing with a permissions
/// error nobody sees.
pub fn pem_is_windows_native(path: &str) -> bool {
    let path = path.trim();
    (path.len() >= 2 && path.as_bytes()[1] == b':') || path.starts_with("/mnt/")
}

/// Where the Windows-side copy of the managed ssh block is written.
///
/// Kept under the WSL home rather than the Windows one: it is ours, it is
/// handed to `ssh.exe` explicitly with `-F`, and finding the Windows user's
/// profile would mean shelling out to `cmd.exe` for no gain.
pub fn windows_managed_config() -> Option<PathBuf> {
    crate::util::home_dir()
        .map(|h| h.join(".ssh").join("config.d").join("ec2-manager.win"))
}

/// Is the Windows ssh client actually present?
pub fn windows_ssh_available() -> bool {
    std::path::Path::new(WINDOWS_SSH).exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_paths_become_windows_paths() {
        assert_eq!(
            to_windows_path("/mnt/c/Users/adk/keys/key.pem").as_deref(),
            Some("C:\\Users\\adk\\keys\\key.pem")
        );
        // Drive letter is upper-cased, as Windows writes it.
        assert_eq!(
            to_windows_path("/mnt/d/keys/k.pem").as_deref(),
            Some("D:\\keys\\k.pem")
        );
    }

    /// A user may paste a Windows path into the pem field; it must survive
    /// unchanged apart from slash direction.
    #[test]
    fn an_existing_windows_path_is_left_alone() {
        assert_eq!(
            to_windows_path("C:/Users/adk/key.pem").as_deref(),
            Some("C:\\Users\\adk\\key.pem")
        );
        assert_eq!(
            to_windows_path("C:\\Users\\adk\\key.pem").as_deref(),
            Some("C:\\Users\\adk\\key.pem")
        );
    }

    #[test]
    fn blank_paths_have_no_translation() {
        assert_eq!(to_windows_path(""), None);
        assert_eq!(to_windows_path("   "), None);
    }

    /// `/mnt/` followed by something that is not a single drive letter is not
    /// a drive mount — it must not be mangled into one.
    #[test]
    fn a_non_drive_mount_is_not_treated_as_a_drive() {
        let out = to_windows_path("/mnt/storage/keys/k.pem");
        assert!(
            out.is_none() || out.as_deref().unwrap().starts_with("\\\\wsl.localhost\\"),
            "unexpected translation: {out:?}"
        );
    }

    /// Only a key on a Windows drive avoids the `\\wsl.localhost` share, and
    /// Windows OpenSSH is particular about private key permissions.
    #[test]
    fn pem_location_is_classified_for_the_permissions_warning() {
        assert!(pem_is_windows_native("/mnt/c/Users/adk/key.pem"));
        assert!(pem_is_windows_native("C:\\Users\\adk\\key.pem"));
        assert!(!pem_is_windows_native("/home/bconrad/keys/key.pem"));
    }
}

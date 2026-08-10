//! End-to-end verification that a port forward actually carries traffic.
//!
//! A tunnel that survives its first seconds has authenticated and **bound**
//! its local ports — `ExitOnForwardFailure=yes` guarantees that much. It has
//! not shown that anything reaches the far side: ssh only attempts the remote
//! connection when something first uses the forward, so a listener can sit
//! there looking healthy and fail the moment it matters. That failure is
//! invisible until a browser or a client hits it much later, which is exactly
//! the class of problem the Port Forwards window exists to surface.
//!
//! So we make one request through the tunnel and see what comes back.
//!
//! Two decisions worth keeping:
//!
//! - **The request goes through `curl`,** not a linked HTTP stack, for the
//!   same reason the alerts feed does: it matches how the app already shells
//!   out, and `curl.exe` ships with Windows 10 1803+.
//! - **Reaching the server is the test, not the reply.** Vault answers
//!   `/v1/sys/health` with 429, 472, 501 or 503 when it is standby,
//!   performance-standby, uninitialised or sealed — every one of those is a
//!   fully working forward. Even a TLS complaint proves bytes crossed. Only a
//!   connect failure or a timeout means the forward is not carrying traffic.

use std::process::Command;

/// What a probe concluded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeVerdict {
    /// Something answered through the tunnel.
    Reached,
    /// Nothing answered — the forward is bound but not carrying traffic.
    Unreachable,
    /// curl failed in a way that says nothing about the forward (it is
    /// missing, or died for its own reasons). Deliberately distinct from
    /// `Unreachable`: reporting "not forwarded" because curl is absent would
    /// send someone to debug a tunnel that is fine.
    Inconclusive,
}

/// Host and port to probe, taken from a `vault_addr`-style URL.
///
/// Accepts `https://host:8200`, `http://host`, and a bare `host:8200`. The
/// port falls back to the scheme's default, since an address written without
/// one still names a reachable endpoint.
pub fn parse_endpoint(addr: &str) -> Option<(String, u16)> {
    let addr = addr.trim();
    if addr.is_empty() {
        return None;
    }
    let (default_port, rest) = match addr.split_once("://") {
        Some(("https", rest)) => (443u16, rest),
        Some(("http", rest)) => (80u16, rest),
        // An unknown scheme is not something to guess at.
        Some(_) => return None,
        None => (443u16, addr),
    };
    // Drop any path, query or fragment.
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .trim_end_matches('.');
    if authority.is_empty() {
        return None;
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() => {
            let port = port.parse().ok()?;
            Some((host.to_string(), port))
        }
        _ => Some((authority.to_string(), default_port)),
    }
}

/// The curl invocation that probes one forward.
///
/// `--resolve` pins the hostname to the forward's local bind, so the request
/// travels the tunnel whether or not the machine's hosts file maps that name —
/// the hosts file is optional here, and plenty of users have no entry at all.
/// Keeping the *name* in the URL (rather than curling the IP) means SNI and
/// the `Host` header are what the far side expects.
///
/// `-k` because this is a reachability check, not an authentication one: a
/// self-signed or internally-issued certificate must not read as "the forward
/// is broken".
pub fn probe_args(host: &str, local_port: u16, bind_ip: &str, path: &str) -> Vec<String> {
    vec![
        "-sS".to_string(),
        "-k".to_string(),
        "--max-time".to_string(),
        "5".to_string(),
        "-o".to_string(),
        null_device().to_string(),
        "-w".to_string(),
        "%{http_code}".to_string(),
        "--resolve".to_string(),
        format!("{host}:{local_port}:{bind_ip}"),
        format!("https://{host}:{local_port}{path}"),
    ]
}

fn null_device() -> &'static str {
    if cfg!(windows) {
        "NUL"
    } else {
        "/dev/null"
    }
}

/// Turn curl's exit code into a verdict.
///
/// The codes that matter: 7 is "couldn't connect", 28 "timed out", 52 "empty
/// reply", 56 "receive error" — the forward is not carrying traffic. TLS
/// complaints (35, 51, 58, 60, 77, 91) mean we *reached* a TLS endpoint, so
/// they are proof, not failure, even though `-k` makes them unlikely. Code 6
/// is DNS, which `--resolve` should have made impossible, so it indicates a
/// malformed probe rather than a broken tunnel.
pub fn classify_exit(code: Option<i32>) -> ProbeVerdict {
    match code {
        Some(0) => ProbeVerdict::Reached,
        Some(35 | 51 | 58 | 60 | 77 | 91) => ProbeVerdict::Reached,
        Some(7 | 28 | 52 | 56) => ProbeVerdict::Unreachable,
        _ => ProbeVerdict::Inconclusive,
    }
}

/// Run the probe. Blocking — callers run it off the UI thread.
pub fn probe(host: &str, local_port: u16, bind_ip: &str, path: &str) -> (ProbeVerdict, String) {
    let args = probe_args(host, local_port, bind_ip, path);
    let mut command = Command::new("curl");
    command.args(&args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // No console window: this runs behind a GUI.
        command.creation_flags(0x0800_0000);
    }
    let output = match command.output() {
        Ok(output) => output,
        Err(e) => {
            return (
                ProbeVerdict::Inconclusive,
                format!("could not run curl: {e}"),
            )
        }
    };
    let verdict = classify_exit(output.status.code());
    let status = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = match &verdict {
        ProbeVerdict::Reached if !status.is_empty() && status != "000" => {
            format!("HTTP {status}")
        }
        ProbeVerdict::Reached => "answered".to_string(),
        _ => {
            let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if err.is_empty() {
                format!("curl exit {:?}", output.status.code())
            } else {
                err.lines().last().unwrap_or(&err).to_string()
            }
        }
    };
    (verdict, detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_endpoint_takes_the_explicit_port() {
        assert_eq!(
            parse_endpoint("https://vault.dev1.internal:8200"),
            Some(("vault.dev1.internal".to_string(), 8200))
        );
    }

    /// An address written without a port still names a reachable endpoint,
    /// so the scheme's default stands in rather than the probe being skipped.
    #[test]
    fn parse_endpoint_defaults_the_port_from_the_scheme() {
        assert_eq!(
            parse_endpoint("https://vault.internal"),
            Some(("vault.internal".to_string(), 443))
        );
        assert_eq!(
            parse_endpoint("http://vault.internal"),
            Some(("vault.internal".to_string(), 80))
        );
    }

    /// A trailing path is common in copied URLs and must not become part of
    /// the hostname.
    #[test]
    fn parse_endpoint_drops_a_path() {
        assert_eq!(
            parse_endpoint("https://vault.internal:8200/ui/"),
            Some(("vault.internal".to_string(), 8200))
        );
    }

    #[test]
    fn parse_endpoint_accepts_a_bare_host() {
        assert_eq!(
            parse_endpoint("vault.internal:8200"),
            Some(("vault.internal".to_string(), 8200))
        );
    }

    /// Blank means "this environment has no Vault", which is a skip, not a
    /// failure — and an unknown scheme is not something to guess at.
    #[test]
    fn parse_endpoint_refuses_blank_and_unknown_schemes() {
        assert_eq!(parse_endpoint(""), None);
        assert_eq!(parse_endpoint("   "), None);
        assert_eq!(parse_endpoint("ftp://vault.internal"), None);
    }

    /// A non-2xx reply still proves the forward carries traffic: Vault
    /// answers /v1/sys/health with 429 on standby, 472 on a DR secondary,
    /// 501 uninitialised and 503 sealed. Treating those as failures would
    /// report a working tunnel as broken.
    #[test]
    fn classify_exit_counts_any_http_reply_as_reached() {
        assert_eq!(classify_exit(Some(0)), ProbeVerdict::Reached);
    }

    /// A TLS complaint means bytes crossed the tunnel and something answered,
    /// which is the whole question being asked.
    #[test]
    fn classify_exit_counts_tls_errors_as_reached() {
        assert_eq!(classify_exit(Some(60)), ProbeVerdict::Reached);
        assert_eq!(classify_exit(Some(35)), ProbeVerdict::Reached);
    }

    #[test]
    fn classify_exit_reports_connect_failures_as_unreachable() {
        assert_eq!(classify_exit(Some(7)), ProbeVerdict::Unreachable);
        assert_eq!(classify_exit(Some(28)), ProbeVerdict::Unreachable);
        assert_eq!(classify_exit(Some(52)), ProbeVerdict::Unreachable);
    }

    /// curl missing, or killed, says nothing about the tunnel. Reporting
    /// "not forwarded" here would send someone to debug a healthy one.
    #[test]
    fn classify_exit_is_inconclusive_when_curl_itself_failed() {
        assert_eq!(classify_exit(None), ProbeVerdict::Inconclusive);
        assert_eq!(classify_exit(Some(2)), ProbeVerdict::Inconclusive);
        assert_eq!(classify_exit(Some(6)), ProbeVerdict::Inconclusive);
    }

    /// `--resolve` pins the name to the tunnel's local bind, and the URL
    /// keeps the name so SNI and the Host header stay correct. Curling the
    /// IP directly would break both.
    #[test]
    fn probe_args_pin_the_name_to_the_local_bind() {
        let args = probe_args("vault.internal", 8200, "127.200.20.1", "/v1/sys/health");
        assert!(args.contains(&"--resolve".to_string()));
        assert!(args.contains(&"vault.internal:8200:127.200.20.1".to_string()));
        assert!(args.contains(&"https://vault.internal:8200/v1/sys/health".to_string()));
        // Reachability, not authentication: an internal CA must not read as
        // a broken forward.
        assert!(args.contains(&"-k".to_string()));
    }
}

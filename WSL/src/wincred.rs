//! Read a Windows Credential Manager *generic* credential.
//!
//! Write side is `scripts/store_jsm_credential.ps1`, `cmdkey
//! /generic:ec2_manager/jsm /user:<email> /pass`, or the Credential Manager
//! GUI — all use the same target name and the same username/secret shape, so
//! whichever one wrote the credential, this reads it. They agree on the
//! target and the (username, secret) shape but not on the blob's text
//! encoding, so that is sniffed rather than assumed — see `decode_blob`.
//!
//! `CredReadW` is an ordinary documented Win32 credential API. Nothing here
//! caches, writes, or enumerates — this app has a CrowdStrike quarantine in
//! its history and credential *enumeration* is a flagged pattern.

/// Decode a credential blob whose encoding depends on who wrote it.
///
/// `cmdkey`, the Credential Manager GUI and our own PowerShell writer all
/// store UTF-16LE; other tools store UTF-8. Guessing wrong is silent — the
/// token decodes to garbage and the API answers 401, which looks exactly
/// like a wrong password.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn decode_blob(bytes: &[u8]) -> String {
    // UTF-16LE ASCII text has a zero as every second byte; UTF-8 never has
    // interior NULs. That separates the two cases without a BOM.
    let looks_utf16 = bytes.len() >= 2
        && bytes.len().is_multiple_of(2)
        && bytes.chunks_exact(2).filter(|c| c[1] == 0).count() * 2 > bytes.len() / 2;
    let decoded = if looks_utf16 {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).to_string()
    };
    // Some writers store a terminator inside the blob length (a trailing
    // NUL, or a CRLF from an interactive prompt); left in, it becomes part
    // of the token and produces the same silent 401.
    decoded
        .trim_end_matches(['\r', '\n', '\0'])
        .to_string()
}

/// `(username, secret)` for `target`, or `None` when it is absent,
/// unreadable, or this is not Windows.
#[cfg(not(target_os = "windows"))]
pub fn read_generic(_target: &str) -> Option<(String, String)> {
    None
}

#[cfg(target_os = "windows")]
pub fn read_generic(target: &str) -> Option<(String, String)> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Security::Credentials::{
        CredFree, CredReadW, CREDENTIALW, CRED_TYPE_GENERIC,
    };

    let wide: Vec<u16> = std::ffi::OsStr::new(target)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let mut raw: *mut CREDENTIALW = std::ptr::null_mut();
    // SAFETY: `wide` is NUL-terminated and outlives the call; `raw` is only
    // read when the call reports success, and is freed on every path after.
    let ok = unsafe { CredReadW(wide.as_ptr(), CRED_TYPE_GENERIC, 0, &mut raw) };
    if ok == 0 || raw.is_null() {
        return None;
    }

    let result = unsafe {
        let cred = &*raw;
        let user = if cred.UserName.is_null() {
            String::new()
        } else {
            let mut len = 0usize;
            while *cred.UserName.add(len) != 0 {
                len += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(cred.UserName, len))
        };
        let secret = if cred.CredentialBlob.is_null() {
            String::new()
        } else {
            let bytes = std::slice::from_raw_parts(
                cred.CredentialBlob,
                cred.CredentialBlobSize as usize,
            );
            decode_blob(bytes)
        };
        (user, secret)
    };
    // SAFETY: `raw` came from a successful CredReadW and is freed exactly once.
    unsafe { CredFree(raw as *mut _) };

    if result.0.trim().is_empty() && result.1.trim().is_empty() {
        None
    } else {
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16le(s: &str) -> Vec<u8> {
        s.encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect()
    }

    #[test]
    fn a_utf16le_encoding_decodes_correctly() {
        assert_eq!(decode_blob(&utf16le("abc123")), "abc123");
    }

    #[test]
    fn utf8_bytes_decode_correctly() {
        assert_eq!(decode_blob("abc123".as_bytes()), "abc123");
    }

    #[test]
    fn a_trailing_nul_is_stripped() {
        let mut bytes = utf16le("abc123");
        bytes.extend_from_slice(&0u16.to_le_bytes());
        assert_eq!(decode_blob(&bytes), "abc123");
    }

    #[test]
    fn a_trailing_crlf_is_stripped() {
        let mut bytes = "abc123".as_bytes().to_vec();
        bytes.extend_from_slice(b"\r\n");
        assert_eq!(decode_blob(&bytes), "abc123");
    }

    #[test]
    fn an_empty_blob_yields_an_empty_string() {
        assert_eq!(decode_blob(&[]), "");
    }
}

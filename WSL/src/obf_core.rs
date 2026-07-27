// Keystream obfuscation for compiled-in assets (shell scripts + JSON config).
//
// NOTE: regular `//` comments, not `//!` inner docs — this file is also pulled
// into build.rs with `include!`, where inner-doc attributes are a syntax error.
//
// **This is obfuscation, not encryption.** The seed below ships inside the
// binary, so anyone willing to run a debugger or re-implement this function
// can recover the plaintext. Its only job is to keep the embedded scripts and
// config out of `strings`/hex-dump and casual inspection, so a copycat cannot
// read them straight out of the `.exe`. Do not put anything here that would be
// genuinely harmful to disclose — put real secrets behind IAM instead.
//
// This file is shared verbatim by two compilations so the two never drift:
//   * `build.rs` pulls it in with `include!` and uses `obf_transform` to
//     encrypt each asset at build time, writing `<name>.obf` into `OUT_DIR`.
//   * The library compiles it as the `obf_core` module and uses the same
//     `obf_transform` to decrypt at runtime.
//
// The transform is a keystream XOR, so it is symmetric — the *same* call both
// encrypts and decrypts. No external crates, so it adds no dependencies, no
// binary bloat, and no antivirus/EDR surface (a real crypto library flags
// more readily than a few integer ops).

/// Keystream seed. Arbitrary 64-bit value — change it to re-key every asset.
/// Not a recognizable constant on purpose (a well-known value in a hex dump is
/// a signpost). Keeping it in source means only someone with the repo can
/// produce a build whose blobs decrypt the same way.
pub const OBF_SEED: u64 = 0x53F1_A2C7_9BD4_6E08;

/// XOR `data` against a splitmix64 keystream seeded from [`OBF_SEED`].
///
/// splitmix64 gives a non-repeating byte stream, so the output shows none of
/// the tell-tale periodicity a fixed repeating-key XOR would leave in a hex
/// editor. Applying it twice returns the original bytes, which is why build.rs
/// (encrypt) and the runtime (decrypt) can call the identical function.
pub fn obf_transform(data: &[u8]) -> Vec<u8> {
    let mut state = OBF_SEED;
    data.iter()
        .map(|&byte| {
            // Advance splitmix64.
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            byte ^ (z as u8)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transform_round_trips() {
        let plain = br#"[{"label":"Dev","account_id":"123456789012"}]"#;
        let enc = obf_transform(plain);
        assert_ne!(enc.as_slice(), plain.as_slice(), "should not be plaintext");
        assert_eq!(obf_transform(&enc), plain, "double transform restores input");
    }

    #[test]
    fn transform_hides_structure() {
        // A long run of one byte must not encrypt to a repeating pattern —
        // that's the whole point of a keystream over a repeating key.
        let plain = vec![b'{'; 64];
        let enc = obf_transform(&plain);
        let distinct: std::collections::HashSet<u8> = enc.iter().copied().collect();
        assert!(distinct.len() > 1, "keystream should vary the output");
    }

    #[test]
    fn empty_input_is_empty() {
        assert!(obf_transform(&[]).is_empty());
    }
}

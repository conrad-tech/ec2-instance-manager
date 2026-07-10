// Validates `assets/accounts.json` at compile time. Fails the build if
// any account entry is missing any required field. Every entry must
// specify: label, account_id, region, sort_order, color.

use std::path::Path;

fn main() {
    // Validate the file the binary actually embeds via
    // `include_str!("../assets/accounts.json")` from `src/`, i.e.
    // `<manifest_dir>/assets/accounts.json` (the WSL copy) — NOT the stale
    // root-level `../assets/accounts.json`. Mirrors the features.json check.
    let path = Path::new("assets/accounts.json");
    println!("cargo:rerun-if-changed={}", path.display());

    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(err) => {
            panic!(
                "Build failed: could not read {}: {err}",
                path.display()
            );
        }
    };

    let value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(err) => {
            panic!(
                "Build failed: {} is not valid JSON: {err}",
                path.display()
            );
        }
    };

    let entries = match value.as_array() {
        Some(a) => a,
        None => panic!(
            "Build failed: {} must be a JSON array of account objects",
            path.display()
        ),
    };

    let required_string_fields = ["label", "account_id", "region", "color"];
    let mut problems: Vec<String> = Vec::new();
    for (idx, entry) in entries.iter().enumerate() {
        let label = entry
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or("<no label>");
        let account_id = entry
            .get("account_id")
            .and_then(|v| v.as_str())
            .unwrap_or("<no account_id>");

        let mut missing: Vec<&str> = Vec::new();
        for field in required_string_fields {
            let ok = entry
                .get(field)
                .and_then(|v| v.as_str())
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false);
            if !ok {
                missing.push(field);
            }
        }
        // sort_order must be present as a number (not a string)
        let sort_ok = entry
            .get("sort_order")
            .and_then(|v| v.as_u64())
            .is_some();
        if !sort_ok {
            missing.push("sort_order");
        }

        if !missing.is_empty() {
            problems.push(format!(
                "  [{idx}] label={label} account_id={account_id} — missing: {}",
                missing.join(", ")
            ));
        }
    }

    if !problems.is_empty() {
        panic!(
            "Build failed: accounts.json entries missing required fields:\n{}\n\
             Every account must have: label, account_id, region, sort_order, color.",
            problems.join("\n")
        );
    }

    validate_features();
}

/// Validate `assets/features.json` at compile time. Every documented flag
/// must be present with the correct type; a missing (or mistyped) field
/// fails the build rather than silently defaulting.
fn validate_features() {
    // This is the file bundled via `include_str!("../assets/features.json")`
    // from `src/`, i.e. `<manifest_dir>/assets/features.json`.
    let path = Path::new("assets/features.json");
    println!("cargo:rerun-if-changed={}", path.display());

    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(err) => panic!(
            "Build failed: could not read {}: {err}",
            path.display()
        ),
    };

    let value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(err) => panic!(
            "Build failed: {} is not valid JSON: {err}",
            path.display()
        ),
    };

    let obj = match value.as_object() {
        Some(o) => o,
        None => panic!(
            "Build failed: {} must be a JSON object",
            path.display()
        ),
    };

    let mut missing: Vec<&str> = Vec::new();
    if !obj
        .get("allow_delete_user")
        .map(|v| v.is_boolean())
        .unwrap_or(false)
    {
        missing.push("allow_delete_user (boolean)");
    }
    for field in ["primary_bastion_filter", "secondary_bastion_filter"] {
        if !obj.get(field).map(|v| v.is_string()).unwrap_or(false) {
            missing.push(field);
        }
    }

    if !missing.is_empty() {
        panic!(
            "Build failed: {} is missing required field(s): {}\n\
             Required: allow_delete_user (boolean), primary_bastion_filter \
             (string), secondary_bastion_filter (string).",
            path.display(),
            missing.join(", ")
        );
    }
}

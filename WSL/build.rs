// Validates `assets/accounts.json` at compile time. Fails the build if
// any account entry is missing any required field. Every entry must
// specify: label, account_id, region, sort_order, color.

use std::path::Path;

fn main() {
    let path = Path::new("../assets/accounts.json");
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
}

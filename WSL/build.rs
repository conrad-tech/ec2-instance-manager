// Validates `assets/accounts.json` at compile time. Fails the build if
// any account entry is missing any required field. Every entry must
// specify: label, account_id, region, sort_order, color.

use std::path::Path;

// Shared, dependency-free keystream used to obfuscate the compiled-in assets.
// The runtime compiles the same file as the `obf_core` module; pulling it in
// here with `include!` guarantees build-time encrypt and run-time decrypt can
// never use different logic. See src/obf_core.rs for the (obfuscation-not-
// encryption) threat model.
include!("src/obf_core.rs");

// Shared with the library (as the `forwards_check` module, where its tests
// live) so the build-time check and the schema it checks cannot drift.
include!("src/forwards_check.rs");

// Likewise the `defaults_check` module: the shape checks below say every
// required field is present, and this says the values in them are not still
// the ones this repo ships as a template.
include!("src/defaults_check.rs");

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
    validate_forwards();
    validate_not_template();
    emit_obfuscated_assets();
    embed_windows_icon();
}

/// Compile `assets/app_icon.ico` into the GUI executable as its Win32 icon
/// resource (Windows targets only).
///
/// This is what Explorer, the Start menu, alt-tab and — the case that started
/// this — a **pinned taskbar shortcut** read, because all of them look at the
/// file on disk and never see the icon eframe sets on the live window at
/// runtime. Without a resource here they fall back to the generic executable
/// glyph, so the "e" came and went depending on how the app was launched.
///
/// Three things are deliberate:
///
/// - **It fails soft.** No resource compiler, an unknown target, a failed run
///   — all are `cargo:warning=` and carry on. An icon must never break a
///   build. Because that makes every failure silent, `build_binaries.sh`
///   re-checks the produced exe for a `.rsrc` section and *does* fail: dev
///   builds stay soft, released ones do not.
/// - **The resource compiler differs per target env.** `windres` emits a
///   GNU-format `.o` for `*-pc-windows-gnu`; the MSVC linker wants a `.res`
///   from `rc.exe` (or `llvm-rc`) instead. Both are passed to the linker the
///   same way, via `rustc-link-arg-bin`.
/// - **It runs from `OUT_DIR` on bare filenames.** The repo path contains a
///   space (`/mnt/d/Work Projects/...`) and the mingw tools are careless
///   about quoting the paths they hand to their own child processes; that is
///   exactly the `dlltool` failure documented in CLAUDE.md. Copying the
///   `.ico` next to the generated `.rc` and passing neither a directory keeps
///   a space out of the command entirely.
/// - **It targets `ec2_manager_gui` only.** `ec2_manager` is a console tool.
fn embed_windows_icon() {
    let icon_src = Path::new("assets/app_icon.ico");
    println!("cargo:rerun-if-changed={}", icon_src.display());
    println!("cargo:rerun-if-env-changed=WINDRES");
    println!("cargo:rerun-if-env-changed=RC");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_env != "gnu" && target_env != "msvc" {
        println!(
            "cargo:warning=app icon not embedded: unsupported target env {target_env:?} \
             (expected gnu or msvc)"
        );
        return;
    }

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR is always set for build scripts");
    let out_dir = Path::new(&out_dir);

    if let Err(err) = std::fs::copy(icon_src, out_dir.join("app_icon.ico")) {
        println!(
            "cargo:warning=app icon not embedded: could not copy {}: {err}",
            icon_src.display()
        );
        return;
    }
    // Resource id 1: the lowest-numbered ICON resource is the one Windows
    // shows for the file, so this must stay 1.
    if let Err(err) = std::fs::write(out_dir.join("app_icon.rc"), "1 ICON \"app_icon.ico\"\n") {
        println!("cargo:warning=app icon not embedded: could not write app_icon.rc: {err}");
        return;
    }

    // Which resource compiler to drive, how to call it, and what it produces.
    // The two differ only here; the run-and-link loop below is shared, so a
    // change to how the result reaches the linker cannot apply to one target
    // env and not the other.
    let (candidates, args, product, hint) = if target_env == "gnu" {
        let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
        let mut candidates: Vec<String> = Vec::new();
        if let Ok(explicit) = std::env::var("WINDRES") {
            candidates.push(explicit);
        }
        candidates.push(match arch.as_str() {
            "x86" => "i686-w64-mingw32-windres".to_string(),
            _ => "x86_64-w64-mingw32-windres".to_string(),
        });
        // Building natively under MSYS2/MinGW, where the tool is unprefixed.
        candidates.push("windres".to_string());
        (
            candidates,
            vec!["-i", "app_icon.rc", "-O", "coff", "-o", "app_icon.o"],
            "app_icon.o",
            "Install binutils-mingw-w64, or set WINDRES.",
        )
    } else {
        let mut candidates: Vec<String> = Vec::new();
        if let Ok(explicit) = std::env::var("RC") {
            candidates.push(explicit);
        }
        // rc.exe ships with the Windows SDK and is on PATH inside a Visual
        // Studio developer prompt; llvm-rc is the drop-in that is not tied to
        // one. Neither is given `/nologo` — llvm-rc's handling of it has
        // varied, and the banner is harmless.
        candidates.push("rc.exe".to_string());
        candidates.push("llvm-rc".to_string());
        (
            candidates,
            vec!["/fo", "app_icon.res", "app_icon.rc"],
            "app_icon.res",
            "Build from a Visual Studio developer prompt, install llvm-rc, or set RC.",
        )
    };

    for tool in &candidates {
        let result = std::process::Command::new(tool)
            .current_dir(out_dir)
            .args(&args)
            .output();
        match result {
            // Not installed under this name — try the next spelling.
            Err(_) => continue,
            Ok(output) if !output.status.success() => {
                println!(
                    "cargo:warning=app icon not embedded: {tool} failed ({}): {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                );
                return;
            }
            Ok(_) => {
                // Both a COFF object and a .res are accepted as plain linker
                // inputs by their respective linkers.
                println!(
                    "cargo:rustc-link-arg-bin=ec2_manager_gui={}",
                    out_dir.join(product).display()
                );
                return;
            }
        }
    }

    println!(
        "cargo:warning=app icon not embedded: no resource compiler found (tried {}). {hint}",
        candidates.join(", ")
    );
}

/// Encrypt each compiled-in asset with the shared keystream and write the
/// result to `OUT_DIR/<name>.obf`. The runtime `include_bytes!`s these blobs
/// and calls `obf_core::obf_transform` to recover the plaintext, so the raw
/// scripts and JSON never appear in the binary as readable strings.
///
/// Only the *shape* changes: the same bytes are still embedded, just scrambled.
/// CR-stripping for the shell scripts stays at runtime (unchanged), so the
/// blobs here are the files verbatim.
fn emit_obfuscated_assets() {
    let out_dir = std::env::var("OUT_DIR")
        .expect("OUT_DIR is always set for build scripts");

    // (output name, source path). Output names match the include_bytes! calls
    // in accounts.rs / features.rs / ec2_manager_gui.rs.
    let assets = [
        ("accounts.json.obf", "assets/accounts.json"),
        ("features.json.obf", "assets/features.json"),
        ("forwards.json.obf", "assets/forwards.json"),
        ("create_new_user.sh.obf", "assets/scripts/create_new_user.sh"),
        ("delete_user.sh.obf", "assets/scripts/delete_user.sh"),
        // User-management shell that used to be inline string literals in the
        // GUI (verify/diagnostics/preflight/secondary-mirror). Each is a
        // template with a `__USER__` placeholder the runtime substitutes after
        // decrypting; see the `deobf_*` call sites in ec2_manager_gui.rs.
        ("create_diagnostics.sh.obf", "assets/scripts/create_diagnostics.sh"),
        ("delete_preflight.sh.obf", "assets/scripts/delete_preflight.sh"),
        ("delete_diagnostics.sh.obf", "assets/scripts/delete_diagnostics.sh"),
        ("secondary_mirror.sh.obf", "assets/scripts/secondary_mirror.sh"),
        ("sudoers_grant.sh.obf", "assets/scripts/sudoers_grant.sh"),
        ("user_sync_dump.sh.obf", "assets/scripts/user_sync_dump.sh"),
        ("reaper_fix.sh.obf", "assets/scripts/reaper_fix.sh"),
        ("reaper_docker_ps.sh.obf", "assets/scripts/reaper_docker_ps.sh"),
        ("reaper_probe.sh.obf", "assets/scripts/reaper_probe.sh"),
    ];

    for (out_name, src) in assets {
        println!("cargo:rerun-if-changed={src}");
        let plain = std::fs::read(src)
            .unwrap_or_else(|err| panic!("Build failed: could not read {src}: {err}"));
        let obfuscated = obf_transform(&plain);
        let dest = Path::new(&out_dir).join(out_name);
        std::fs::write(&dest, obfuscated)
            .unwrap_or_else(|err| panic!("Build failed: could not write {}: {err}", dest.display()));
    }
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

/// Validate `assets/forwards.json` at compile time, and **fail the build when
/// it declares no port forwards at all**.
///
/// The runtime parser is deliberately fail-soft — a malformed file yields no
/// forwards rather than an error, so a bad config can never block a VS Code
/// launch — which means every mistake in that file is silent. An empty
/// `environments` block, a stray comma and a `"port": "8443"` written as a
/// string all produce the same thing: a Port Forwards window with nothing in
/// it, ports the app never binds, and not one word saying why. Nothing on the
/// running side can tell those apart from "this site has no forwards", so the
/// question is asked here instead, where it can be shouted about.
///
/// `ALLOW_NO_FORWARDS=1` allows a build with nothing declared, for a developer
/// who has no endpoints to put in. It does **not** excuse a file that is
/// wrong: every shape problem is fatal either way.
fn validate_forwards() {
    // The file bundled through OUT_DIR by `emit_obfuscated_assets` and read
    // back by `forwards::bundled()`, i.e. `<manifest_dir>/assets/forwards.json`.
    let path = Path::new("assets/forwards.json");
    println!("cargo:rerun-if-changed={}", path.display());
    println!("cargo:rerun-if-env-changed={ALLOW_NO_FORWARDS_ENV}");

    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(err) => panic!("Build failed: could not read {}: {err}", path.display()),
    };

    let require_forwards = std::env::var_os(ALLOW_NO_FORWARDS_ENV).is_none();
    let problems = check_forwards_json(&raw, require_forwards);
    if problems.is_empty() {
        return;
    }

    let listed = problems
        .iter()
        .map(|p| format!("  - {p}"))
        .collect::<Vec<_>>()
        .join("\n");
    panic!(
        "\nBuild failed: {} does not declare usable port forwards:\n{listed}\n\n\
         Every environment the app should forward for needs an entry under \
         `environments`,\nkeyed by that instance's MMODAL_ENV tag. Copy a block out of \
         `_example_environments`\nin that file and edit it:\n\n  \
         \"environments\": {{\n    \
         \"DEV1\": [\n      \
         {{ \"ip\": \"127.200.10.1\", \"host\": \"uweb01.dev1.example.net\" }},\n      \
         {{ \"ip\": \"127.200.10.2\", \"host\": \"admin01.dev1.example.net\", \"port\": 8443 }}\n    \
         ]\n  }}\n\n  \
         ip    loopback address bound on this machine; each environment needs its own\n        \
         addresses, since every environment's tunnel runs at once.\n  \
         host  remote DNS name, resolved on the bastion.\n  \
         port  optional — omit it and the port comes from port_rules / default_port.\n\n\
         This is checked here because it cannot be noticed later: a malformed or empty\n\
         forwards.json yields NO forwards at runtime rather than an error, so the app\n\
         comes up looking healthy with an empty Port Forwards window.\n\n\
         Set {ALLOW_NO_FORWARDS_ENV}=1 to build with no forwards declared, or build with\n\
         `./scripts/build_binaries.sh test`, which sets it for you.\n",
        path.display()
    );
}

/// Fail the build when `assets/accounts.json` or `assets/features.json` is
/// still carrying the values this repo ships as a template.
///
/// The two validators above check *shape* — every required field present with
/// the right type — and a wholly unedited pair of files passes both, because
/// the template is well-formed on purpose. So the build that comes out is
/// valid, runs, and is wrong in the one way nothing downstream can notice: it
/// offers three AWS accounts that do not exist, a git host called
/// `github.YOUR-ENTERPRISE.com`, and an access-email path whose only permitted
/// mail domain is `test.com`. Every one of those fails later, somewhere else,
/// as something else — an empty inventory, a clone that will not resolve, a
/// create that quietly opens a draft instead of sending.
///
/// This runs last, so a file that is malformed or missing a field is reported
/// by the check that actually knows what to do about it.
///
/// The bypass is the same one the forwards check uses — `ALLOW_NO_FORWARDS=1`,
/// set for you by `./scripts/build_binaries.sh test`. One variable covers all
/// three checks deliberately: they ask the same question about three files,
/// and a developer building an unconfigured tree wants past all of them or
/// none. Do not release a build made with it.
fn validate_not_template() {
    // `rerun-if-changed` for both files, and for the variable, is already
    // printed by the validators above.
    if std::env::var_os(ALLOW_NO_FORWARDS_ENV).is_some() {
        return;
    }

    let mut problems: Vec<String> = Vec::new();
    for (path, check) in [
        (
            "assets/accounts.json",
            check_accounts_json as fn(&str) -> Vec<String>,
        ),
        ("assets/features.json", check_features_json),
    ] {
        // Unreadable is the other validators' business; they have already
        // panicked on it by the time this runs.
        let Ok(raw) = std::fs::read_to_string(path) else {
            continue;
        };
        problems.extend(check(&raw).into_iter().map(|p| format!("  - {path}: {p}")));
    }

    if problems.is_empty() {
        return;
    }

    panic!(
        "\nBuild failed: the bundled configuration is still this repo's template:\n{}\n\n\
         assets/accounts.json must name the AWS accounts this build is for — label,\n\
         account_id, region, sort_order and color per account, plus each account's\n\
         environments and their vault_addr. See README \"Account configuration\".\n\n\
         assets/features.json must name your site wherever a feature is switched on:\n\
         personal_scripts.git_host and its default_scripts (checked only once\n\
         personal_scripts.allowed_users names somebody), and access_email's mail\n\
         domains and encrypt_template_guid (checked only while access_email.enabled\n\
         is true). See README \"Feature flags\" and ACCESS_EMAIL_WALKTHROUGH.md.\n\n\
         This is checked here because it cannot be noticed later. The template is\n\
         valid JSON with every required field, so the app comes up looking healthy\n\
         and is simply pointed at nothing — an empty inventory reads exactly like an\n\
         account you have no access to.\n\n\
         Set {ALLOW_NO_FORWARDS_ENV}=1 to build against the template anyway, or build with\n\
         `./scripts/build_binaries.sh test`, which sets it for you. The same variable\n\
         waives the forwards.json check; do not release a build made with it.\n",
        problems.join("\n")
    );
}

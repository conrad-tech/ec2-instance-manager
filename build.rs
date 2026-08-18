// Refuse to build the stale copy of the project that lives at the repo root.
//
// The code that actually ships is in `WSL/`. This root copy was left behind
// and has drifted badly — as of 2026-08-14 its `src/bin/ec2_manager_gui.rs`
// is roughly a third the size of the real one and three months older, and it
// has no `build.rs`, no `assets/app_icon.*` and no `with_icon` call.
//
// That last part is what made this worth a guard rather than a note. A GUI
// built here comes out with neither app-icon path wired up: no `.rsrc`
// section for Explorer and pinned shortcuts, and no runtime icon for the
// window. The result looks like a *broken build* — a generic Windows glyph
// on the taskbar — rather than like the wrong directory, so it cost real
// time to trace back to its source. Everything else missing from three
// months of drift fails just as quietly.
//
// Nothing is deleted; the old tree is still here to read. It just cannot be
// built by accident. Set ALLOW_STALE_ROOT_BUILD=1 if you deliberately need
// to compile it.

fn main() {
    println!("cargo:rerun-if-env-changed=ALLOW_STALE_ROOT_BUILD");

    if std::env::var("ALLOW_STALE_ROOT_BUILD").as_deref() == Ok("1") {
        println!(
            "cargo:warning=building the stale root project deliberately \
             (ALLOW_STALE_ROOT_BUILD=1) — this is not the code that ships"
        );
        return;
    }

    panic!(
        "\n\
         \n\
         This is the stale copy of the project at the repo root — not the code that ships.\n\
         \n\
         Build from the WSL/ directory instead:\n\
         \n\
             cd WSL && cargo build --release --features gui\n\
         \n\
         Or, for a Windows release (run this on Linux/WSL, not on Windows):\n\
         \n\
             cd WSL && ./scripts/build_binaries.sh windows\n\
         \n\
         Binaries built here are missing months of changes, including both app-icon\n\
         paths, so they show the generic Windows glyph on the taskbar.\n\
         \n\
         Set ALLOW_STALE_ROOT_BUILD=1 if you really mean to build this tree.\n\
         "
    );
}

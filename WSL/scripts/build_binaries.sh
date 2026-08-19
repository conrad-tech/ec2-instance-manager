#!/usr/bin/env bash
set -euo pipefail

CLI_APP_NAME="ec2_manager"
GUI_APP_NAME="ec2_manager_gui"
APP_VERSION="1.1"
# WALKTHROUGH.md is a full feature-by-feature spec of the GUI, so it is opt-in
# rather than shipped by default. Set by --with-walkthrough (see usage()).
INCLUDE_WALKTHROUGH=0
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Cargo's build directory. The mingw toolchain's `dlltool` (invoked for crates
# that use raw-dylib imports, e.g. chrono -> windows-link) does not quote the
# paths it passes to the assembler, so it fails outright when the build
# directory contains a space — as it does under "/mnt/d/Work Projects/...".
# When ROOT_DIR has a space, build into a space-free scratch dir instead. The
# artifacts are copied out to dist/ either way, so nothing else changes.
if [[ "$ROOT_DIR" == *" "* ]]; then
  TARGET_DIR="${TMPDIR:-/tmp}/ec2-manager-build/target"
  export CARGO_TARGET_DIR="$TARGET_DIR"
  echo "info: repo path contains a space; building into $TARGET_DIR"
else
  TARGET_DIR="${ROOT_DIR}/target"
fi

# Keep build-machine paths out of shipped binaries. Rust compiles source paths
# in as string literals (panic!/#[track_caller] locations), so they ship
# regardless of debuginfo or `strip` — which is how "/home/<user>/.cargo/..."
# and the full repo path ended up readable in released .exe files. The
# `trim-paths` profile key would be tidier but is not stable as of cargo 1.94.
#
# Uses CARGO_ENCODED_RUSTFLAGS (0x1f-separated) rather than RUSTFLAGS, which is
# split on spaces — and ROOT_DIR contains one under "/mnt/d/Work Projects/...",
# which silently mangles the flag into an invalid one.
RUSTFLAGS_SEP=$'\x1f'
REMAP_FLAGS="--remap-path-prefix=${CARGO_HOME:-${HOME}/.cargo}/registry=/deps"
REMAP_FLAGS="${REMAP_FLAGS}${RUSTFLAGS_SEP}--remap-path-prefix=${ROOT_DIR}=/src"
if [[ -n "${RUSTFLAGS:-}" ]]; then
  # Preserve caller-supplied flags, then hand everything over in encoded form.
  REMAP_FLAGS="${RUSTFLAGS// /${RUSTFLAGS_SEP}}${RUSTFLAGS_SEP}${REMAP_FLAGS}"
  unset RUSTFLAGS
fi
export CARGO_ENCODED_RUSTFLAGS="${CARGO_ENCODED_RUSTFLAGS:+${CARGO_ENCODED_RUSTFLAGS}${RUSTFLAGS_SEP}}${REMAP_FLAGS}"

DIST_DIR="${ROOT_DIR}/dist"
LINUX_DIST_DIR="${DIST_DIR}/linux"
WINDOWS_DIST_DIR="${DIST_DIR}/windows"
# HOST_TRIPLE is computed in main() AFTER ensure_rust(), since rustc may not be
# installed yet on a fresh WSL/machine.
HOST_TRIPLE=""

usage() {
  cat <<USAGE
Usage: $0 [native|all|linux|windows] [--with-walkthrough]

Build release binaries for Pop!_OS (linux) and Windows.

Output layout:
  dist/linux/
  dist/windows/

Modes:
  native   Build only host-native binaries
  all      Build linux + windows binaries (best on Linux host)
  linux    Build Linux binaries
  windows  Build Windows binaries

Options:
  --with-walkthrough
           Include WALKTHROUGH.md in the Windows zip and extracted folder.
           Off by default: the walkthrough documents every feature, panel and
           shortcut, so it doubles as a spec for anyone reimplementing the GUI.
           Use it for internal builds; omit it for anything handed outside.
USAGE
}

require_cmd() {
  local cmd="$1"
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "error: required command not found: $cmd" >&2
    exit 1
  fi
}

# Write SHA-256 checksums for every file passed, into <dir>/SHA256SUMS.txt.
# Lets whoever receives a release verify it is byte-for-byte the build you
# published — a mismatched or extra copy is a tampered/unofficial one. Uses
# whatever hashing tool is available (sha256sum, then shasum, then openssl,
# then python3) so it works across Linux/macOS/Git-Bash build hosts. The
# output format matches `sha256sum -c`, so users can verify with that.
write_checksums() {
  local dir="$1"; shift
  local out="${dir}/SHA256SUMS.txt"
  : > "$out"

  local f name
  for f in "$@"; do
    [[ -f "$f" ]] || continue
    name="$(basename "$f")"
    if command -v sha256sum >/dev/null 2>&1; then
      (cd "$dir" && sha256sum "$name")
    elif command -v shasum >/dev/null 2>&1; then
      (cd "$dir" && shasum -a 256 "$name")
    elif command -v openssl >/dev/null 2>&1; then
      printf '%s  %s\n' "$(openssl dgst -sha256 -r "$f" | awk '{print $1}')" "$name"
    elif command -v python3 >/dev/null 2>&1; then
      printf '%s  %s\n' \
        "$(python3 -c 'import hashlib,sys;print(hashlib.sha256(open(sys.argv[1],"rb").read()).hexdigest())' "$f")" \
        "$name"
    else
      echo "warning: no SHA-256 tool found; skipping checksums for $dir" >&2
      rm -f "$out"
      return 0
    fi
  done >> "$out"

  echo "info: wrote checksums: $out"
}

# Ensure the Rust toolchain (rustc + cargo + rustup) is present. If Rust is
# installed but not on PATH, load ~/.cargo/env; if it's missing entirely,
# install it non-interactively via rustup. Safe to run every build.
ensure_rust() {
  # Always load an existing rustup/cargo install first, so it's visible even in
  # a non-login shell whose PATH doesn't include ~/.cargo/bin. This is what makes
  # the "already installed" check below short-circuit instead of reinstalling.
  if [[ -f "$HOME/.cargo/env" ]]; then
    # shellcheck disable=SC1091
    source "$HOME/.cargo/env"
  fi

  # cargo + rustc are all we need to build; if present, Rust is already set up.
  if command -v cargo >/dev/null 2>&1 && command -v rustc >/dev/null 2>&1; then
    echo "info: using $(rustc --version)"
    return 0
  fi

  echo "info: Rust toolchain not found - installing via rustup (non-interactive)..."
  require_cmd curl
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"
  require_cmd cargo
  require_cmd rustc
  echo "info: installed $(rustc --version)"
}

# Ensure the mingw-w64 cross toolchain (x86_64-w64-mingw32-gcc) is present for
# the Windows GNU target. Auto-installs via apt when available.
ensure_mingw() {
  if command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1; then
    return 0
  fi

  echo "info: mingw-w64 cross toolchain not found - installing..."
  if command -v apt-get >/dev/null 2>&1; then
    sudo apt-get update && sudo apt-get install -y mingw-w64
  else
    echo "error: missing cross-linker x86_64-w64-mingw32-gcc for Windows GNU target." >&2
    echo "hint: install the mingw-w64 toolchain for your distro." >&2
    exit 1
  fi
  require_cmd x86_64-w64-mingw32-gcc
}

ensure_target() {
  local target="$1"

  if rustup target list --installed | grep -qx "$target"; then
    return 0
  fi

  echo "info: installing Rust target: $target"
  rustup target add "$target"
}

target_output_dir() {
  local target="$1"
  if [[ "$target" == *"windows"* ]]; then
    echo "$WINDOWS_DIST_DIR"
  else
    echo "$LINUX_DIST_DIR"
  fi
}

# Rename with version suffix, e.g. ec2_manager_gui.exe -> ec2_manager_gui_${APP_VERSION}.exe
# Shared by copy_artifact and verify_windows_icon so the two cannot disagree
# about which file was just written.
versioned_name() {
  local artifact_name="$1"

  if [[ "$artifact_name" == *.exe ]]; then
    local base="${artifact_name%.exe}"
    echo "${base}_${APP_VERSION}.exe"
  elif [[ "$artifact_name" == *.* ]]; then
    local base="${artifact_name%.*}"
    local ext="${artifact_name##*.}"
    echo "${base}_${APP_VERSION}.${ext}"
  else
    echo "${artifact_name}_${APP_VERSION}"
  fi
}

copy_artifact() {
  local target="$1"
  local artifact_name="$2"

  local source_dir
  if [[ "$target" == "$HOST_TRIPLE" ]]; then
    source_dir="$TARGET_DIR/release"
  else
    source_dir="$TARGET_DIR/${target}/release"
  fi

  local out_dir
  out_dir="$(target_output_dir "$target")"
  mkdir -p "$out_dir"

  local dest_name
  dest_name="$(versioned_name "$artifact_name")"

  cp "${source_dir}/${artifact_name}" "${out_dir}/${dest_name}"
}

# Refuse to ship a GUI exe that carries no icon resource.
#
# `embed_windows_icon()` in build.rs fails **soft** by design — an icon must
# not break a developer's build — so every way of losing it is silent: no
# `windres` on PATH, an MSVC target without `rc.exe`, or (the one that
# actually happened) building the stale project at the repo root, whose
# source has no icon code at all. The result is an exe with no `.rsrc`
# section, which Explorer, the Start menu and a pinned taskbar shortcut all
# render with the generic executable glyph.
#
# A release is the point where that stops being tolerable, so the check is
# here rather than in build.rs: dev builds stay soft, published ones do not.
# Set SKIP_ICON_VERIFY=1 to override on a host with no objdump.
verify_windows_icon() {
  local exe="$1"

  if [[ "${SKIP_ICON_VERIFY:-}" == "1" ]]; then
    echo "warning: SKIP_ICON_VERIFY=1 — not checking $(basename "$exe") for an icon resource" >&2
    return 0
  fi

  local dumper=""
  local candidate
  for candidate in x86_64-w64-mingw32-objdump objdump llvm-objdump; do
    if command -v "$candidate" >/dev/null 2>&1; then
      dumper="$candidate"
      break
    fi
  done
  if [[ -z "$dumper" ]]; then
    echo "error: cannot verify the app icon in $(basename "$exe"): no objdump found" >&2
    echo "       (tried x86_64-w64-mingw32-objdump, objdump, llvm-objdump)" >&2
    echo "       Install binutils, or re-run with SKIP_ICON_VERIFY=1 to bypass." >&2
    exit 1
  fi

  # The .rsrc size is compared against the source .ico rather than a magic
  # number: the section is essentially the icon file plus a small directory,
  # so anything under half its size means the images did not make it in.
  local ico="${ROOT_DIR}/assets/app_icon.ico"
  local floor=1024
  if [[ -f "$ico" ]]; then
    floor=$(( $(wc -c < "$ico") / 2 ))
  fi

  local size_hex
  size_hex="$("$dumper" -h "$exe" 2>/dev/null | awk '$2 == ".rsrc" { print $3; exit }')"

  if [[ -z "$size_hex" ]]; then
    echo "error: $(basename "$exe") has no .rsrc section — the app icon was not embedded." >&2
    echo "       Explorer and any pinned shortcut will show the generic exe glyph." >&2
    echo "       Likely causes: windres/rc.exe missing, an unsupported target, or" >&2
    echo "       building the stale project at the repo root instead of WSL/." >&2
    echo "       Re-run the build and check for a 'cargo:warning=app icon not embedded' line." >&2
    exit 1
  fi

  local size=$((16#$size_hex))
  if (( size < floor )); then
    echo "error: $(basename "$exe") has a .rsrc section of only ${size} bytes (expected >= ${floor})." >&2
    echo "       The icon resource looks truncated or replaced." >&2
    exit 1
  fi

  echo "info: app icon verified in $(basename "$exe") (.rsrc = ${size} bytes)"
}

copy_windows_runtime_dlls() {
  local target="$1"

  if [[ "$target" != "x86_64-pc-windows-gnu" ]]; then
    return 0
  fi

  local gcc_cmd="${EC2_MANAGER_MINGW_GCC:-}"
  if [[ -z "$gcc_cmd" ]]; then
    if command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1; then
      gcc_cmd="x86_64-w64-mingw32-gcc"
    elif command -v gcc >/dev/null 2>&1; then
      gcc_cmd="gcc"
    elif command -v cc >/dev/null 2>&1; then
      gcc_cmd="cc"
    else
      echo "error: unable to locate a mingw gcc to resolve runtime DLLs" >&2
      echo "hint: ensure mingw-w64 toolchain is installed or set EC2_MANAGER_MINGW_GCC" >&2
      exit 1
    fi
  fi

  local dll_names=(
    "libgcc_s_seh-1.dll"
    "libstdc++-6.dll"
    "libwinpthread-1.dll"
  )

  for dll in "${dll_names[@]}"; do
    local dll_path
    dll_path="$("$gcc_cmd" -print-file-name="$dll")"

    if [[ -z "$dll_path" || "$dll_path" == "$dll" || ! -f "$dll_path" ]]; then
      echo "error: unable to locate required Windows runtime DLL: $dll" >&2
      echo "hint: ensure mingw-w64 is fully installed or set EC2_MANAGER_MINGW_GCC" >&2
      exit 1
    fi

    cp "$dll_path" "$WINDOWS_DIST_DIR/$dll"
  done
}

package_windows_zip() {
  local zip_path="${WINDOWS_DIST_DIR}/ec2_manager_windows_${APP_VERSION}.zip"
  # Copy walkthrough into dist dir for packaging, only when opted in. Otherwise
  # remove any copy staged by an earlier --with-walkthrough run, so the zip
  # candidate below finds nothing and the doc stays out of the artifact.
  if [[ "$INCLUDE_WALKTHROUGH" == "1" && -f "${ROOT_DIR}/WALKTHROUGH.md" ]]; then
    cp "${ROOT_DIR}/WALKTHROUGH.md" "${WINDOWS_DIST_DIR}/WALKTHROUGH.md"
  else
    rm -f "${WINDOWS_DIST_DIR}/WALKTHROUGH.md"
  fi
  # The access-email helper ships as a plain file next to the GUI exe (the GUI
  # runs it from its own directory). It is intentionally NOT embedded in the
  # binary - keeping the PowerShell out of the .exe avoids EDR false positives.
  if [[ -f "${ROOT_DIR}/assets/scripts/send_access_email.ps1" ]]; then
    cp "${ROOT_DIR}/assets/scripts/send_access_email.ps1" \
       "${WINDOWS_DIST_DIR}/send_access_email.ps1"
  fi
  # Same treatment for the on-call escalation send helper, for the same
  # reason: it is run from the file beside the exe, never written to %TEMP%
  # and executed from there, which is a pattern EDRs quarantine on sight.
  if [[ -f "${ROOT_DIR}/assets/scripts/send_escalation.ps1" ]]; then
    cp "${ROOT_DIR}/assets/scripts/send_escalation.ps1" \
       "${WINDOWS_DIST_DIR}/send_escalation.ps1"
  fi
  # Same treatment for the optional fed sign-in helper, for the same reason.
  if [[ -f "${ROOT_DIR}/assets/scripts/fed_login.ps1" ]]; then
    cp "${ROOT_DIR}/assets/scripts/fed_login.ps1" \
       "${WINDOWS_DIST_DIR}/fed_login.ps1"
  fi
  local candidates=(
    "${WINDOWS_DIST_DIR}/${CLI_APP_NAME}_${APP_VERSION}.exe"
    "${WINDOWS_DIST_DIR}/${GUI_APP_NAME}_${APP_VERSION}.exe"
    "${WINDOWS_DIST_DIR}/send_access_email.ps1"
    "${WINDOWS_DIST_DIR}/send_escalation.ps1"
    "${WINDOWS_DIST_DIR}/fed_login.ps1"
    "${WINDOWS_DIST_DIR}/WALKTHROUGH.md"
    "${WINDOWS_DIST_DIR}/libgcc_s_seh-1.dll"
    "${WINDOWS_DIST_DIR}/libstdc++-6.dll"
    "${WINDOWS_DIST_DIR}/libwinpthread-1.dll"
  )
  local files=()

  for candidate in "${candidates[@]}"; do
    if [[ -f "$candidate" ]]; then
      files+=("$candidate")
    fi
  done

  if [[ "${#files[@]}" -eq 0 ]]; then
    echo "warning: no Windows artifacts found to package; skipping zip creation"
    return 0
  fi

  require_cmd zip
  local extract_dir="${WINDOWS_DIST_DIR}/ec2_manager_windows"
  rm -rf "$extract_dir"
  rm -f "$zip_path"
  zip -q -j "$zip_path" "${files[@]}"
  mkdir -p "$extract_dir"
  unzip -qo "$zip_path" -d "$extract_dir"
  # User-facing README goes ONLY in the extracted folder (not the zip), so
  # someone opening the folder has a quick "what is this / how to run" guide.
  # The WALKTHROUGH is in both, but only under --with-walkthrough.
  if [[ -f "${ROOT_DIR}/USER_README.md" ]]; then
    cp "${ROOT_DIR}/USER_README.md" "${extract_dir}/README.md"
  fi
  echo "info: packaged Windows zip: $zip_path"
  echo "info: extracted to: $extract_dir"

  # Checksums for the zip and the loose binaries next to it.
  write_checksums "$WINDOWS_DIST_DIR" \
    "$zip_path" \
    "${WINDOWS_DIST_DIR}/${CLI_APP_NAME}_${APP_VERSION}.exe" \
    "${WINDOWS_DIST_DIR}/${GUI_APP_NAME}_${APP_VERSION}.exe"
}

package_linux_zip() {
  local zip_path="${LINUX_DIST_DIR}/ec2_manager_linux_${APP_VERSION}.zip"
  # Must match the versioned names copy_artifact actually writes (e.g.
  # ec2_manager_gui_1.1) — otherwise nothing is ever found to package.
  local candidates=(
    "${LINUX_DIST_DIR}/${CLI_APP_NAME}_${APP_VERSION}"
    "${LINUX_DIST_DIR}/${GUI_APP_NAME}_${APP_VERSION}"
  )
  local files=()

  for candidate in "${candidates[@]}"; do
    if [[ -f "$candidate" ]]; then
      files+=("$candidate")
    fi
  done

  if [[ "${#files[@]}" -eq 0 ]]; then
    echo "warning: no Linux artifacts found to package; skipping zip creation"
    return 0
  fi

  require_cmd zip
  rm -f "$zip_path"
  zip -q -j "$zip_path" "${files[@]}"
  echo "info: packaged Linux zip: $zip_path"

  write_checksums "$LINUX_DIST_DIR" "$zip_path" "${files[@]}"
}

build_for_target() {
  local target="$1"

  echo "info: building release target: $target"

  if [[ "$target" == "$HOST_TRIPLE" ]]; then
    (cd "$ROOT_DIR" && cargo build --release --bin "$CLI_APP_NAME")
    if [[ "$target" == *"windows"* ]]; then
      copy_artifact "$target" "${CLI_APP_NAME}.exe"
    else
      copy_artifact "$target" "$CLI_APP_NAME"
    fi

    (cd "$ROOT_DIR" && cargo build --release --features gui --bin "$GUI_APP_NAME")
    if [[ "$target" == *"windows"* ]]; then
      copy_artifact "$target" "${GUI_APP_NAME}.exe"
      verify_windows_icon "$(target_output_dir "$target")/$(versioned_name "${GUI_APP_NAME}.exe")"
    else
      copy_artifact "$target" "$GUI_APP_NAME"
    fi
    if [[ "$target" == *"windows"* ]]; then
      copy_windows_runtime_dlls "$target"
      package_windows_zip
    else
      package_linux_zip
    fi
    return 0
  fi

  ensure_target "$target"

  if [[ "$target" == "x86_64-pc-windows-gnu" ]]; then
    ensure_mingw
    export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc
  fi

  (cd "$ROOT_DIR" && cargo build --release --target "$target" --bin "$CLI_APP_NAME")
  if [[ "$target" == *"windows"* ]]; then
    copy_artifact "$target" "${CLI_APP_NAME}.exe"
  else
    copy_artifact "$target" "$CLI_APP_NAME"
  fi

  (cd "$ROOT_DIR" && cargo build --release --target "$target" --features gui --bin "$GUI_APP_NAME")
  if [[ "$target" == *"windows"* ]]; then
    copy_artifact "$target" "${GUI_APP_NAME}.exe"
    verify_windows_icon "$(target_output_dir "$target")/$(versioned_name "${GUI_APP_NAME}.exe")"
    copy_windows_runtime_dlls "$target"
    package_windows_zip
  else
    copy_artifact "$target" "$GUI_APP_NAME"
    package_linux_zip
  fi
}

resolve_targets() {
  local mode="$1"
  local os="$2"
  local host_triple="$3"

  case "$mode" in
    native)
      echo "$host_triple"
      ;;
    linux)
      echo "x86_64-unknown-linux-gnu"
      ;;
    windows)
      if [[ "$os" == "Linux" ]]; then
        echo "x86_64-pc-windows-gnu"
      else
        echo "x86_64-pc-windows-msvc"
      fi
      ;;
    all)
      if [[ "$os" == "Linux" ]]; then
        echo "x86_64-unknown-linux-gnu"
        echo "x86_64-pc-windows-gnu"
      else
        echo "$host_triple"
      fi
      ;;
    *)
      return 1
      ;;
  esac
}

main() {
  local mode=""
  while [[ $# -gt 0 ]]; do
    case "$1" in
      -h|--help)
        usage
        exit 0
        ;;
      --with-walkthrough)
        INCLUDE_WALKTHROUGH=1
        shift
        ;;
      -*)
        echo "error: unknown option: $1" >&2
        usage
        exit 1
        ;;
      *)
        if [[ -n "$mode" ]]; then
          echo "error: unexpected argument: $1" >&2
          usage
          exit 1
        fi
        mode="$1"
        shift
        ;;
    esac
  done
  mode="${mode:-all}"

  # Bootstrap the toolchain before anything that needs rustc (e.g. HOST_TRIPLE).
  ensure_rust
  HOST_TRIPLE="$(rustc -vV | awk '/host:/ {print $2}')"

  mkdir -p "$LINUX_DIST_DIR" "$WINDOWS_DIST_DIR"

  local os
  os="$(uname -s)"

  if [[ "$mode" == "all" && "$os" != "Linux" ]]; then
    echo "warning: on non-Linux hosts, full linux+windows cross-build may require extra toolchains."
  fi

  local targets=()
  if ! mapfile -t targets < <(resolve_targets "$mode" "$os" "$HOST_TRIPLE"); then
    echo "error: invalid mode: $mode" >&2
    usage
    exit 1
  fi

  for target in "${targets[@]}"; do
    build_for_target "$target"
  done

  echo "info: build complete"
  echo "info: linux artifacts:   $LINUX_DIST_DIR"
  echo "info: windows artifacts: $WINDOWS_DIST_DIR"
  echo
  ls -lh "$LINUX_DIST_DIR" || true
  ls -lh "$WINDOWS_DIST_DIR" || true
}

if [[ "${EC2_MANAGER_BUILD_LIB_ONLY:-0}" != "1" ]]; then
  main "$@"
fi

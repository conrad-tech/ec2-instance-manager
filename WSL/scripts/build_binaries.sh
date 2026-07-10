#!/usr/bin/env bash
set -euo pipefail

CLI_APP_NAME="ec2_manager"
GUI_APP_NAME="ec2_manager_gui"
APP_VERSION="1.1"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_DIR="${ROOT_DIR}/dist"
LINUX_DIST_DIR="${DIST_DIR}/linux"
WINDOWS_DIST_DIR="${DIST_DIR}/windows"
# HOST_TRIPLE is computed in main() AFTER ensure_rust(), since rustc may not be
# installed yet on a fresh WSL/machine.
HOST_TRIPLE=""

usage() {
  cat <<USAGE
Usage: $0 [native|all|linux|windows]

Build release binaries for Pop!_OS (linux) and Windows.

Output layout:
  dist/linux/
  dist/windows/

Modes:
  native   Build only host-native binaries
  all      Build linux + windows binaries (best on Linux host)
  linux    Build Linux binaries
  windows  Build Windows binaries
USAGE
}

require_cmd() {
  local cmd="$1"
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "error: required command not found: $cmd" >&2
    exit 1
  fi
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

copy_artifact() {
  local target="$1"
  local artifact_name="$2"

  local source_dir
  if [[ "$target" == "$HOST_TRIPLE" ]]; then
    source_dir="$ROOT_DIR/target/release"
  else
    source_dir="$ROOT_DIR/target/${target}/release"
  fi

  local out_dir
  out_dir="$(target_output_dir "$target")"
  mkdir -p "$out_dir"

  # Rename with version suffix, e.g. ec2_manager_gui.exe -> ec2_manager_gui_${APP_VERSION}.exe
  local dest_name
  if [[ "$artifact_name" == *.exe ]]; then
    local base="${artifact_name%.exe}"
    dest_name="${base}_${APP_VERSION}.exe"
  elif [[ "$artifact_name" == *.* ]]; then
    local base="${artifact_name%.*}"
    local ext="${artifact_name##*.}"
    dest_name="${base}_${APP_VERSION}.${ext}"
  else
    dest_name="${artifact_name}_${APP_VERSION}"
  fi

  cp "${source_dir}/${artifact_name}" "${out_dir}/${dest_name}"
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
  # Copy walkthrough into dist dir for packaging
  if [[ -f "${ROOT_DIR}/WALKTHROUGH.md" ]]; then
    cp "${ROOT_DIR}/WALKTHROUGH.md" "${WINDOWS_DIST_DIR}/WALKTHROUGH.md"
  fi
  local candidates=(
    "${WINDOWS_DIST_DIR}/${CLI_APP_NAME}_${APP_VERSION}.exe"
    "${WINDOWS_DIST_DIR}/${GUI_APP_NAME}_${APP_VERSION}.exe"
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
  # The WALKTHROUGH is in both (it's a zip candidate above).
  if [[ -f "${ROOT_DIR}/USER_README.md" ]]; then
    cp "${ROOT_DIR}/USER_README.md" "${extract_dir}/README.md"
  fi
  echo "info: packaged Windows zip: $zip_path"
  echo "info: extracted to: $extract_dir"
}

package_linux_zip() {
  local zip_path="${LINUX_DIST_DIR}/ec2_manager_linux.zip"
  local candidates=(
    "${LINUX_DIST_DIR}/${CLI_APP_NAME}"
    "${LINUX_DIST_DIR}/${GUI_APP_NAME}"
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
  local mode="${1:-all}"
  case "$mode" in
    -h|--help)
      usage
      exit 0
      ;;
  esac

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

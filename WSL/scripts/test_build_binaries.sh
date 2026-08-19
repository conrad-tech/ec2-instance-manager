#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

EC2_MANAGER_BUILD_LIB_ONLY=1 source "$ROOT_DIR/scripts/build_binaries.sh"

assert_eq() {
  local got="$1"
  local want="$2"
  local msg="$3"
  if [[ "$got" != "$want" ]]; then
    echo "assertion failed: $msg" >&2
    echo "  got:  $got" >&2
    echo "  want: $want" >&2
    exit 1
  fi
}

test_native_mode_uses_host_target_on_linux() {
  local got
  got="$(resolve_targets native Linux aarch64-unknown-linux-gnu)"
  assert_eq "$got" "aarch64-unknown-linux-gnu" "native mode should use host target on Linux"
}

test_windows_mode_target_by_host_os() {
  local got_linux got_darwin
  got_linux="$(resolve_targets windows Linux x86_64-unknown-linux-gnu)"
  got_darwin="$(resolve_targets windows Darwin aarch64-apple-darwin)"
  assert_eq "$got_linux" "x86_64-pc-windows-gnu" "Linux should use Windows GNU target"
  assert_eq "$got_darwin" "x86_64-pc-windows-msvc" "non-Linux should use Windows MSVC target"
}

test_all_mode_outputs_expected_targets() {
  local linux_targets darwin_targets
  linux_targets="$(resolve_targets all Linux x86_64-unknown-linux-gnu | tr '\n' ' ' | sed 's/ $//')"
  darwin_targets="$(resolve_targets all Darwin aarch64-apple-darwin)"
  assert_eq "$linux_targets" "x86_64-unknown-linux-gnu x86_64-pc-windows-gnu" "all mode on Linux should emit two targets"
  assert_eq "$darwin_targets" "aarch64-apple-darwin" "all mode on non-Linux should use host target"
}

test_invalid_mode_fails() {
  if resolve_targets nope Linux x86_64-unknown-linux-gnu >/dev/null 2>&1; then
    echo "assertion failed: invalid mode should fail" >&2
    exit 1
  fi
}

test_package_linux_zip_creates_archive_with_artifacts() {
  if ! command -v zip >/dev/null 2>&1 || ! command -v unzip >/dev/null 2>&1; then
    echo "skipping linux zip packaging test (zip/unzip not installed)"
    return 0
  fi

  local tmpdir
  tmpdir="$(mktemp -d)"
  local original_linux_dist_dir="$LINUX_DIST_DIR"
  LINUX_DIST_DIR="$tmpdir"

  # Same versioned names copy_artifact writes, e.g. ec2_manager_gui_1.1.
  touch "$LINUX_DIST_DIR/${CLI_APP_NAME}_${APP_VERSION}"
  touch "$LINUX_DIST_DIR/${GUI_APP_NAME}_${APP_VERSION}"

  package_linux_zip

  local zip_path="$LINUX_DIST_DIR/ec2_manager_linux_${APP_VERSION}.zip"
  if [[ ! -f "$zip_path" ]]; then
    echo "assertion failed: linux zip was not created" >&2
    exit 1
  fi

  local listing
  listing="$(unzip -Z1 "$zip_path" | tr '\n' ' ')"
  if [[ "$listing" != *"$CLI_APP_NAME"* || "$listing" != *"$GUI_APP_NAME"* ]]; then
    echo "assertion failed: linux zip missing expected artifacts" >&2
    echo "  listing: $listing" >&2
    exit 1
  fi

  rm -rf "$tmpdir"
  LINUX_DIST_DIR="$original_linux_dist_dir"
}

test_package_windows_zip_ships_both_powershell_scripts() {
  if ! command -v zip >/dev/null 2>&1 || ! command -v unzip >/dev/null 2>&1; then
    echo "skipping windows zip packaging test (zip/unzip not installed)"
    return 0
  fi

  local tmpdir
  tmpdir="$(mktemp -d)"
  local original_windows_dist_dir="$WINDOWS_DIST_DIR"
  WINDOWS_DIST_DIR="$tmpdir"

  # Same versioned names copy_artifact writes.
  touch "$WINDOWS_DIST_DIR/${CLI_APP_NAME}_${APP_VERSION}.exe"
  touch "$WINDOWS_DIST_DIR/${GUI_APP_NAME}_${APP_VERSION}.exe"

  # No SKIP_ICON_VERIFY: package_windows_zip does not call verify_windows_icon,
  # so setting it here only suggested a coupling that does not exist.
  package_windows_zip

  # Both scripts must land beside the exe rather than inside the archive
  # only: they are run from the file next to the executable, never written
  # to %TEMP% and run from there, because that is a pattern EDRs quarantine
  # on sight and this app has a CrowdStrike quarantine in its history.
  local missing=""
  [[ -f "$WINDOWS_DIST_DIR/send_access_email.ps1" ]] || missing="$missing send_access_email.ps1"
  [[ -f "$WINDOWS_DIST_DIR/send_escalation.ps1" ]] || missing="$missing send_escalation.ps1"
  if [[ -n "$missing" ]]; then
    echo "assertion failed: not copied beside the exe:$missing" >&2
    exit 1
  fi

  # ...and they must be IN the archive too, which is a separate fact. The
  # copies above happen before the `candidates` array is even read, so
  # dropping a script from that array leaves the dist dir correct and the
  # distributed zip -- the thing users actually receive -- missing it. The
  # check above alone passes on that. Assert the listing, and the extracted
  # tree package_windows_zip unpacks beside the zip.
  local zip_path="$WINDOWS_DIST_DIR/ec2_manager_windows_${APP_VERSION}.zip"
  if [[ ! -f "$zip_path" ]]; then
    echo "assertion failed: windows zip was not created" >&2
    exit 1
  fi

  local listing
  listing="$(unzip -Z1 "$zip_path" | tr '\n' ' ')"
  local extract_dir="$WINDOWS_DIST_DIR/ec2_manager_windows"
  for shipped in send_access_email.ps1 send_escalation.ps1 \
                 "${CLI_APP_NAME}_${APP_VERSION}.exe" \
                 "${GUI_APP_NAME}_${APP_VERSION}.exe"; do
    if [[ "$listing" != *"$shipped"* ]]; then
      echo "assertion failed: windows zip is missing $shipped" >&2
      echo "  listing: $listing" >&2
      exit 1
    fi
    if [[ ! -f "$extract_dir/$shipped" ]]; then
      echo "assertion failed: extracted folder is missing $shipped" >&2
      exit 1
    fi
  done

  rm -rf "$tmpdir"
  WINDOWS_DIST_DIR="$original_windows_dist_dir"
}

test_package_linux_zip_skips_when_no_artifacts() {
  local tmpdir
  tmpdir="$(mktemp -d)"
  local original_linux_dist_dir="$LINUX_DIST_DIR"
  LINUX_DIST_DIR="$tmpdir"

  package_linux_zip

  if [[ -f "$LINUX_DIST_DIR/ec2_manager_linux_${APP_VERSION}.zip" ]]; then
    echo "assertion failed: linux zip should not be created without artifacts" >&2
    exit 1
  fi

  rm -rf "$tmpdir"
  LINUX_DIST_DIR="$original_linux_dist_dir"
}

test_copy_windows_runtime_dlls_with_custom_gcc() {
  local tmpdir
  tmpdir="$(mktemp -d)"
  local original_windows_dist_dir="$WINDOWS_DIST_DIR"
  WINDOWS_DIST_DIR="$tmpdir"

  local bin_dir="$tmpdir/bin"
  mkdir -p "$bin_dir"

  local fake_gcc="$bin_dir/fake-gcc"
  cat >"$fake_gcc" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "$1" in
  -print-file-name=libgcc_s_seh-1.dll) echo "$TEST_DLL_DIR/libgcc_s_seh-1.dll" ;;
  -print-file-name=libstdc++-6.dll) echo "$TEST_DLL_DIR/libstdc++-6.dll" ;;
  -print-file-name=libwinpthread-1.dll) echo "$TEST_DLL_DIR/libwinpthread-1.dll" ;;
  *) echo "$1" ;;
esac
EOF
  chmod +x "$fake_gcc"

  local dll_dir="$tmpdir/dlls"
  mkdir -p "$dll_dir"
  touch "$dll_dir/libgcc_s_seh-1.dll"
  touch "$dll_dir/libstdc++-6.dll"
  touch "$dll_dir/libwinpthread-1.dll"

  TEST_DLL_DIR="$dll_dir" EC2_MANAGER_MINGW_GCC="$fake_gcc" copy_windows_runtime_dlls "x86_64-pc-windows-gnu"

  if [[ ! -f "$WINDOWS_DIST_DIR/libgcc_s_seh-1.dll" || ! -f "$WINDOWS_DIST_DIR/libstdc++-6.dll" || ! -f "$WINDOWS_DIST_DIR/libwinpthread-1.dll" ]]; then
    echo "assertion failed: windows runtime DLLs were not copied" >&2
    exit 1
  fi

  rm -rf "$tmpdir"
  WINDOWS_DIST_DIR="$original_windows_dist_dir"
}

# verify_windows_icon is handed the path copy_artifact wrote, so the two must
# agree on the versioned filename. They share versioned_name for that reason;
# this pins the shape it produces.
test_versioned_name_matches_the_exe_copy_artifact_writes() {
  assert_eq "$(versioned_name "ec2_manager_gui.exe")" \
    "ec2_manager_gui_${APP_VERSION}.exe" "exe should gain the version suffix before .exe"
  assert_eq "$(versioned_name "ec2_manager")" \
    "ec2_manager_${APP_VERSION}" "extensionless artifact should gain a version suffix"
}

# The whole point of the check: an exe with no icon resource must stop the
# release rather than ship a binary that shows the generic Windows glyph.
test_verify_windows_icon_rejects_an_exe_without_a_resource() {
  if ! command -v objdump >/dev/null 2>&1 \
     && ! command -v x86_64-w64-mingw32-objdump >/dev/null 2>&1 \
     && ! command -v llvm-objdump >/dev/null 2>&1; then
    echo "skipping icon verification tests (no objdump)"
    return 0
  fi

  local tmpdir
  tmpdir="$(mktemp -d)"
  local bogus="$tmpdir/ec2_manager_gui_${APP_VERSION}.exe"
  printf 'not a PE file' > "$bogus"

  if ( verify_windows_icon "$bogus" ) >/dev/null 2>&1; then
    echo "assertion failed: verify_windows_icon accepted an exe with no .rsrc" >&2
    rm -rf "$tmpdir"
    exit 1
  fi

  # ...but it must stay overridable, for a build host with no objdump.
  if ! ( SKIP_ICON_VERIFY=1 verify_windows_icon "$bogus" ) >/dev/null 2>&1; then
    echo "assertion failed: SKIP_ICON_VERIFY=1 should bypass the check" >&2
    rm -rf "$tmpdir"
    exit 1
  fi

  rm -rf "$tmpdir"
}

# And the positive case, against a real cross-compiled exe when one is around.
# Skipped rather than failed when it is not: a clean checkout has no artifacts.
test_verify_windows_icon_accepts_a_real_build() {
  local exe="$ROOT_DIR/dist/windows/ec2_manager_gui_${APP_VERSION}.exe"
  if [[ ! -f "$exe" ]]; then
    echo "skipping icon verification positive test (no built exe at $exe)"
    return 0
  fi
  if ! ( verify_windows_icon "$exe" ) >/dev/null 2>&1; then
    echo "note: $exe has no embedded icon — expected for a pre-4fb70d9 build" >&2
  fi
}

main() {
  test_native_mode_uses_host_target_on_linux
  test_windows_mode_target_by_host_os
  test_all_mode_outputs_expected_targets
  test_invalid_mode_fails
  test_package_linux_zip_creates_archive_with_artifacts
  test_package_windows_zip_ships_both_powershell_scripts
  test_package_linux_zip_skips_when_no_artifacts
  test_copy_windows_runtime_dlls_with_custom_gcc
  test_versioned_name_matches_the_exe_copy_artifact_writes
  test_verify_windows_icon_rejects_an_exe_without_a_resource
  test_verify_windows_icon_accepts_a_real_build
  echo "build_binaries tests passed"
}

main "$@"

#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
augmenter="$repo_root/scripts/augment-installers.sh"

if [[ ! -f "$augmenter" ]]; then
  echo "missing installer augmenter: $augmenter" >&2
  exit 1
fi

temp_dir="$(mktemp -d)"
trap 'rm -rf "$temp_dir"' EXIT
install_log="$temp_dir/install.log"
setup_log="$temp_dir/setup.log"

fail() {
  echo "release_installer_augmentation: $*" >&2
  exit 1
}

# Writes fake cargo-dist installers and data assets into a fresh directory.
make_artifacts() {
  local dir="$1"
  mkdir -p "$dir"
  cat > "$dir/x86mcp-installer.sh" <<'INSTALLER'
#!/bin/sh
set -eu
printf '%s\n' installed > "$INSTALL_TEST_LOG"
INSTALLER
  cat > "$dir/x86mcp-installer.ps1" <<'INSTALLER'
Write-Output "installed"
INSTALLER
  : > "$dir/x86mcp-data-0.1.0.tar.zst"
  : > "$dir/x86mcp-data-0.1.0.tar.zst.sha256"
}

# Installs a fake binary at the given path that records how it was invoked.
make_binary() {
  mkdir -p "$(dirname "$1")"
  cat > "$1" <<'BINARY'
#!/bin/sh
printf '%s %s\n' "$0" "$*" >> "$X86MCP_TEST_LOG"
BINARY
  chmod +x "$1"
}

# Runs a command with every install override cleared, a fake HOME and
# CARGO_HOME, and then the given VAR=value assignments (an empty value unsets).
run_clean() {
  env -u X86MCP_INSTALL_DIR -u CARGO_DIST_FORCE_INSTALL_DIR -u X86MCP_UNMANAGED_INSTALL \
    -u X86MCP_SKIP_SETUP -u X86MCP_DATA_URL -u INFERRED_HOME \
    HOME="$temp_dir/home" \
    CARGO_HOME="$temp_dir/cargo-home" \
    INSTALL_TEST_LOG="$install_log" \
    X86MCP_TEST_LOG="$setup_log" \
    "$@"
}

# Release tags are preserved exactly; the version is parsed out of them.
for tag in 0.1.0 v0.1.0 x86mcp-v0.1.0 x86mcp/0.1.0 releases/v0.1.0; do
  artifacts="$temp_dir/artifacts-${tag//\//_}"
  make_artifacts "$artifacts"
  bash "$augmenter" "$artifacts" "$tag"
  expected_url="https://github.com/sa413x/x86mcp/releases/download/$tag/x86mcp-data-0.1.0.tar.zst"
  for installer in "$artifacts/x86mcp-installer.sh" "$artifacts/x86mcp-installer.ps1"; do
    grep -Fq "x86mcp managed data bootstrap" "$installer" || fail "$tag: bootstrap missing in $installer"
    grep -Fq "$expected_url" "$installer" || fail "$tag: $expected_url missing in $installer"
  done
done

artifacts="$temp_dir/artifacts-v0.1.0"
installer="$artifacts/x86mcp-installer.sh"
ps_installer="$artifacts/x86mcp-installer.ps1"
expected_url="https://github.com/sa413x/x86mcp/releases/download/v0.1.0/x86mcp-data-0.1.0.tar.zst"

# Augmenting twice must not append a second bootstrap.
bash "$augmenter" "$artifacts" v0.1.0
for file in "$installer" "$ps_installer"; do
  test "$(grep -c "x86mcp managed data bootstrap" "$file")" = 1 || fail "bootstrap appended twice to $file"
done

# Tags without a package version and missing data assets are rejected.
rejected="$temp_dir/artifacts-rejected"
make_artifacts "$rejected"
if bash "$augmenter" "$rejected" latest 2>/dev/null; then
  fail "tag without a version was accepted"
fi
rm "$rejected/x86mcp-data-0.1.0.tar.zst.sha256"
if bash "$augmenter" "$rejected" v0.1.0 2>/dev/null; then
  fail "release without the data checksum asset was accepted"
fi
if grep -Fq "x86mcp managed data bootstrap" "$rejected/x86mcp-installer.sh"; then
  fail "rejected release still modified the installer"
fi

# The PowerShell bootstrap is checked when pwsh is available: the setup call is
# replaced by printing the resolved binary path.
ps_probe="$temp_dir/probe.ps1"
if command -v pwsh >/dev/null 2>&1; then
  have_pwsh=1
  probe_call="  Write-Output \"resolved:\$x86mcpBin|\$x86mcpDataUrl\"; \$global:LASTEXITCODE = 0"
  while IFS= read -r line; do
    if [[ "$line" == '  & $x86mcpBin setup --data-url $x86mcpDataUrl' ]]; then
      printf '%s\n' "$probe_call"
    else
      printf '%s\n' "$line"
    fi
  done < "$ps_installer" > "$ps_probe"
  grep -Fq "resolved:" "$ps_probe" || fail "PowerShell setup call not found"
else
  have_pwsh=0
  echo "pwsh not found; skipping PowerShell bootstrap checks" >&2
fi

# Each case: description, install prefix, layout (cargo-home|flat), then
# environment assignments. Mirrors cargo-dist 0.32's install-dir precedence.
check_install_dir() {
  local description="$1" prefix="$2" layout="$3"
  shift 3
  local sh_bin ps_bin
  if [[ "$layout" == cargo-home ]]; then
    sh_bin="$prefix/bin/x86mcp"
    # pwsh on Linux/macOS normalizes the bootstrap's '\' separators to '/'.
    ps_bin="$prefix/bin/x86mcp.exe"
  else
    sh_bin="$prefix/x86mcp"
    ps_bin="$prefix/x86mcp.exe"
  fi

  rm -rf "$temp_dir/home" "$temp_dir/cargo-home" "$temp_dir/prefix-"*
  make_binary "$sh_bin"
  : > "$install_log"
  : > "$setup_log"
  run_clean "$@" sh "$installer"
  test "$(cat "$install_log")" = "installed" || fail "$description: installer did not run"
  test "$(cat "$setup_log")" = "$sh_bin setup --data-url $expected_url" \
    || fail "$description: unexpected shell setup call: $(cat "$setup_log")"

  if [[ "$have_pwsh" == 1 ]]; then
    local resolved
    resolved="$(run_clean "$@" pwsh -NoProfile -NonInteractive -File "$ps_probe" | grep '^resolved:')"
    test "$resolved" = "resolved:$ps_bin|$expected_url" \
      || fail "$description: unexpected PowerShell resolution: $resolved"
  fi
}

check_install_dir "CARGO_HOME" "$temp_dir/cargo-home" cargo-home
check_install_dir "HOME fallback" "$temp_dir/home/.cargo" cargo-home CARGO_HOME=
check_install_dir "X86MCP_INSTALL_DIR" "$temp_dir/prefix-app" cargo-home \
  X86MCP_INSTALL_DIR="$temp_dir/prefix-app" \
  CARGO_DIST_FORCE_INSTALL_DIR="$temp_dir/prefix-force" \
  X86MCP_UNMANAGED_INSTALL="$temp_dir/prefix-unmanaged"
check_install_dir "CARGO_DIST_FORCE_INSTALL_DIR" "$temp_dir/prefix-force" cargo-home \
  CARGO_DIST_FORCE_INSTALL_DIR="$temp_dir/prefix-force" \
  X86MCP_UNMANAGED_INSTALL="$temp_dir/prefix-unmanaged"
check_install_dir "X86MCP_UNMANAGED_INSTALL" "$temp_dir/prefix-unmanaged" flat \
  X86MCP_UNMANAGED_INSTALL="$temp_dir/prefix-unmanaged"
check_install_dir "X86MCP_UNMANAGED_INSTALL at CARGO_HOME" "$temp_dir/cargo-home" cargo-home \
  X86MCP_UNMANAGED_INSTALL="$temp_dir/cargo-home"
check_install_dir "X86MCP_UNMANAGED_INSTALL at the default Cargo home" "$temp_dir/home/.cargo" cargo-home \
  CARGO_HOME= X86MCP_UNMANAGED_INSTALL="$temp_dir/home/.cargo"

# X86MCP_DATA_URL overrides the release data URL.
rm -rf "$temp_dir/cargo-home"
make_binary "$temp_dir/cargo-home/bin/x86mcp"
: > "$setup_log"
run_clean X86MCP_DATA_URL=/srv/x86mcp-data.tar.zst sh "$installer"
test "$(cat "$setup_log")" = "$temp_dir/cargo-home/bin/x86mcp setup --data-url /srv/x86mcp-data.tar.zst" \
  || fail "X86MCP_DATA_URL was ignored: $(cat "$setup_log")"

# X86MCP_SKIP_SETUP=1 performs a binary-only install.
: > "$setup_log"
run_clean X86MCP_SKIP_SETUP=1 sh "$installer"
test ! -s "$setup_log" || fail "X86MCP_SKIP_SETUP=1 still ran setup"

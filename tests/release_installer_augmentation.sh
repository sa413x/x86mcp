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

artifacts="$temp_dir/artifacts"
install_prefix="$temp_dir/install"
install_dir="$install_prefix/bin"
mkdir -p "$artifacts" "$install_dir"

cat > "$artifacts/x86mcp-installer.sh" <<'INSTALLER'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' installed > "$INSTALL_TEST_LOG"
INSTALLER

cat > "$artifacts/x86mcp-installer.ps1" <<'INSTALLER'
Write-Output "installed"
INSTALLER

cat > "$install_dir/x86mcp" <<'BINARY'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$X86MCP_TEST_LOG"
BINARY
chmod +x "$artifacts/x86mcp-installer.sh" "$install_dir/x86mcp"

bash "$augmenter" "$artifacts" 0.1.0

expected_url="https://github.com/sa413x/x86mcp/releases/download/v0.1.0/x86mcp-data-0.1.0.tar.zst"
grep -Fq "x86mcp managed data bootstrap" "$artifacts/x86mcp-installer.sh"
grep -Fq "x86mcp managed data bootstrap" "$artifacts/x86mcp-installer.ps1"
grep -Fq "$expected_url" "$artifacts/x86mcp-installer.sh"
grep -Fq "$expected_url" "$artifacts/x86mcp-installer.ps1"

install_log="$temp_dir/install.log"
setup_log="$temp_dir/setup.log"
INSTALL_TEST_LOG="$install_log" \
X86MCP_TEST_LOG="$setup_log" \
X86MCP_INSTALL_DIR="$install_prefix" \
bash "$artifacts/x86mcp-installer.sh"

test "$(cat "$install_log")" = "installed"
test "$(cat "$setup_log")" = "setup --data-url $expected_url"

: > "$setup_log"
INSTALL_TEST_LOG="$install_log" \
X86MCP_TEST_LOG="$setup_log" \
X86MCP_INSTALL_DIR="$install_prefix" \
X86MCP_SKIP_SETUP=1 \
bash "$artifacts/x86mcp-installer.sh"
test ! -s "$setup_log"

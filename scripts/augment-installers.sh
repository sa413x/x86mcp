#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <artifact-directory> <release-tag-or-version>" >&2
  exit 2
fi

artifact_dir="$1"
release="$2"
repository="${GITHUB_REPOSITORY:-sa413x/x86mcp}"

if [[ "$release" == v* ]]; then
  tag="$release"
  version="${release#v}"
else
  tag="v$release"
  version="$release"
fi

if [[ ! "$tag" =~ ^v[0-9A-Za-z._+-]+$ ]]; then
  echo "invalid release tag: $tag" >&2
  exit 2
fi
if [[ ! "$repository" =~ ^[0-9A-Za-z._-]+/[0-9A-Za-z._-]+$ ]]; then
  echo "invalid GitHub repository: $repository" >&2
  exit 2
fi

shell_installer="$artifact_dir/x86mcp-installer.sh"
powershell_installer="$artifact_dir/x86mcp-installer.ps1"
for installer in "$shell_installer" "$powershell_installer"; do
  if [[ ! -f "$installer" ]]; then
    echo "missing generated installer: $installer" >&2
    exit 1
  fi
done

data_url="https://github.com/$repository/releases/download/$tag/x86mcp-data-$version.tar.zst"
marker="x86mcp managed data bootstrap"

if ! grep -Fq "$marker" "$shell_installer"; then
  cat >> "$shell_installer" <<EOF

# $marker
if [ "\${X86MCP_SKIP_SETUP:-0}" != "1" ]; then
    if [ -n "\${X86MCP_INSTALL_DIR:-}" ]; then
        x86mcp_bin="\${X86MCP_INSTALL_DIR}/bin/x86mcp"
    else
        x86mcp_bin="\${CARGO_HOME:-\${HOME}/.cargo}/bin/x86mcp"
    fi
    x86mcp_data_url="\${X86MCP_DATA_URL:-$data_url}"
    printf '%s\n' "installing the x86 architecture data bundle"
    "\$x86mcp_bin" setup --data-url "\$x86mcp_data_url"
fi
EOF
fi

if ! grep -Fq "$marker" "$powershell_installer"; then
  cat >> "$powershell_installer" <<EOF

# $marker
if (\$env:X86MCP_SKIP_SETUP -ne "1") {
  if (\$env:X86MCP_INSTALL_DIR) {
    \$x86mcpBin = Join-Path \$env:X86MCP_INSTALL_DIR "bin\\x86mcp.exe"
  } elseif (\$env:CARGO_HOME) {
    \$x86mcpBin = Join-Path \$env:CARGO_HOME "bin\\x86mcp.exe"
  } else {
    \$x86mcpBin = Join-Path \$HOME ".cargo\\bin\\x86mcp.exe"
  }
  \$x86mcpDataUrl = if (\$env:X86MCP_DATA_URL) { \$env:X86MCP_DATA_URL } else { "$data_url" }
  Write-Information "installing the x86 architecture data bundle"
  & \$x86mcpBin setup --data-url \$x86mcpDataUrl
  if (\$LASTEXITCODE -ne 0) {
    throw "x86mcp data setup failed with exit code \$LASTEXITCODE"
  }
}
EOF
fi

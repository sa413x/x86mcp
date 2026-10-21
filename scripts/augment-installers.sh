#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <artifact-directory> <release-tag>" >&2
  exit 2
fi

artifact_dir="$1"
tag="$2"
repository="${GITHUB_REPOSITORY:-sa413x/x86mcp}"

# The release URL uses the exact tag CI publishes. The package version is
# parsed out of it the way cargo-dist (axotag) does: drop everything up to the
# last '/', then an optional "x86mcp-" package prefix, then an optional 'v'.
version="${tag##*/}"
version="${version#x86mcp-}"
version="${version#v}"

if [[ ! "$tag" =~ ^[0-9A-Za-z._+/-]+$ ]]; then
  echo "invalid release tag: $tag" >&2
  exit 2
fi
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([+-][0-9A-Za-z.-]+)?$ ]]; then
  echo "release tag $tag does not contain a package version" >&2
  exit 2
fi
if [[ ! "$repository" =~ ^[0-9A-Za-z._-]+/[0-9A-Za-z._-]+$ ]]; then
  echo "invalid GitHub repository: $repository" >&2
  exit 2
fi

shell_installer="$artifact_dir/x86mcp-installer.sh"
powershell_installer="$artifact_dir/x86mcp-installer.ps1"
data_asset="x86mcp-data-$version.tar.zst"
for required in "$shell_installer" "$powershell_installer" "$artifact_dir/$data_asset" "$artifact_dir/$data_asset.sha256"; do
  if [[ ! -f "$required" ]]; then
    echo "missing release artifact: $required" >&2
    exit 1
  fi
done

data_url="https://github.com/$repository/releases/download/$tag/$data_asset"
marker="x86mcp managed data bootstrap"

# Both bootstraps resolve the installed binary with cargo-dist 0.32's
# install-dir precedence for `install-path = "CARGO_HOME"`:
# X86MCP_INSTALL_DIR, then CARGO_DIST_FORCE_INSTALL_DIR (both <dir>/bin), then
# X86MCP_UNMANAGED_INSTALL (flat <dir>, unless it is the Cargo home), then
# $CARGO_HOME/bin, then $HOME/.cargo/bin.
if ! grep -Fq "$marker" "$shell_installer"; then
  cat >> "$shell_installer" <<EOF

# $marker
if [ "\${X86MCP_SKIP_SETUP:-0}" != "1" ]; then
    x86mcp_cargo_home="\${CARGO_HOME:-\${INFERRED_HOME:-\${HOME:-}}/.cargo}"
    if [ -n "\${X86MCP_INSTALL_DIR:-}" ]; then
        x86mcp_bin="\${X86MCP_INSTALL_DIR}/bin/x86mcp"
    elif [ -n "\${CARGO_DIST_FORCE_INSTALL_DIR:-}" ]; then
        x86mcp_bin="\${CARGO_DIST_FORCE_INSTALL_DIR}/bin/x86mcp"
    elif [ -n "\${X86MCP_UNMANAGED_INSTALL:-}" ]; then
        if [ "\${X86MCP_UNMANAGED_INSTALL}" = "\$x86mcp_cargo_home" ]; then
            x86mcp_bin="\${X86MCP_UNMANAGED_INSTALL}/bin/x86mcp"
        else
            x86mcp_bin="\${X86MCP_UNMANAGED_INSTALL}/x86mcp"
        fi
    else
        x86mcp_bin="\$x86mcp_cargo_home/bin/x86mcp"
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
  \$x86mcpCargoHome = if (\$env:CARGO_HOME) { \$env:CARGO_HOME } else {
    Join-Path \$(if (\$HOME) { \$HOME } else { "." }) ".cargo"
  }
  if (\$env:X86MCP_INSTALL_DIR) {
    \$x86mcpBin = Join-Path \$env:X86MCP_INSTALL_DIR "bin\\x86mcp.exe"
  } elseif (\$env:CARGO_DIST_FORCE_INSTALL_DIR) {
    \$x86mcpBin = Join-Path \$env:CARGO_DIST_FORCE_INSTALL_DIR "bin\\x86mcp.exe"
  } elseif (\$env:X86MCP_UNMANAGED_INSTALL) {
    if (\$env:X86MCP_UNMANAGED_INSTALL.Replace('\\\\', '\\') -eq \$x86mcpCargoHome) {
      \$x86mcpBin = Join-Path \$env:X86MCP_UNMANAGED_INSTALL "bin\\x86mcp.exe"
    } else {
      \$x86mcpBin = Join-Path \$env:X86MCP_UNMANAGED_INSTALL "x86mcp.exe"
    }
  } else {
    \$x86mcpBin = Join-Path \$x86mcpCargoHome "bin\\x86mcp.exe"
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

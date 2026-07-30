#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
  echo "usage: $0 OUTPUT_DIR [VERSION]" >&2
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
output_dir="$1"
mkdir -p "$output_dir"
output_dir="$(cd "$output_dir" && pwd)"
version="${2:-$(cargo metadata --format-version 1 --no-deps --manifest-path "$repo_root/Cargo.toml" | jq -r '.packages[] | select(.name == "x86mcp") | .version')}"

if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([+-][0-9A-Za-z.-]+)?$ ]]; then
  echo "invalid package version: $version" >&2
  exit 2
fi

snapshot_id="$(tr -d '\r\n' < "$repo_root/index/CURRENT")"
if [[ ! "$snapshot_id" =~ ^[0-9a-f]{64}$ ]]; then
  echo "invalid snapshot ID in index/CURRENT" >&2
  exit 2
fi

snapshot_dir="$repo_root/index/snapshots/$snapshot_id"
if [[ ! -f "$snapshot_dir/snapshot.json" ]]; then
  echo "current snapshot is missing: $snapshot_dir" >&2
  exit 2
fi

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/index/snapshots"
cp -a "$repo_root/corpus" "$stage/corpus"
cp "$repo_root/index/CURRENT" "$stage/index/CURRENT"
cp -a "$snapshot_dir" "$stage/index/snapshots/$snapshot_id"
printf '{\n  "schema_version": 1,\n  "package_version": "%s",\n  "snapshot_id": "%s"\n}\n' \
  "$version" "$snapshot_id" > "$stage/x86mcp-data.json"

archive="$output_dir/x86mcp-data-$version.tar.zst"
epoch="${SOURCE_DATE_EPOCH:-$(git -C "$repo_root" log -1 --format=%ct 2>/dev/null || printf '0')}"
if command -v zstd >/dev/null 2>&1 && [[ "$(tar --help)" == *"--sort"* ]]; then
  tar \
    --sort=name \
    --mtime="@$epoch" \
    --owner=0 \
    --group=0 \
    --numeric-owner \
    -C "$stage" \
    -cf - \
    corpus index x86mcp-data.json \
    | zstd -19 -T0 -q -o "$archive"
else
  tar \
    --zstd \
    --format ustar \
    --mtime="@$epoch" \
    -C "$stage" \
    -cf "$archive" \
    corpus index x86mcp-data.json
fi

checksum="$(sha256sum "$archive" | cut -d ' ' -f 1)"
printf '%s  %s\n' "$checksum" "$(basename "$archive")" > "$archive.sha256"
printf '%s\n' "$archive"
printf '%s\n' "$archive.sha256"

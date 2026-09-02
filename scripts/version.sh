#!/usr/bin/env bash
# Read and validate release versions from Cargo.toml, the sole version source.
set -euo pipefail

DIR="$(cd "$(dirname "$0")/.." && pwd)"
# read the manifest directly: cargo is a developer tool, but install-bin.sh and
# update.sh need the version at runtime on machines that have never seen Rust
version="$(awk -F'"' '/^version[[:space:]]*=/ { print $2; exit }' "$DIR/Cargo.toml" 2>/dev/null)"
if [ -z "$version" ]; then
  printf 'agenmux: could not read package version from Cargo.toml\n' >&2
  exit 1
fi
tag="v$version"

case "${1:-version}" in
version) printf '%s\n' "$version" ;;
tag) printf '%s\n' "$tag" ;;
check-tag)
  actual="${2:-${GITHUB_REF_NAME:-}}"
  if [ "$actual" != "$tag" ]; then
    printf 'agenmux: release tag %s does not match Cargo.toml version %s (expected %s)\n' \
      "${actual:-<empty>}" "$version" "$tag" >&2
    exit 1
  fi
  ;;
*)
  printf 'usage: %s [version|tag|check-tag <tag>]\n' "$0" >&2
  exit 2
  ;;
esac

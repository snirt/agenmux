#!/usr/bin/env bash
# Read and validate release versions from Cargo.toml, the sole version source.
set -euo pipefail

DIR="$(cd "$(dirname "$0")/.." && pwd)"
package_id="$(cargo pkgid --manifest-path "$DIR/Cargo.toml")"
version="${package_id##*@}"
if [ "$version" = "$package_id" ] || [ -z "$version" ]; then
  printf 'agents-mon: could not read package version from Cargo.toml\n' >&2
  exit 1
fi
tag="v$version"

case "${1:-version}" in
  version) printf '%s\n' "$version" ;;
  tag) printf '%s\n' "$tag" ;;
  check-tag)
    actual="${2:-${GITHUB_REF_NAME:-}}"
    if [ "$actual" != "$tag" ]; then
      printf 'agents-mon: release tag %s does not match Cargo.toml version %s (expected %s)\n' \
        "${actual:-<empty>}" "$version" "$tag" >&2
      exit 1
    fi
    ;;
  *)
    printf 'usage: %s [version|tag|check-tag <tag>]\n' "$0" >&2
    exit 2
    ;;
esac

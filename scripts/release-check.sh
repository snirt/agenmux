#!/usr/bin/env bash
# Shared release preflight for local preparation and CI.
set -euo pipefail

cd "$(dirname "$0")/.."
mode="${1:-}"
version="$(bash scripts/version.sh)"
fail() {
  if [ "${GITHUB_ACTIONS:-}" = true ]; then
    echo "::error::$1" >&2
  else
    echo "$1" >&2
  fi
  exit 1
}

if [ "$mode" = prepare ]; then
  version="${2:?usage: release-check.sh prepare <next-version>}"
elif [ "$mode" = pr ]; then
  base="${2:?usage: release-check.sh pr <base-ref>}"
  base_version="$(git show "$base:Cargo.toml" | awk -F'"' '/^version[[:space:]]*=/ { print $2; exit }')"
  [ "$version" != "$base_version" ] || exit 0
elif [ "$mode" = master ]; then
  if git show-ref --verify --quiet "refs/tags/v$version"; then
    echo "v$version is already tagged; no release to prepare"
    exit 0
  fi
elif [ "$mode" = tag ]; then
  bash scripts/version.sh check-tag "${2:?usage: release-check.sh tag <tag>}"
else
  echo 'usage: release-check.sh {prepare <next-version>|pr <base-ref>|master|tag <tag>}' >&2
  exit 2
fi

if [[ ! "$version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
  fail "invalid Cargo version '$version'; use numeric major.minor.patch (for example 0.7.0)"
fi
if [ "$mode" != tag ] && git show-ref --verify --quiet "refs/tags/v$version"; then
  fail "v$version already exists; choose an unused version"
fi

previous=''
while IFS= read -r candidate; do
  [ "$mode" = tag ] && [ "$candidate" = "v$version" ] && continue
  previous="$candidate"
  break
done < <(git tag --list 'v*' --sort=-version:refname)

if [ -n "$previous" ]; then
  old="${previous#v}"
  if [ "$(printf '%s\n%s\n' "$old" "$version" | sort -V | tail -n 1)" != "$version" ] ||
    [ "$old" = "$version" ]; then
    fail "v$version must be newer than the previous release $previous; update Cargo.toml"
  fi
fi

if [ ! -s RELEASE_NOTES.md ] ||
  { [ -n "$previous" ] && git diff --quiet "$previous" -- RELEASE_NOTES.md; }; then
  fail "RELEASE_NOTES.md is empty or unchanged since ${previous:-the previous release}; update it before preparing v$version"
fi

if [ "$mode" != prepare ]; then
  lock_version="$(awk '/^\[\[package\]\]$/ { package=0 } /^name = "agenmux"$/ { package=1; next } package && /^version = / { gsub(/"/, "", $3); print $3; exit }' Cargo.lock)"
  if [ "$lock_version" != "$version" ]; then
    fail "Cargo.lock has agenmux $lock_version, but Cargo.toml has $version; run cargo metadata to refresh the lockfile"
  fi
  cargo metadata --locked --no-deps --format-version 1 >/dev/null || {
    fail 'Cargo.lock is stale; run cargo metadata to refresh it'
  }
fi

echo "release readiness passed for v$version"

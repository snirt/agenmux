#!/usr/bin/env bash
# Prepare a patch or minor release without creating a commit or tag.
set -euo pipefail

cd "$(dirname "$0")/.."
kind="${1:-patch}"
[ "$kind" = patch ] || [ "$kind" = minor ] || {
  echo 'usage: bump.sh [patch|minor]' >&2
  exit 2
}

old="$(bash scripts/version.sh)"
if [[ ! "$old" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
  echo "invalid Cargo version '$old'; use numeric major.minor.patch (for example 0.6.1)" >&2
  exit 1
fi
if ! git diff --quiet HEAD -- Cargo.toml Cargo.lock; then
  echo 'Cargo.toml or Cargo.lock has uncommitted changes; review them before preparing a release' >&2
  exit 1
fi

if [ "$kind" = patch ]; then
  new="${BASH_REMATCH[1]}.${BASH_REMATCH[2]}.$((10#${BASH_REMATCH[3]} + 1))"
else
  new="${BASH_REMATCH[1]}.$((10#${BASH_REMATCH[2]} + 1)).0"
fi
bash scripts/release-check.sh prepare "$new"

sed -i.bak "s/^version = \"$old\"/version = \"$new\"/" Cargo.toml
rm Cargo.toml.bak
cargo metadata --format-version 1 >/dev/null
cargo metadata --locked --no-deps --format-version 1 >/dev/null
echo "prepared v$new; review git diff, then commit the version files and release notes in a PR"

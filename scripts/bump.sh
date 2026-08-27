#!/usr/bin/env bash
# Patch-bump Cargo.toml, verify, commit, and tag locally.
set -euo pipefail

DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$DIR"
old="$(bash scripts/version.sh)"
new="${old%.*}.$((${old##*.} + 1))"
sed -i.bak "s/^version = \"$old\"/version = \"$new\"/" Cargo.toml
rm Cargo.toml.bak
cargo test
./tests/run.sh
git add Cargo.toml Cargo.lock
git commit -m "chore: bump version to $new"
git tag "v$new"
echo "bumped $old -> $new, tagged v$new (not pushed)"

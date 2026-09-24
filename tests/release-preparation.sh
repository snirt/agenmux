#!/usr/bin/env bash
set -euo pipefail

DIR="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

fixture() {
  work="$tmp/$1"
  mkdir -p "$work/scripts" "$work/src"
  cp "$DIR/Makefile" "$work/Makefile"
  cp "$DIR/scripts/bump.sh" "$DIR/scripts/release-check.sh" \
    "$DIR/scripts/version.sh" "$work/scripts/"
  chmod +x "$work/scripts/"*.sh
  printf '[package]\nname = "agenmux"\nversion = "0.6.1"\nedition = "2021"\n' >"$work/Cargo.toml"
  printf '' >"$work/src/lib.rs"
  printf 'notes for 0.6.1\n' >"$work/RELEASE_NOTES.md"
  cargo generate-lockfile --manifest-path "$work/Cargo.toml" >/dev/null
  git -C "$work" init -q -b master
  git -C "$work" config user.name test
  git -C "$work" config user.email test@example.com
  git -C "$work" add .
  git -C "$work" commit -qm initial
  git -C "$work" tag v0.6.1
}

fail_with() {
  expected="$1"
  shift
  if "$@" >"$tmp/out" 2>&1; then
    echo "FAIL release-preparation: unexpectedly passed: $*"
    exit 1
  fi
  grep -Fq "$expected" "$tmp/out" || {
    cat "$tmp/out"
    echo "FAIL release-preparation: missing error $expected"
    exit 1
  }
}

fixture patch
fail_with 'RELEASE_NOTES.md is empty or unchanged' make -s -C "$work" patch-bump
[ "$(bash "$work/scripts/version.sh")" = 0.6.1 ]
[ -z "$(git -C "$work" status --porcelain)" ]
: >"$work/RELEASE_NOTES.md"
fail_with 'RELEASE_NOTES.md is empty or unchanged' make -s -C "$work" patch-bump
[ "$(bash "$work/scripts/version.sh")" = 0.6.1 ]
printf 'notes for 0.6.2\n' >"$work/RELEASE_NOTES.md"
make -s -C "$work" bump >"$tmp/out"
[ "$(bash "$work/scripts/version.sh")" = 0.6.2 ]
grep -Fq 'version = "0.6.2"' "$work/Cargo.lock"
cargo metadata --manifest-path "$work/Cargo.toml" --locked --no-deps --format-version 1 >/dev/null
[ "$(git -C "$work" rev-list --count HEAD)" = 1 ]
[ "$(git -C "$work" tag --list 'v*' | wc -l | tr -d ' ')" = 1 ]

fixture carry
sed -i.bak 's/0.6.1/0.6.19/g' "$work/Cargo.toml" "$work/RELEASE_NOTES.md"
rm "$work/Cargo.toml.bak" "$work/RELEASE_NOTES.md.bak"
cargo metadata --manifest-path "$work/Cargo.toml" --format-version 1 >/dev/null
git -C "$work" add .
git -C "$work" commit -qm 'prepare fixture'
git -C "$work" tag v0.6.19
printf 'notes for 0.6.20\n' >"$work/RELEASE_NOTES.md"
make -s -C "$work" patch-bump >"$tmp/out"
[ "$(bash "$work/scripts/version.sh")" = 0.6.20 ]

fixture minor
printf 'notes for 0.7.0\n' >"$work/RELEASE_NOTES.md"
make -s -C "$work" minor-bump >"$tmp/out"
[ "$(bash "$work/scripts/version.sh")" = 0.7.0 ]
grep -Fq 'version = "0.7.0"' "$work/Cargo.lock"

fixture invalid
sed -i.bak 's/0.6.1/invalid/' "$work/Cargo.toml"
rm "$work/Cargo.toml.bak"
fail_with 'invalid Cargo version' make -s -C "$work" patch-bump
[ "$(git -C "$work" tag --list 'v*' | wc -l | tr -d ' ')" = 1 ]

fixture reused
git -C "$work" tag v0.6.2
fail_with 'v0.6.2 already exists' bash "$work/scripts/release-check.sh" prepare 0.6.2

fixture checks
git -C "$work" branch base
bash "$work/scripts/release-check.sh" pr base >"$tmp/out"
sed -i.bak 's/0.6.1/0.6.2/' "$work/Cargo.toml"
rm "$work/Cargo.toml.bak"
fail_with 'RELEASE_NOTES.md is empty or unchanged' bash "$work/scripts/release-check.sh" pr base
printf 'notes for 0.6.2\n' >"$work/RELEASE_NOTES.md"
fail_with 'Cargo.lock has agenmux' bash "$work/scripts/release-check.sh" pr base
cargo metadata --manifest-path "$work/Cargo.toml" --format-version 1 >/dev/null
bash "$work/scripts/release-check.sh" pr base >"$tmp/out"
bash "$work/scripts/release-check.sh" master >"$tmp/out"
fail_with 'does not match Cargo.toml' bash "$work/scripts/release-check.sh" tag v0.6.3
git -C "$work" add .
git -C "$work" commit -qm 'prepare release'
git -C "$work" tag v0.6.2
bash "$work/scripts/release-check.sh" tag v0.6.2 >"$tmp/out"
printf 'notes for 0.6.1\n' >"$work/RELEASE_NOTES.md"
fail_with 'RELEASE_NOTES.md is empty or unchanged' bash "$work/scripts/release-check.sh" tag v0.6.2

echo 'ok   release-preparation-and-readiness'

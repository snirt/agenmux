#!/usr/bin/env bash
set -euo pipefail

DIR="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
origin="$tmp/origin.git"
work="$tmp/work"

mkdir -p "$work/scripts"
git init -q --bare --initial-branch=master "$origin"
git -C "$work" init -q --initial-branch=master
git -C "$work" config user.name test
git -C "$work" config user.email test@example.com
git -C "$work" remote add origin "$origin"
cp "$DIR/Makefile" "$work/Makefile"
cp "$DIR/scripts/version.sh" "$DIR/scripts/release.sh" "$work/scripts/"
chmod +x "$work/scripts/version.sh" "$work/scripts/release.sh"
printf '[package]\nname = "agents-mon"\nversion = "0.2.0"\n' >"$work/Cargo.toml"
printf '0.2.0 notes\n' >"$work/RELEASE_NOTES.md"
git -C "$work" add Makefile Cargo.toml RELEASE_NOTES.md scripts/version.sh scripts/release.sh
git -C "$work" commit -qm initial
git -C "$work" push -q -u origin master

git -C "$work" switch -qc feature
if make -s -C "$work" release >"$tmp/feature.out" 2>&1; then
  echo 'FAIL make-release: feature branch was published'
  exit 1
fi
grep -Fq 'release requires master' "$tmp/feature.out" || {
  cat "$tmp/feature.out"
  echo 'FAIL make-release: feature branch failed without the release guard'
  exit 1
}

git -C "$work" switch -q master
touch "$work/untracked"
if make -s -C "$work" release >"$tmp/dirty.out" 2>&1; then
  echo 'FAIL make-release: dirty worktree was published'
  exit 1
fi
grep -Fq 'release requires a clean worktree' "$tmp/dirty.out" || {
  cat "$tmp/dirty.out"
  echo 'FAIL make-release: dirty worktree failed without the release guard'
  exit 1
}
rm "$work/untracked"

sed -i.bak 's/version = "0.2.0"/version = "0.2.1"/' "$work/Cargo.toml"
rm "$work/Cargo.toml.bak"
git -C "$work" commit -qam 'chore: bump version to 0.2.1'
git -C "$work" tag v0.2.1
if make -s -C "$work" release >"$tmp/notes.out" 2>&1; then
  echo 'FAIL make-release: unchanged release notes were published'
  exit 1
fi
grep -Fq 'release requires updated RELEASE_NOTES.md' "$tmp/notes.out" || {
  cat "$tmp/notes.out"
  echo 'FAIL make-release: unchanged notes failed without the release-notes guard'
  exit 1
}
git -C "$work" tag -d v0.2.1 >/dev/null
printf '0.2.1 notes\n' >"$work/RELEASE_NOTES.md"
git -C "$work" add RELEASE_NOTES.md
git -C "$work" commit -q --amend --no-edit
git -C "$work" tag v0.2.1
make -s -C "$work" release >"$tmp/release.out"

head="$(git -C "$work" rev-parse HEAD)"
remote_head="$(git --git-dir="$origin" rev-parse refs/heads/master)"
remote_tag="$(git --git-dir="$origin" rev-parse 'refs/tags/v0.2.1^{commit}')"
if [ "$remote_head" != "$head" ] || [ "$remote_tag" != "$head" ]; then
  cat "$tmp/release.out"
  echo "FAIL make-release: head=$head remote-head=$remote_head remote-tag=$remote_tag"
  exit 1
fi

echo 'ok   make-release-guards-and-publishes-atomically'

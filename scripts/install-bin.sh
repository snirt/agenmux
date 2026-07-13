#!/usr/bin/env bash
# Install the latest verified native engine for this OS/architecture.
set -u

DIR="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$DIR/target/release/agents-mon"
[ -x "$BIN" ] && exit 0
tmp=""

case "$(uname -s):$(uname -m)" in
  Darwin:arm64)  platform="macos-aarch64" ;;
  Darwin:x86_64) platform="macos-x86_64" ;;
  Linux:aarch64|Linux:arm64) platform="linux-aarch64" ;;
  Linux:x86_64|Linux:amd64)  platform="linux-x86_64" ;;
  *) platform="" ;;
esac

download_bin() {
  [ -n "$platform" ] && command -v curl >/dev/null && command -v tar >/dev/null || return 1
  command -v sha256sum >/dev/null || command -v shasum >/dev/null || return 1

  local package="tmux-agents-mon-$platform"
  local archive="$package.tar.gz"
  local base="https://github.com/snirt/tmux-agents-mon/releases/latest/download"
  local expected actual staged
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/agents-mon.XXXXXX")" || return 1
  trap 'rm -rf "$tmp"' EXIT

  curl -fsSL "$base/$archive" -o "$tmp/$archive" || return 1
  curl -fsSL "$base/SHA256SUMS" -o "$tmp/SHA256SUMS" || return 1
  expected="$(awk -v file="$archive" '$2 == file { print $1 }' "$tmp/SHA256SUMS")"
  [ "${#expected}" -eq 64 ] || return 1
  if command -v sha256sum >/dev/null; then
    actual="$(sha256sum "$tmp/$archive" | awk '{ print $1 }')"
  else
    actual="$(shasum -a 256 "$tmp/$archive" | awk '{ print $1 }')"
  fi
  [ "$actual" = "$expected" ] || return 1

  tar -xzf "$tmp/$archive" -C "$tmp" "$package/target/release/agents-mon" || return 1
  mkdir -p "$(dirname "$BIN")"
  staged="$BIN.$$"
  cp "$tmp/$package/target/release/agents-mon" "$staged" || return 1
  chmod +x "$staged"
  mv -f "$staged" "$BIN"
}

download_bin && exit 0
if command -v cargo >/dev/null 2>&1; then
  (cd "$DIR" && cargo build --release)
  exit $?
fi
exit 1

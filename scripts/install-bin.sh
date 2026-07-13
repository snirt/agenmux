#!/usr/bin/env bash
# Install the latest verified native engine for this OS/architecture.
set -u

DIR="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$DIR/target/release/agents-mon"
STATE="$DIR/target/release/.agents-mon-version"
tmp=""
current_rev="$(git -C "$DIR" rev-parse HEAD 2>/dev/null || printf '-')"
installed_tag="$(sed -n '1p' "$STATE" 2>/dev/null)"
installed_rev="$(sed -n '2p' "$STATE" 2>/dev/null)"

# Avoid a network request on every toggle. A TPM update changes current_rev and
# forces an immediate check; otherwise checks happen at most once per day.
if [ -x "$BIN" ] && [ "$installed_rev" = "$current_rev" ] \
   && [ -n "$(find "$STATE" -mtime -1 -print 2>/dev/null)" ]; then
  exit 0
fi

case "$(uname -s):$(uname -m)" in
  Darwin:arm64)  platform="macos-aarch64" ;;
  Darwin:x86_64) platform="macos-x86_64" ;;
  Linux:aarch64|Linux:arm64) platform="linux-aarch64" ;;
  Linux:x86_64|Linux:amd64)  platform="linux-x86_64" ;;
  *) platform="" ;;
esac

write_state() {
  local staged="$STATE.$$"
  mkdir -p "$(dirname "$STATE")"
  printf '%s\n%s\n' "$1" "$current_rev" > "$staged" && mv -f "$staged" "$STATE"
}

latest_tag() {
  local url tag
  url="$(curl -fsSL -o /dev/null -w '%{url_effective}' \
    https://github.com/snirt/tmux-agents-mon/releases/latest)" || return 1
  tag="${url##*/}"
  case "$tag" in v*) printf '%s\n' "$tag" ;; *) return 1 ;; esac
}

download_bin() {
  [ -n "$platform" ] && command -v curl >/dev/null && command -v tar >/dev/null || return 1
  command -v sha256sum >/dev/null || command -v shasum >/dev/null || return 1

  local tag="$1"
  local package="tmux-agents-mon-$platform"
  local archive="$package.tar.gz"
  local base="https://github.com/snirt/tmux-agents-mon/releases/download/$tag"
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
  mv -f "$staged" "$BIN" && write_state "$tag"
}

tag=""
if command -v curl >/dev/null 2>&1; then tag="$(latest_tag)"; fi
if [ -n "$tag" ] && [ -x "$BIN" ] && [ "$installed_tag" = "$tag" ]; then
  write_state "$tag"
  exit 0
fi
if [ -n "$tag" ] && download_bin "$tag"; then exit 0; fi
[ -x "$BIN" ] && exit 0
if command -v cargo >/dev/null 2>&1; then
  (cd "$DIR" && cargo build --release)
  exit $?
fi
exit 1

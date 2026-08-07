#!/usr/bin/env bash
# Install the verified native engine matching this checkout's version, and
# record the available releases so the sidebar can offer an update/rollback.
#
#   install-bin.sh              install/refresh the engine (throttled)
#   install-bin.sh refresh      re-read the release list now, ignoring the throttle
#   install-bin.sh fetch T DIR  download+verify release T, extract into DIR
set -u

DIR="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$DIR/target/release/agents-mon"
STATE="$DIR/target/release/.agents-mon-version"
LATEST="$DIR/target/release/.agents-mon-latest"
TAGS="$DIR/target/release/.agents-mon-tags"
REPO="${AGENTS_MON_REPO:-https://github.com/snirt/tmux-agents-mon}"
tmp=""

case "$(uname -s):$(uname -m)" in
  Darwin:arm64)  platform="macos-aarch64" ;;
  Darwin:x86_64) platform="macos-x86_64" ;;
  Linux:aarch64|Linux:arm64) platform="linux-aarch64" ;;
  Linux:x86_64|Linux:amd64)  platform="linux-x86_64" ;;
  *) platform="" ;;
esac

# Download the release archive for $1, verify its checksum, extract the whole
# package into $2 (the archive carries the full plugin source, not just the
# binary — that is what update.sh switches versions with). Prints the package
# directory. Nothing is written outside $2 unless verification passed.
fetch_pkg() {
  local tag="$1" dest="$2"
  [ -n "$tag" ] && [ -n "$dest" ] || return 1
  [ -n "$platform" ] && command -v curl >/dev/null && command -v tar >/dev/null || return 1
  command -v sha256sum >/dev/null || command -v shasum >/dev/null || return 1

  local package="tmux-agents-mon-$platform"
  local archive="$package.tar.gz"
  local base="$REPO/releases/download/$tag"
  local expected actual
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/agents-mon.XXXXXX")" || return 1
  trap 'rm -rf "$tmp"' EXIT

  curl -fsSL "$base/$archive" -o "$tmp/$archive" || return 1
  curl -fsSL "$base/SHA256SUMS" -o "$tmp/SHA256SUMS" || return 1
  expected="$(awk -v file="$archive" '$2 == file || $2 == "./" file { print $1 }' "$tmp/SHA256SUMS")"
  [ "${#expected}" -eq 64 ] || return 1
  if command -v sha256sum >/dev/null; then
    actual="$(sha256sum "$tmp/$archive" | awk '{ print $1 }')"
  else
    actual="$(shasum -a 256 "$tmp/$archive" | awk '{ print $1 }')"
  fi
  [ "$actual" = "$expected" ] || return 1

  mkdir -p "$dest" || return 1
  tar -xzf "$tmp/$archive" -C "$dest" || return 1
  [ -d "$dest/$package" ] || return 1
  printf '%s\n' "$dest/$package"
}

if [ "${1:-}" = "fetch" ]; then
  fetch_pkg "${2:-}" "${3:-}" || exit 1
  exit 0
fi

current_rev="$(git -C "$DIR" rev-parse HEAD 2>/dev/null || printf '-')"
installed_tag="$(sed -n '1p' "$STATE" 2>/dev/null)"
installed_rev="$(sed -n '2p' "$STATE" 2>/dev/null)"
# the release this checkout's source belongs to — Cargo.toml is the sole
# version source, so a rollback to v0.1.5 pins the v0.1.5 binary with no
# extra state to track
want="$(bash "$DIR/scripts/version.sh" tag 2>/dev/null)"

write_state() {
  local staged="$STATE.$$"
  mkdir -p "$(dirname "$STATE")"
  printf '%s\n%s\n' "$1" "$current_rev" > "$staged" && mv -f "$staged" "$STATE"
}

latest_tag() {
  local url tag
  url="$(curl -fsSL -o /dev/null -w '%{url_effective}' \
    "$REPO/releases/latest")" || return 1
  tag="${url##*/}"
  case "$tag" in v*) printf '%s\n' "$tag" ;; *) return 1 ;; esac
}

# What is out there, for the sidebar's update notice and version picker.
# Best effort: a failure here must never block installing the engine.
record_releases() {
  local tag staged
  mkdir -p "$(dirname "$LATEST")"
  tag="$(latest_tag)" || return 0
  staged="$LATEST.$$"
  printf '%s\n' "$tag" > "$staged" && mv -f "$staged" "$LATEST"
  command -v git >/dev/null 2>&1 || return 0
  staged="$TAGS.$$"
  # ls-remote needs no API token and has no rate limit, unlike the releases API.
  # A pushed tag has no binaries until its release publishes — minutes later,
  # or never when the build fails — so offer nothing newer than the published
  # latest: everything at or below it went through the same release pipeline.
  git ls-remote --tags --refs "$REPO" 2>/dev/null |
    awk '{ sub(/^refs\/tags\//, "", $2); if ($2 ~ /^v[0-9]/) print $2 }' |
    sort -V -r |
    awk -v top="$tag" 'seen || $0 == top { seen = 1 } seen' > "$staged"
  if [ -s "$staged" ]; then mv -f "$staged" "$TAGS"; else rm -f "$staged"; fi
}

# Install the release archive's engine for $1 (verified), atomically.
download_bin() {
  local tag="$1" pkg scratch staged rc=1
  scratch="$(mktemp -d "${TMPDIR:-/tmp}/agents-mon-pkg.XXXXXX")" || return 1
  pkg="$(fetch_pkg "$tag" "$scratch")" || { rm -rf "$scratch"; return 1; }
  if [ -f "$pkg/target/release/agents-mon" ]; then
    mkdir -p "$(dirname "$BIN")"
    staged="$BIN.$$"
    if cp "$pkg/target/release/agents-mon" "$staged"; then
      chmod +x "$staged"
      mv -f "$staged" "$BIN" && write_state "$tag" && rc=0
    fi
    [ "$rc" -eq 0 ] || rm -f "$staged"
    # macOS packages also carry the notification helper (see install-app.sh)
    if [ "$rc" -eq 0 ] && [ -f "$pkg/target/release/agents-mon-notifier" ]; then
      staged="$(dirname "$BIN")/agents-mon-notifier.$$"
      if cp "$pkg/target/release/agents-mon-notifier" "$staged"; then
        chmod +x "$staged"
        mv -f "$staged" "$(dirname "$BIN")/agents-mon-notifier" || rm -f "$staged"
      fi
    fi
  fi
  rm -rf "$scratch"
  return "$rc"
}

# Opening the version picker is an explicit "what is out there?" — answer it
# now rather than serving a list that can be a day old.
if [ "${1:-}" = "refresh" ]; then
  command -v curl >/dev/null 2>&1 && record_releases
  exit 0
fi

# macOS: keep the AgentsMon.app notification helper installed and current, on
# every engine install path below. Quiet: notification permission is requested
# by the first real notification, not here.
sync_app() {
  local notifier="$DIR/target/release/agents-mon-notifier"
  local app_bin="$HOME/Applications/AgentsMon.app/Contents/MacOS/agents-mon-notifier"
  [ "$(uname -s)" = Darwin ] && [ -x "$notifier" ] || return 0
  cmp -s "$notifier" "$app_bin" 2>/dev/null && return 0
  case "$(tmux show-option -gqv @agents-mon-notifications 2>/dev/null)" in
    off | false | 0) return 0 ;;
  esac
  bash "$DIR/scripts/install-app.sh" --quiet >/dev/null 2>&1 || true
}
trap sync_app EXIT

# "what is released" and "does the engine need installing" are separate
# questions with separate throttles. Gating the first behind the second left an
# up-to-date install with no release list at all — and so no update notice and
# an empty version picker — until its install marker aged past a day.
if command -v curl >/dev/null 2>&1 \
   && [ -z "$(find "$LATEST" -mtime -1 -print 2>/dev/null)" ]; then
  record_releases
fi

# Avoid a download on every toggle. A TPM update changes current_rev and forces
# an immediate check; otherwise the engine is re-checked at most once per day.
if [ -x "$BIN" ] && [ "$installed_rev" = "$current_rev" ] \
   && [ -n "$(find "$STATE" -mtime -1 -print 2>/dev/null)" ]; then
  exit 0
fi

# 1. the engine this checkout's source expects
if [ -n "$want" ]; then
  if [ -x "$BIN" ] && [ "$installed_tag" = "$want" ]; then
    write_state "$want"
    exit 0
  fi
  download_bin "$want" && exit 0
fi
# 2. no release for this version — a checkout tracking master is ahead of every
#    tag, so fall back to the newest release rather than leaving it on bash
tag="$(sed -n '1p' "$LATEST" 2>/dev/null)"
if [ -n "$tag" ] && [ "$tag" != "$want" ]; then
  if [ -x "$BIN" ] && [ "$installed_tag" = "$tag" ]; then
    write_state "$tag"
    exit 0
  fi
  download_bin "$tag" && exit 0
fi
# 3. keep whatever works: existing binary, then a local build, then bash
[ -x "$BIN" ] && exit 0
if command -v cargo >/dev/null 2>&1; then
  (cd "$DIR" && cargo build --release)
  exit $?
fi
exit 1

#!/usr/bin/env bash
# Switch the whole plugin — source and engine — to a release, forward or back.
#
#   update.sh            latest release
#   update.sh v0.1.5     roll back (or forward) to that release
#
# Cargo.toml is the sole version source, so moving the source is enough:
# install-bin.sh then fetches the engine that matches it.
set -u

DIR="$(cd "$(dirname "$0")/.." && pwd)"
LATEST="$DIR/target/release/.agents-mon-latest"
REPO="${AGENTS_MON_REPO:-https://github.com/snirt/tmux-agents-mon}"

note() {
  tmux display-message "agents-mon: $*" 2>/dev/null || printf 'agents-mon: %s\n' "$*"
}
fail() { note "$*"; exit 1; }

target="${1:-latest}"
if [ "$target" = "latest" ]; then
  target="$(sed -n '1p' "$LATEST" 2>/dev/null)"
  if [ -z "$target" ] && command -v curl >/dev/null 2>&1; then
    url="$(curl -fsSL -o /dev/null -w '%{url_effective}' "$REPO/releases/latest" 2>/dev/null)"
    target="${url##*/}"
  fi
fi
case "$target" in
  v[0-9]*) ;;
  *) fail "no release to switch to" ;;
esac

current="$(bash "$DIR/scripts/version.sh" tag 2>/dev/null)"
if [ "$target" = "$current" ]; then
  note "already on $target"
  exit 0
fi

# reopen afterwards only if the view was open when the switch started
was_open=""
if [ "$(tmux show-option -gqv @agents-mon-on 2>/dev/null)" = 1 ] ||
   [ -n "$(tmux show-option -gqv @agents-mon-sidebar 2>/dev/null)" ]; then
  was_open=1
fi

note "switching to ${target}…"   # braces: bash can fold the ellipsis bytes into the name
if [ -e "$DIR/.git" ]; then
  # a developer's own checkout must never be detached out from under an edit
  [ -z "$(git -C "$DIR" status --porcelain 2>/dev/null)" ] ||
    fail "uncommitted changes in $DIR — commit or stash first"
  git -C "$DIR" fetch --tags --quiet origin 2>/dev/null
  git -C "$DIR" rev-parse -q --verify "refs/tags/$target^{commit}" >/dev/null ||
    fail "unknown release $target"
  # detached at the tag: the normal pinned-plugin state
  git -C "$DIR" checkout --quiet "$target" || fail "could not check out $target"
else
  # tarball install: the release archive carries the full plugin source
  scratch="$(mktemp -d "${TMPDIR:-/tmp}/agents-mon-up.XXXXXX")" || fail "mktemp failed"
  trap 'rm -rf "$scratch"' EXIT
  pkg="$(bash "$DIR/scripts/install-bin.sh" fetch "$target" "$scratch")" ||
    fail "could not download $target"
  rm -rf "$pkg/target"   # keep the installed engine; refreshed just below
  cp -R "$pkg/." "$DIR/" || fail "could not write to $DIR"
fi

# clear the install marker so the daily throttle cannot skip the engine swap;
# install-bin.sh also keeps the macOS notification helper app current
rm -f "$DIR/target/release/.agents-mon-version"
bash "$DIR/scripts/install-bin.sh" >/dev/null 2>&1

# restart against the new code: drop the running view, re-run the entry point
# for the new key bindings and hooks, then reopen if it was open
if command -v tmux >/dev/null 2>&1 && tmux info >/dev/null 2>&1; then
  bash "$DIR/scripts/teardown.sh"
  bash "$DIR/agents-mon.tmux"
  [ -n "$was_open" ] && bash "$DIR/scripts/toggle.sh"
fi
note "now on $target"

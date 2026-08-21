#!/usr/bin/env bash
# Pre-binary activation bootstrap. Rust owns all split/popup runtime behavior.
DIR="$(cd "$(dirname "$0")/.." && pwd)"

BIN="$(tmux show-option -gqv @agents-mon-bin)"
[ -n "$BIN" ] || BIN="$DIR/target/release/agents-mon"

# A source checkout starts without a binary. Serialize first activation with
# the eager installer from agents-mon.tmux, then let the freshly installed
# tree refresh bindings before retrying the requested action.
if [ ! -x "$BIN" ]; then
  locked=""
  unlock() {
    [ -n "$locked" ] || return
    locked=""
    tmux wait-for -U agents-mon-install 2>/dev/null || true
  }
  trap unlock EXIT HUP INT TERM
  if tmux wait-for -L agents-mon-install; then
    locked=1
    if [ ! -x "$BIN" ] && [ "$BIN" = "$DIR/target/release/agents-mon" ]; then
      bash "$DIR/scripts/install-bin.sh" >/dev/null 2>&1 || true
    fi
    unlock
  fi
  if [ ! -x "$BIN" ]; then
    tmux display-message 'agents-mon: native engine installation failed' 2>/dev/null || true
    exit 1
  fi
  bash "$DIR/agents-mon.tmux"
fi

exec env AGENTS_MON_DIR="$DIR" "$BIN" toggle "$@"

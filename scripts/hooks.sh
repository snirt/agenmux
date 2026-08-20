#!/usr/bin/env bash
# Compatibility entry point for live tmux servers holding the old script path.
DIR="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$(tmux show-option -gqv @agents-mon-bin)"
[ -n "$BIN" ] || BIN="$DIR/target/release/agents-mon"
export AGENTS_MON_DIR="$DIR"
exec "$BIN" setup

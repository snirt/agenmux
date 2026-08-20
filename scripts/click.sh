#!/usr/bin/env bash
DIR="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${AGENTS_MON_BIN:-$(tmux show-option -gqv @agents-mon-bin)}"
[ -n "$BIN" ] || BIN="$DIR/target/release/agents-mon"
exec "$BIN" click "$@"

#!/usr/bin/env bash
# Add a mirror pane to one window (default: the client's current window).
# Called at toggle-on for every window, and from hooks for windows created
# or first visited while mirror mode is on.
DIR="$(cd "$(dirname "$0")/.." && pwd)"

[ "$(tmux show-option -gqv @agents-mon-on)" = 1 ] || exit 0
win="${1:-$(tmux display-message -p '#{window_id}')}"
[ "$(tmux display-message -p -t "$win" '#{session_name}')" = "pi" ] && exit 0
tmux list-panes -t "$win" -F '#{pane_title}' 2>/dev/null | grep -qx 'agents-mon' && exit 0

BIN="$(tmux show-option -gqv @agents-mon-bin)"
[ -n "$BIN" ] || BIN="$DIR/target/release/agents-mon"
[ -x "$BIN" ] || exit 0

width="$(tmux show-option -gqv @agents-mon-width)"
# remember the layout so teardown can restore pane sizes when mirrors leave
tmux set-option -g "@agents-mon-layout-${win}" "$(tmux display-message -p -t "$win" '#{window_layout}')"
id="$(tmux split-window -hbf -d -l "${width:-30}" -t "$win" -P -F '#{pane_id}' \
  "bash -c \"'$BIN' mirror\"")" || exit 0
tmux set-option -p -t "$id" allow-rename off
tmux select-pane -t "$id" -T 'agents-mon'
exit 0

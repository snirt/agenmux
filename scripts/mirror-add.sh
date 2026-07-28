#!/usr/bin/env bash
# Add one processless preserved sidebar pane to a window.
# The filename remains stable because installed hooks call it across upgrades.
DIR="$(cd "$(dirname "$0")/.." && pwd)"

[ "$(tmux show-option -gqv @agents-mon-on)" = 1 ] || exit 0
win="${1:-$(tmux display-message -p '#{window_id}')}"
[ "$(tmux display-message -p -t "$win" '#{session_name}')" = "pi" ] && exit 0

# Hook fan-out routinely asks for the same window concurrently. A tmux-native
# lock keeps the layout snapshot and split one transaction without relying on
# pane_start_command (an empty pane deliberately has none).
lock="agents-mon-add-${win#@}"
tmux wait-for -L "$lock" || exit 0
locked=1
unlock() {
  [ "$locked" = 1 ] || return
  locked=0
  tmux wait-for -U "$lock" 2>/dev/null || true
}
trap unlock EXIT
trap 'exit 130' HUP INT TERM

[ "$(tmux show-option -gqv @agents-mon-on)" = 1 ] || exit 0
if tmux list-panes -t "$win" -F '#{pane_title}' 2>/dev/null | grep -qx agents-mon; then
  exit 0
fi

width="$(tmux show-option -gqv @agents-mon-width)"
layout="$(tmux display-message -p -t "$win" '#{window_layout}')"
tmux set-option -g "@agents-mon-layout-$win" "$layout"

# -I creates an empty input pane. Redirecting stdin closes the temporary input
# stream immediately while tmux preserves the pane itself with pane_pid=0.
id="$(tmux split-window -I -hbf -d -l "${width:-30}" -t "$win" \
  -P -F '#{pane_id}' </dev/null)" || {
  tmux set-option -gu "@agents-mon-layout-$win" 2>/dev/null
  exit 1
}
tmux set-option -p -t "$id" allow-rename off
tmux select-pane -t "$id" -T agents-mon
exit 0

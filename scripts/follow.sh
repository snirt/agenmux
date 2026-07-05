#!/usr/bin/env bash
# Hook handler: move the sidebar pane into the newly selected window.
DIR="$(cd "$(dirname "$0")/.." && pwd)"

sb="$(tmux show-option -gqv @agents-mon-sidebar)"
[ -n "$sb" ] || exit 0
if ! tmux list-panes -a -F '#{pane_id}' | grep -qx "$sb"; then
  # sidebar died — clear the stale option (hooks stay installed; they no-op)
  tmux set-option -gu @agents-mon-sidebar
  exit 0
fi

cur_win="$(tmux display-message -p '#{window_id}')"
sb_win="$(tmux display-message -p -t "$sb" '#{window_id}')"
[ "$cur_win" = "$sb_win" ] && exit 0

active="$(tmux display-message -p '#{pane_id}')"
[ "$active" = "$sb" ] && exit 0
# keep the sidebar's current width (incl. manual resizes) across moves —
# unless it fills its window (orphaned after last real pane closed), then
# use the last remembered width instead
width="$(tmux display-message -p -t "$sb" '#{pane_width}')"
win_w="$(tmux display-message -p -t "$sb" '#{window_width}')"
if [ "$width" = "$win_w" ]; then
  width="$(tmux show-option -gqv @agents-mon-last-width)"
  [ -n "$width" ] || width="$(tmux show-option -gqv @agents-mon-width)"
else
  tmux set-option -g @agents-mon-last-width "$width"
fi
tmux join-pane -hbf -d -l "${width:-30}" -s "$sb" -t "$active"
tmux resize-pane -t "$sb" -x "${width:-30}"

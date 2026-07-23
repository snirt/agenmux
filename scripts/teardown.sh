#!/usr/bin/env bash
# Mirror-mode teardown: kill every mirror pane, restore each window's saved
# layout, drop the bookkeeping options. Idempotent — safe to run on a clean
# server (toggle.sh runs it before opening to clear crash leftovers).
tmux list-panes -a -F '#{pane_id}	#{pane_title}	#{window_id}' 2>/dev/null |
  awk -F'\t' '$2 == "agents-mon" { print $1, $3 }' |
  while read -r pane win; do
    tmux kill-pane -t "$pane" 2>/dev/null
    lay="$(tmux show-option -gqv "@agents-mon-layout-${win}")"
    [ -n "$lay" ] || continue
    # size-mismatched restores leave dead (dotted) window area — skip them
    size="${lay#*,}"; size="${size%%,*}"
    [ "$size" = "$(tmux display-message -p -t "$win" '#{window_width}x#{window_height}' 2>/dev/null)" ] \
      && tmux select-layout -t "$win" "$lay" 2>/dev/null
  done
# unset every saved layout (including windows whose mirror already died)
tmux show-options -g 2>/dev/null | sed -n 's/^\(@agents-mon-layout-@[0-9]*\) .*/\1/p' |
  while read -r opt; do tmux set-option -gu "$opt"; done
tmux set-option -gu @agents-mon-on 2>/dev/null
exit 0

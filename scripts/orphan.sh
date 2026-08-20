#!/usr/bin/env bash
# When a processless sidebar is the only pane left in a window, move only
# clients stranded there and kill that window. Unrelated clients and control
# clients are never moved.
[ "$(tmux show-option -gqv @agents-mon-on)" = 1 ] || exit 0

tmux list-windows -a -F '#{window_id}	#{window_panes}	#{session_id}' |
  while IFS=$'\t' read -r win npanes session; do
    [ "$npanes" = 1 ] || continue
    [ "$(tmux list-panes -t "$win" -F '#{pane_title}' 2>/dev/null)" = "agents-mon" ] || continue
    target="$(tmux list-windows -t "$session" -F '#{window_id}	#{window_last_flag}' |
      awk -v win="$win" '$1 != win && $2 == 1 { print $1; exit }')"
    [ -n "$target" ] || target="$(tmux list-windows -t "$session" -F '#{window_id}' |
      awk -v win="$win" '$1 != win { print $1; exit }')"
    tmux list-clients -f '#{?#{m:*control-mode*,#{client_flags}},0,1}' -F '#{client_name}' |
      while IFS= read -r client; do
        [ -n "$client" ] || continue
        [ "$(tmux display-message -p -c "$client" '#{window_id}' 2>/dev/null)" = "$win" ] || continue
        if [ -n "$target" ]; then
          tmux switch-client -c "$client" -t "$target" 2>/dev/null || true
        else
          tmux switch-client -c "$client" -l 2>/dev/null ||
            tmux switch-client -c "$client" -p 2>/dev/null || true
        fi
      done
    tmux set-option -gu "@agents-mon-layout-${win}" 2>/dev/null
    tmux kill-window -t "$win" 2>/dev/null
  done
exit 0

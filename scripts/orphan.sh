#!/usr/bin/env bash
# Hook handler (pane-exited): when the sidebar is the only pane left in its
# window, jump to the previous window/session instead of stranding the user.
# follow.sh (select-window/session-changed hooks) then pulls the sidebar over
# and the emptied window dies on its own.
sb="$(tmux show-option -gqv @agents-mon-sidebar)"
[ -n "$sb" ] || exit 0
win="$(tmux display-message -p -t "$sb" '#{window_id}' 2>/dev/null)" || exit 0
[ "$(tmux list-panes -t "$win" -F x | wc -l)" -eq 1 ] || exit 0

session="$(tmux display-message -p -t "$sb" '#{session_id}')"
if [ "$(tmux list-windows -t "$session" -F x | wc -l)" -gt 1 ]; then
  tmux last-window -t "$session" 2>/dev/null || tmux next-window -t "$session"
else
  # last window of the session — hop to another session if one exists
  tmux switch-client -l 2>/dev/null || tmux switch-client -p 2>/dev/null
fi
exit 0

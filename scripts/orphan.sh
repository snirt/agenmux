#!/usr/bin/env bash
# Hook handler (pane-exited/window-pane-changed): when the sidebar is the only
# pane left in its window, move only clients that are actually stranded on that
# sidebar-only window. follow.sh then pulls the sidebar into the new window and
# the emptied window dies on its own. Do not move an unrelated active client just
# because an orphaned sidebar exists in another window/session.
sb="$(tmux show-option -gqv @agents-mon-sidebar)"
[ -n "$sb" ] || exit 0
win="$(tmux display-message -p -t "$sb" '#{window_id}' 2>/dev/null)" || exit 0
[ "$(tmux list-panes -t "$win" -F x | wc -l)" -eq 1 ] || exit 0

session="$(tmux display-message -p -t "$sb" '#{session_id}')"

clients=""
while IFS= read -r client; do
  [ -n "$client" ] || continue
  client_win="$(tmux display-message -p -c "$client" '#{window_id}' 2>/dev/null)" || continue
  [ "$client_win" = "$win" ] && clients="$clients$client
"
done <<EOF
$(tmux list-clients -F '#{client_name}')
EOF

[ -n "$clients" ] || exit 0

target="$(tmux list-windows -t "$session" -F '#{window_id}	#{window_last_flag}' |
  awk -v win="$win" '$1 != win && $2 == 1 { print $1; exit }')"
[ -n "$target" ] || target="$(tmux list-windows -t "$session" -F '#{window_id}' |
  awk -v win="$win" '$1 != win { print $1; exit }')"

if [ -n "$target" ]; then
  while IFS= read -r client; do
    [ -n "$client" ] || continue
    tmux switch-client -c "$client" -t "$target" 2>/dev/null || true
  done <<EOF
$clients
EOF
else
  # last window of the session — hop only stranded clients to another session if one exists
  while IFS= read -r client; do
    [ -n "$client" ] || continue
    tmux switch-client -c "$client" -l 2>/dev/null || tmux switch-client -c "$client" -p 2>/dev/null || true
  done <<EOF
$clients
EOF
fi
exit 0

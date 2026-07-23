#!/usr/bin/env bash
# Hook handler (pane-exited/window-pane-changed): when the sidebar is the only
# pane left in its window, move only clients that are actually stranded on that
# sidebar-only window. follow.sh then pulls the sidebar into the new window and
# the emptied window dies on its own. Do not move an unrelated active client just
# because an orphaned sidebar exists in another window/session.
# Mirror mode: a window whose only pane is a mirror lost its agent — move
# stranded clients off it and kill it (the mirror pane won't die on its own).
if [ "$(tmux show-option -gqv @agents-mon-on)" = 1 ]; then
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
            tmux switch-client -c "$client" -l 2>/dev/null \
              || tmux switch-client -c "$client" -p 2>/dev/null || true
          fi
        done
      tmux set-option -gu "@agents-mon-layout-${win}" 2>/dev/null
      tmux kill-window -t "$win" 2>/dev/null
    done
  exit 0
fi

sb="$(tmux show-option -gqv @agents-mon-sidebar)"
[ -n "$sb" ] || exit 0
win="$(tmux display-message -p -t "$sb" '#{window_id}' 2>/dev/null)"
if [ -z "$win" ]; then
  # sidebar died (q/Esc) — without this, the freed width lands on one
  # neighbor and the skewed sizes get re-saved as "clean" on the next visit
  tmux show-options -g 2>/dev/null | sed -n 's/^\(@agents-mon-layout-@[0-9]*\) .*/\1/p' |
    while read -r opt; do
      lay="$(tmux show-option -gqv "$opt")"
      win="${opt#@agents-mon-layout-}"
      # size-mismatched restores leave dead (dotted) window area — skip them
      size="${lay#*,}"; size="${size%%,*}"
      [ "$size" = "$(tmux display-message -p -t "$win" '#{window_width}x#{window_height}' 2>/dev/null)" ] \
        && tmux select-layout -t "$win" "$lay" 2>/dev/null
      tmux set-option -gu "$opt"
    done
  tmux set-option -gu @agents-mon-sidebar
  tmux set-option -gu @agents-mon-sidebar-win
  tmux set-option -gu @agents-mon-prev-win
  exit 0
fi
[ "$(tmux list-panes -t "$win" -F x | wc -l)" -eq 1 ] || exit 0

session="$(tmux display-message -p -t "$sb" '#{session_id}')"

clients=""
while IFS= read -r client; do
  [ -n "$client" ] || continue
  client_win="$(tmux display-message -p -c "$client" '#{window_id}' 2>/dev/null)" || continue
  [ "$client_win" = "$win" ] && clients="$clients$client
"
done <<EOF
$(tmux list-clients -f '#{?#{m:*control-mode*,#{client_flags}},0,1}' -F '#{client_name}')
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

#!/usr/bin/env bash
# Install all plugin hooks — single source of truth, called from toggle.sh
# and agents-mon.tmux (config reloads clear hooks).
DIR="$(cd "$(dirname "$0")/.." && pwd)"

ver="$(tmux -V | sed 's/[^0-9.]//g')"
if awk -v v="$ver" 'BEGIN { exit !(v + 0 >= 3.2) }'; then
  # Native follow: the join executes inside the server during the switch, so
  # the new window first renders already WITH the sidebar — no flash/bump.
  # Server-side serialization also kills the old two-hooks race, no lock.
  # Guard: sidebar open, not already in this window, and never follow into pi.
  guard='#{&&:#{&&:#{!=:#{@agents-mon-sidebar},},#{!=:#{@agents-mon-sidebar-win},#{window_id}}},#{!=:#{session_name},pi}}'
  body="run -C 'set -g @agents-mon-prev-win #{@agents-mon-sidebar-win}'"
  body="$body ; run -C 'set -g @agents-mon-layout-#{window_id} \"#{window_layout}\"'"
  body="$body ; run -C 'join-pane -hbf -d -l #{?#{@agents-mon-width},#{@agents-mon-width},30} -s #{@agents-mon-sidebar} -t #{pane_id}'"
  body="$body ; run -C 'set -g @agents-mon-sidebar-win #{window_id}'"
  body="$body ; run-shell -b 'bash $DIR/scripts/restore.sh'"
  follow="if -F \"$guard\" { $body }"
else
  follow="run-shell 'bash $DIR/scripts/follow.sh'"
fi
tmux set-hook -g 'after-select-window[42]' "$follow"
tmux set-hook -g 'client-session-changed[42]' "$follow"
# pane-exited misses kill-pane, and window-pane-changed fires mid-teardown
# (the dying pane still resolves, so orphan.sh takes the wrong branch);
# window-layout-changed fires after removal and is the reliable one. All
# three stay — orphan.sh is a cheap guard-and-exit when nothing died.
tmux set-hook -g 'pane-exited[42]' "run-shell 'bash $DIR/scripts/orphan.sh'"
tmux set-hook -g 'window-pane-changed[42]' "run-shell 'bash $DIR/scripts/orphan.sh'"
tmux set-hook -g 'window-layout-changed[42]' "run-shell 'bash $DIR/scripts/orphan.sh'"
# client resizes rescale panes proportionally — snap the sidebar back
tmux set-hook -g 'window-resized[42]' "run-shell 'bash $DIR/scripts/pin.sh'"

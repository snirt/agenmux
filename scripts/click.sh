#!/usr/bin/env bash
# Mouse click handler: in the sidebar, jump to the clicked agent's pane;
# anywhere else, replicate tmux's default click (select pane + pass through).
# Args: $1 = clicked #{pane_id}, $2 = #{mouse_y}
pane="$1" y="$2"
DIR="$(cd "$(dirname "$0")/.." && pwd)"
ROWS_FILE="${TMPDIR:-/tmp}/agents-mon-rows-${pane#%}"

# sidebar layout: y0 header, y1 blank, agent rows from y2
row=$((y - 1))
target="$(awk -v n="$row" 'NR == n { print $1 }' "$ROWS_FILE" 2>/dev/null)"
case "$target" in
  %*)
    # relocate the sidebar off-screen first — no visible reflow after switch
    bash "$DIR/scripts/follow.sh" "$target"
    client="$(tmux list-clients -f '#{?#{m:*control-mode*,#{client_flags}},0,1}' -F '#{client_name}' | head -n 1)"
    [ -n "$client" ] && tmux switch-client -c "$client" -t "$target" 2>/dev/null
    tmux select-window -t "$target"
    tmux select-pane -t "$target"
    ;;
esac
exit 0

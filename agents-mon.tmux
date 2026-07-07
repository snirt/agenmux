#!/usr/bin/env bash
# tmux-agents-mon TPM entry point.
CURRENT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

key="$(tmux show-option -gqv @agents-mon-key)"
tmux bind-key "${key:-A}" run-shell -b "bash '$CURRENT_DIR/scripts/toggle.sh'"

# optional dedicated popup key, e.g. set -g @agents-mon-popup-key 'e'
popup_key="$(tmux show-option -gqv @agents-mon-popup-key)"
[ -n "$popup_key" ] && tmux bind-key "$popup_key" run-shell -b "bash '$CURRENT_DIR/scripts/toggle.sh' popup"

# window-scoped leftovers (pre-1.0) shadow the global option in the mouse
# binding's format comparison — purge them
tmux list-windows -a -F '#{window_id}' 2>/dev/null | while read -r w; do
  tmux set-option -wu -t "$w" @agents-mon-sidebar 2>/dev/null
done

# config reloads may clear hooks — re-install follow if a sidebar is open
sb="$(tmux show-option -gqv @agents-mon-sidebar)"
if [ -n "$sb" ] && tmux list-panes -a -F '#{pane_id}' | grep -qx "$sb"; then
  tmux set-hook -g 'after-select-window[42]' "run-shell 'bash $CURRENT_DIR/scripts/follow.sh'"
  tmux set-hook -g 'client-session-changed[42]' "run-shell 'bash $CURRENT_DIR/scripts/follow.sh'"
  # pane-exited misses kill-pane; window-pane-changed catches it (guarded, so
  # double-firing is harmless)
  tmux set-hook -g 'pane-exited[42]' "run-shell 'bash $CURRENT_DIR/scripts/orphan.sh'"
  tmux set-hook -g 'window-pane-changed[42]' "run-shell 'bash $CURRENT_DIR/scripts/orphan.sh'"
fi

# click a sidebar row -> jump to that agent; any other pane keeps the native
# click behavior (mouse event stays intact — no run-shell detour)
if [ "$(tmux show-option -gv mouse)" = "on" ]; then
  tmux bind-key -n MouseDown1Pane if-shell -F '#{==:#{pane_id},#{@agents-mon-sidebar}}' \
    "run-shell -b \"bash '$CURRENT_DIR/scripts/click.sh' '#{pane_id}' '#{mouse_y}'\"" \
    'select-pane -t = ; send-keys -M'
fi

# hide windows matching a name pattern from the prefix+w picker,
# e.g. set -g @agents-mon-hide-windows 'agents*'
hide="$(tmux show-option -gqv @agents-mon-hide-windows)"
if [ -n "$hide" ]; then
  # escape tmux format metachars so the pattern can't corrupt the filter
  hide=${hide//'#'/'##'}; hide=${hide//,/'#,'}; hide=${hide//\}/'#}'}
  tmux bind-key w choose-tree -Zw -f "#{?#{m:$hide,#{window_name}},0,1}"
elif [ -n "$(tmux show-options -gq @agents-mon-hide-windows)" ]; then
  # option set to '' — restore default picker (unset alone can't unbind: bindings persist in server)
  tmux bind-key w choose-tree -Zw
fi

# replace #{agents_mon} placeholder in status-left/right with the live segment
seg="#(bash $CURRENT_DIR/scripts/scan.sh status)"
for opt in status-left status-right; do
  v="$(tmux show-option -gqv "$opt")"
  case "$v" in
    *'#{agents_mon}'*)
      tmux set-option -g "$opt" "${v//'#{agents_mon}'/$seg}"
      ;;
  esac
done

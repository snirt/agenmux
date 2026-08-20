#!/usr/bin/env bash
# tmux-agents-mon TPM entry point.
CURRENT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

key="$(tmux show-option -gqv @agents-mon-key)"
tmux bind-key "${key:-A}" run-shell -b \
  "bash '$CURRENT_DIR/scripts/toggle.sh' '' '#{client_name}'"

# optional dedicated popup key, e.g. set -g @agents-mon-popup-key 'e'
popup_key="$(tmux show-option -gqv @agents-mon-popup-key)"
[ -n "$popup_key" ] && tmux bind-key "$popup_key" run-shell -b \
  "bash '$CURRENT_DIR/scripts/toggle.sh' popup '#{client_name}'"

# window-scoped leftovers (pre-1.0) shadow the global option in the mouse
# binding's format comparison — purge them
tmux list-windows -a -F '#{window_id}' 2>/dev/null | while read -r w; do
  tmux set-option -wu -t "$w" @agents-mon-sidebar 2>/dev/null
done
# Live servers may retain the deleted moving-sidebar hooks across an upgrade.
tmux set-hook -gu 'after-select-window[42]' 2>/dev/null || true
tmux set-hook -gu 'client-session-changed[42]' 2>/dev/null || true
tmux set-hook -gu 'session-window-changed[42]' 2>/dev/null || true

# Config reloads may clear hooks; restore them while the native daemon is on.
if [ "$(tmux show-option -gqv @agents-mon-on)" = 1 ]; then
  bash "$CURRENT_DIR/scripts/hooks.sh"
fi

# click a sidebar row -> jump to that agent; any other pane keeps the native
# click behavior (mouse event stays intact — no run-shell detour)
if [ "$(tmux show-option -gv mouse)" = "on" ]; then
  # Every processless sidebar pane is identified by its title.
  tmux bind-key -n MouseDown1Pane if-shell -F '#{==:#{pane_title},agents-mon}' \
    "run-shell -b \"bash '$CURRENT_DIR/scripts/click.sh' '#{pane_id}' '#{mouse_y}' '#{client_name}'\"" \
    'select-pane -t = ; send-keys -M'

  # wheel over the sidebar moves the selection one row per tick; elsewhere the
  # else-branches reproduce tmux's own defaults (WheelDown has no default
  # binding at all, so forwarding the event is the whole native behavior)
  tmux bind-key -n WheelUpPane if-shell -F '#{==:#{pane_title},agents-mon}' \
    "run-shell -b \"bash '$CURRENT_DIR/scripts/scroll.sh' '#{pane_id}' up\"" \
    'if -Ft= "#{||:#{pane_in_mode},#{mouse_any_flag}}" "send-keys -M" "copy-mode -e; send-keys -M"'
  tmux bind-key -n WheelDownPane if-shell -F '#{==:#{pane_title},agents-mon}' \
    "run-shell -b \"bash '$CURRENT_DIR/scripts/scroll.sh' '#{pane_id}' down\"" \
    'send-keys -M'
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

# replace #{agents_mon} placeholder in status-left/right with the live segment.
# The source checkout has no binary, so eagerly install the default in the
# background. toggle.sh takes the same tmux lock when first use beats it.
BIN="$(tmux show-option -gqv @agents-mon-bin)"
[ -n "$BIN" ] || BIN="$CURRENT_DIR/target/release/agents-mon"
if [ "$BIN" = "$CURRENT_DIR/target/release/agents-mon" ] \
   && [ "${AGENTS_MON_INSTALL_REFRESH:-}" != 1 ]; then
  (
    locked=""
    unlock() {
      [ -n "$locked" ] || return
      locked=""
      tmux wait-for -U agents-mon-install 2>/dev/null || true
    }
    trap unlock EXIT HUP INT TERM
    tmux wait-for -L agents-mon-install || exit 0
    locked=1
    before=""
    [ -x "$BIN" ] && before=1
    bash "$CURRENT_DIR/scripts/install-bin.sh" >/dev/null 2>&1 || true
    # A just-installed binary can now own the status command and bindings.
    if [ -z "$before" ] && [ -x "$BIN" ]; then
      AGENTS_MON_INSTALL_REFRESH=1 bash "$CURRENT_DIR/agents-mon.tmux"
    fi
  ) &
fi
# An unknown tmux format expands empty, so leave the placeholder untouched
# while installation runs. The refresh above replaces it once Rust exists.
if [ -x "$BIN" ]; then
  seg="#($BIN status)"
  for opt in status-left status-right; do
    v="$(tmux show-option -gqv "$opt")"
    case "$v" in
      *'#{agents_mon}'*)
        tmux set-option -g "$opt" "${v//'#{agents_mon}'/$seg}"
        ;;
    esac
  done
fi

#!/usr/bin/env bash
# Move the sidebar pane into a target window. Used by the jump path
# (sidebar.rs/click.sh/toggle.sh) to relocate the sidebar BEFORE switching
# the view, so the reflow happens off-screen (no visible flash/bump), and as
# the follow-hook fallback on tmux < 3.2 (hooks.sh installs a native,
# in-server follow on newer tmux).
# Optional $1 = target pane; defaults to the client's active pane.
DIR="$(cd "$(dirname "$0")/.." && pwd)"

sb="$(tmux show-option -gqv @agents-mon-sidebar)"
[ -n "$sb" ] || exit 0
if ! tmux list-panes -a -F '#{pane_id}' | grep -qx "$sb"; then
  # sidebar died — clear the stale option (hooks stay installed; they no-op)
  tmux set-option -gu @agents-mon-sidebar
  exit 0
fi

active="${1:-$(tmux display-message -p '#{pane_id}')}"
cur_session="$(tmux display-message -p -t "$active" '#{session_name}')"
[ "$cur_session" = "pi" ] && exit 0

cur_win="$(tmux display-message -p -t "$active" '#{window_id}')"
sb_win="$(tmux display-message -p -t "$sb" '#{window_id}')"
[ "$cur_win" = "$sb_win" ] && exit 0

[ "$active" = "$sb" ] && exit 0
# ponytail: fixed width from @agents-mon-width only — the pane's current
# width can't be trusted as user intent: window resizes (e.g. two clients
# of different sizes, window-size latest) rescale panes proportionally, and
# remembering that scaled width ratchets the sidebar wider on every bounce
width="$(tmux show-option -gqv @agents-mon-width)"
# remember this window's layout so pane sizes can be restored when the
# sidebar leaves (tmux dumps the freed space onto one adjacent pane)
tmux set-option -g "@agents-mon-layout-${cur_win}" "$(tmux display-message -p -t "$cur_win" '#{window_layout}')"
tmux join-pane -hbf -d -l "${width:-30}" -s "$sb" -t "$active"
tmux resize-pane -t "$sb" -x "${width:-30}"
tmux set-option -g @agents-mon-sidebar-win "$cur_win"
# ponytail: restores pre-join layout; manual resizes made while the sidebar
# was in the window are lost on leave (fails harmlessly if panes changed)
bash "$DIR/scripts/restore.sh" "$sb_win"

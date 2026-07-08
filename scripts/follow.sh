#!/usr/bin/env bash
# Hook handler: move the sidebar pane into the newly selected window.
# Optional $1 = target pane: move the sidebar into that pane's window instead
# of the client's — lets jump/click relocate the sidebar BEFORE switching the
# view, so the reflow happens off-screen (no visible flash/bump).
DIR="$(cd "$(dirname "$0")/.." && pwd)"

sb="$(tmux show-option -gqv @agents-mon-sidebar)"
[ -n "$sb" ] || exit 0
if ! tmux list-panes -a -F '#{pane_id}' | grep -qx "$sb"; then
  # sidebar died — clear the stale option (hooks stay installed; they no-op)
  tmux set-option -gu @agents-mon-sidebar
  exit 0
fi

# session switches fire after-select-window AND client-session-changed — two
# instances race, the loser snapshots a with-sidebar layout that can never be
# restored, and the window leaks the sidebar's width on every hop. Serialize:
# waiters re-read state after the lock, so duplicates hit cur_win == sb_win
# below and no-op.
LOCK="${TMPDIR:-/tmp}/agents-mon-follow.lock"
for _ in $(seq 20); do
  mkdir "$LOCK" 2>/dev/null && { locked=1; break; }
  pid="$(cat "$LOCK/pid" 2>/dev/null)"
  [ -n "$pid" ] && ! kill -0 "$pid" 2>/dev/null && rm -rf "$LOCK"
  sleep 0.1
done
[ -n "${locked:-}" ] || exit 0
echo $$ > "$LOCK/pid"
trap 'rm -rf "$LOCK"' EXIT

active="${1:-$(tmux display-message -p '#{pane_id}')}"
cur_session="$(tmux display-message -p -t "$active" '#{session_name}')"
[ "$cur_session" = "pi" ] && exit 0

cur_win="$(tmux display-message -p -t "$active" '#{window_id}')"
sb_win="$(tmux display-message -p -t "$sb" '#{window_id}')"
[ "$cur_win" = "$sb_win" ] && exit 0

[ "$active" = "$sb" ] && exit 0
# keep the sidebar's current width (incl. manual resizes) across moves —
# unless it fills its window (orphaned after last real pane closed), then
# use the last remembered width instead
width="$(tmux display-message -p -t "$sb" '#{pane_width}')"
win_w="$(tmux display-message -p -t "$sb" '#{window_width}')"
if [ "$width" = "$win_w" ]; then
  width="$(tmux show-option -gqv @agents-mon-last-width)"
  [ -n "$width" ] || width="$(tmux show-option -gqv @agents-mon-width)"
else
  tmux set-option -g @agents-mon-last-width "$width"
fi
# remember this window's layout so pane sizes can be restored when the
# sidebar leaves (tmux dumps the freed space onto one adjacent pane)
tmux set-option -g "@agents-mon-layout-${cur_win}" "$(tmux display-message -p -t "$cur_win" '#{window_layout}')"
tmux join-pane -hbf -d -l "${width:-30}" -s "$sb" -t "$active"
tmux resize-pane -t "$sb" -x "${width:-30}"
# ponytail: restores pre-join layout; manual resizes made while the sidebar
# was in the window are lost on leave (fails harmlessly if panes changed)
old_layout="$(tmux show-option -gqv "@agents-mon-layout-${sb_win}")"
if [ -n "$old_layout" ]; then
  tmux select-layout -t "$sb_win" "$old_layout" 2>/dev/null
  tmux set-option -gu "@agents-mon-layout-${sb_win}"
fi

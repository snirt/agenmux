#!/usr/bin/env bash
# Add a mirror pane to one window (default: the client's current window).
# Called at toggle-on for every window, and from hooks for windows created
# or first visited while mirror mode is on.
DIR="$(cd "$(dirname "$0")/.." && pwd)"

[ "$(tmux show-option -gqv @agents-mon-on)" = 1 ] || exit 0
win="${1:-$(tmux display-message -p '#{window_id}')}"
[ "$(tmux display-message -p -t "$win" '#{session_name}')" = "pi" ] && exit 0

BIN="$(tmux show-option -gqv @agents-mon-bin)"
[ -n "$BIN" ] || BIN="$DIR/target/release/agents-mon"
[ -x "$BIN" ] || exit 0

width="$(tmux show-option -gqv @agents-mon-width)"
# Guard, layout save and split go in ONE command list, which the server runs as a
# single queue item — a racing caller cannot interleave. Two [43] hooks fire for one
# window switch and toggle.sh calls us directly, so concurrent adds are routine; the
# old check-then-split let all of them through. Keyed on pane_start_command, set by
# split-window itself: the pane title needs another round trip, and that gap is where
# the extra mirrors got in. Title is still ORed in so pre-existing mirrors count.
# The layout save sits inside the guard too — a racer used to save a layout that
# already held mirror #1, and teardown then restored a phantom sidebar column.
mirror='#{P:#{||:#{==:#{pane_title},agents-mon},#{m:*agents-mon-mirror*,#{pane_start_command}}}}'
body="run -C -t $win \"set -g @agents-mon-layout-$win \\\"#{window_layout}\\\"\""
body="$body ; split-window -hbf -d -l ${width:-30} -t $win"
body="$body \"bash -c \\\"'$BIN' mirror\\\" agents-mon-mirror\""
tmux if-shell -t "$win" -F "#{==:#{m:*1*,$mirror},0}" "$body" || exit 0
# -P inside an if-shell body has nowhere to print, so re-query. Self-correcting: if
# the guard was false someone else's mirror is found and re-titling it is a no-op.
id="$(tmux list-panes -t "$win" -F '#{pane_id} #{pane_start_command}' 2>/dev/null |
  awk '/agents-mon-mirror/ { print $1; exit }')"
[ -n "$id" ] || exit 0
tmux set-option -p -t "$id" allow-rename off
tmux select-pane -t "$id" -T 'agents-mon'
exit 0

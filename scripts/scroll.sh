#!/usr/bin/env bash
# Mouse wheel handler: one tick moves the sidebar selection one row, exactly
# like the arrow keys, and once scrolling settles it jumps to the agent under
# the cursor. agents-mon.tmux keeps the native wheel behavior in every other
# pane. Args: $1 = sidebar #{pane_id}, $2 = up|down
pane="$1" dir="$2"
DIR="$(cd "$(dirname "$0")/.." && pwd)"

# k/j rather than the arrow escapes: a single byte cannot arrive half-decoded
case "$dir" in
  up) key=k ;;
  down) key=j ;;
  *) exit 0 ;;
esac

# A backgrounded handler must not act after its pane disappeared.
alive() {
  tmux list-panes -a -F '#{pane_id}' 2>/dev/null | grep -Fxq "$pane"
}
alive || exit 0

# One tick is one row, through whichever input the sidebar actually owns.
send() {
  if [ "$(tmux show-option -gqv @agents-mon-on)" = 1 ]; then
    # Preserved panes are processless — the daemon owns selection, via its FIFO.
    BIN="$(tmux show-option -gqv @agents-mon-bin)"
    [ -n "$BIN" ] || BIN="$DIR/target/release/agents-mon"
    [ -x "$BIN" ] && "$BIN" key "$1"
  else
    # bash fallback: the sidebar pane is a real process with stdin
    tmux send-keys -t "$pane" "$1"
  fi
}
send "$key"

# Jumping on every tick would drag the client through every window a fast
# scroll passes over, so the jump waits for scrolling to stop. Last writer
# wins: each tick claims the token, and only the tick still holding it when
# the delay expires jumps. Set the option to 'off' for cursor-only scrolling.
delay="$(tmux show-option -gqv @agents-mon-wheel-jump)"
[ -n "$delay" ] || delay=0.3
[ "$delay" = off ] && exit 0

token="$$"
STATE="${TMPDIR:-/tmp}/agents-mon-wheel"
printf '%s' "$token" >"$STATE"
(
  sleep "$delay"
  [ "$(cat "$STATE" 2>/dev/null)" = "$token" ] || exit 0
  alive || exit 0
  send l
) >/dev/null 2>&1 &
exit 0

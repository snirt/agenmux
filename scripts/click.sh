#!/usr/bin/env bash
# Mouse click handler: jump from an agent row, or focus sidebar navigation from
# any other row. agents-mon.tmux preserves native clicks in non-sidebar panes.
# Args: $1 = clicked #{pane_id}, $2 = #{mouse_y}, $3 = #{client_name} (the
# clicking client — exact, unlike any post-hoc guess)
pane="$1" y="$2" client="$3"
DIR="$(cd "$(dirname "$0")/.." && pwd)"

# Mouse bindings provide an exact origin client and pane. A delayed background
# handler must not guess another viewer or act after either origin disappeared.
[ -n "$client" ] || exit 0
tmux list-clients -F '#{client_name}' 2>/dev/null |
  grep -Fxq "$client" ||
  exit 0
panes="$(tmux list-panes -a -F '#{pane_id}' 2>/dev/null)" || exit 0
printf '%s\n' "$panes" | grep -Fxq "$pane" || exit 0

# Processless preserved panes share the daemon's current visible-row map.
ROWS_FILE="${TMPDIR:-/tmp}/agents-mon-rows"

# y0 is header. Rows file maps every visual line below it, including optional
# contextual hints and session separators, so variable header height is safe.
row="$y"
target="$(awk -v n="$row" 'NR == n { print $1 }' "$ROWS_FILE" 2>/dev/null)"
case "$target" in
%*)
  printf '%s\n' "$panes" | grep -Fxq "$target" ||
    target=''
  ;;
esac
case "$target" in
%*)
  # Clear navigator state before switching the exact originating client.
  BIN="$(tmux show-option -gqv @agents-mon-bin)"
  [ -n "$BIN" ] || BIN="$DIR/target/release/agents-mon"
  [ ! -x "$BIN" ] || "$BIN" key all >/dev/null 2>&1
  tmux switch-client -c "$client" -t "$target" 2>/dev/null
  tmux select-window -t "$target"
  tmux select-pane -t "$target"
  ;;
*)
  # The sidebar binding replaces tmux's native select-pane action. Restore
  # that behavior when the click is not on an agent row, and enter navigation
  # only for the client that generated this mouse event.
  tmux switch-client -c "$client" -t "$pane" 2>/dev/null || exit 0
  tmux switch-client -c "$client" -T agents-mon 2>/dev/null
  ;;
esac
exit 0

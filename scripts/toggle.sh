#!/usr/bin/env bash
# Toggle the native agents view: processless left sidebars or a floating popup.
DIR="$(cd "$(dirname "$0")/.." && pwd)"

BIN="$(tmux show-option -gqv @agents-mon-bin)"
[ -n "$BIN" ] || BIN="$DIR/target/release/agents-mon"

# A source checkout starts without a binary. Serialize first activation with
# the eager installer from agents-mon.tmux, then let the freshly installed
# tree refresh bindings before retrying the requested action.
if [ ! -x "$BIN" ]; then
  locked=""
  unlock() {
    [ -n "$locked" ] || return
    locked=""
    tmux wait-for -U agents-mon-install 2>/dev/null || true
  }
  trap unlock EXIT HUP INT TERM
  if tmux wait-for -L agents-mon-install; then
    locked=1
    if [ ! -x "$BIN" ] && [ "$BIN" = "$DIR/target/release/agents-mon" ]; then
      bash "$DIR/scripts/install-bin.sh" >/dev/null 2>&1 || true
    fi
    unlock
  fi
  if [ ! -x "$BIN" ]; then
    tmux display-message 'agents-mon: native engine installation failed' 2>/dev/null || true
    exit 1
  fi
  bash "$DIR/agents-mon.tmux"
  exec bash "$0" "$@"
fi

# command must start with a bare word: tmux hands it to default-shell,
# and e.g. nushell rejects a quoted token in command position
SIDEBAR_CMD="bash -c \"'$BIN' sidebar\""

# mode from arg (bound key) or @agents-mon-display; default split sidebar
mode="${1:-$(tmux show-option -gqv @agents-mon-display)}"
if [ "$mode" = "popup" ] || [ "$mode" = "float" ]; then
  client="${2:-$(bash "$DIR/scripts/client.sh")}" # stable popup owner
  PIN="${TMPDIR:-/tmp}/agents-mon-pin"
  if [ -f "$PIN" ]; then
    rm -f "$PIN"
    exit 0
  fi
  touch "$PIN"
  width="$(tmux show-option -gqv @agents-mon-width)"
  height="$(tmux show-option -gqv @agents-mon-height)"
  if [ -z "$height" ]; then
    # fit the fleet: agent row + title row each, session headers, up to 2
    # header rows, popup border; floor 15 keeps the help screen readable
    cache="${TMPDIR:-/tmp}/agents-mon-scan-cache"
    if [ -s "$cache" ]; then
      height=$(($(wc -l <"$cache") + \
      $(awk -F'\t' '$6 != "" {n++} END {print n+0}' "$cache") + \
      $(cut -f2 "$cache" | cut -d: -f1 | sort -u | wc -l) + 5))
      max=$(($(tmux display-message -p '#{client_height}') - 2))
      [ "$height" -gt "$max" ] && height=$max
      [ "$height" -lt 15 ] && height=15
    fi
  fi
  # Enter closes and reopens the popup over a selected target. Explicit quit
  # removes the pin inside the Rust sidebar and ends this loop.
  while [ -f "$PIN" ]; do
    popup_args=(-E -w "${width:-40}" -h "${height:-15}" -e "AGENTS_MON_PIN=$PIN")
    if [ -n "$client" ]; then
      popup_args+=(-c "$client" -e "AGENTS_MON_POPUP_CLIENT=$client")
    fi
    tmux display-popup "${popup_args[@]}" "$SIDEBAR_CMD"
    if [ -f "$PIN.jump" ]; then
      target="$(cat "$PIN.jump")"
      rm -f "$PIN.jump"
      client="$(bash "$DIR/scripts/client.sh")"
      [ -n "$client" ] && tmux switch-client -c "$client" -t "$target" 2>/dev/null
      tmux select-window -t "$target"
      tmux select-pane -t "$target"
    else
      rm -f "$PIN"
      break
    fi
  done
  exit 0
fi

# One headless daemon renders directly into processless preserved panes.
control="$(tmux show-option -gqv @agents-mon-control-client)"
alive=0
if [ -n "$control" ] &&
  tmux list-clients -F '#{client_name}' 2>/dev/null | grep -Fxq "$control"; then
  alive=1
fi
if [ "$(tmux show-option -gqv @agents-mon-on)" = 1 ] && [ "$alive" = 1 ]; then
  bash "$DIR/scripts/mirror-add.sh"
else
  bash "$DIR/scripts/teardown.sh"
  tmux set-option -g @agents-mon-on 1
  nohup "$BIN" daemon >/dev/null 2>&1 </dev/null &
  while read -r win; do
    bash "$DIR/scripts/mirror-add.sh" "$win"
  done <<EOF
$(tmux list-windows -a -F '#{window_id}')
EOF
  bash "$DIR/scripts/hooks.sh"
fi
if [ "$(tmux show-option -gqv @agents-mon-nav-version)" != 12 ]; then
  bash "$DIR/scripts/hooks.sh"
fi
client="${2:-$(bash "$DIR/scripts/client.sh")}" # old live bindings omit arg 2
if [ -n "$client" ]; then
  win="$(tmux display-message -p -c "$client" '#{window_id}')"
  pane="$(tmux list-panes -t "$win" \
    -f '#{==:#{pane_title},agents-mon}' -F '#{pane_id}' | head -n 1)"
  [ -n "$pane" ] && tmux select-pane -t "$pane"
  tmux switch-client -c "$client" -T agents-mon 2>/dev/null
fi
exit 0

#!/usr/bin/env bash
# Toggle the agents view: left-split sidebar (follows window switches)
# or floating popup (set -g @agents-mon-display 'popup'; stays until closed).
DIR="$(cd "$(dirname "$0")/.." && pwd)"

# prefer the Rust binary when built; bash sidebar otherwise
BIN="$(tmux show-option -gqv @agents-mon-bin)"
[ -n "$BIN" ] || BIN="$DIR/target/release/agents-mon"
# install the default binary in the background; bash sidebar serves this open
if [ "$BIN" = "$DIR/target/release/agents-mon" ]; then
  bash "$DIR/scripts/install-bin.sh" >/dev/null 2>&1 &
fi
# command must start with a bare word: tmux hands it to default-shell,
# and e.g. nushell rejects a quoted token in command position
if [ -x "$BIN" ]; then
  SIDEBAR_CMD="bash -c \"'$BIN' sidebar\""
else
  SIDEBAR_CMD="bash '$DIR/scripts/sidebar.sh'"
fi
# For a pane that lives as long as the sidebar does, exec away the shell tmux
# wrapped the command in — bash execs its last command anyway, nu and friends
# do not and would idle there for the pane's whole life. The popup keeps the
# plain form: display-popup -E owns its short-lived child lifecycle.
PANE_CMD="exec $SIDEBAR_CMD"

# mode from arg (bound key) or @agents-mon-display; default split sidebar
mode="${1:-$(tmux show-option -gqv @agents-mon-display)}"
if [ "$mode" = "popup" ] || [ "$mode" = "float" ]; then
  client="${2:-$(bash "$DIR/scripts/client.sh")}" # stable popup owner
  PIN="${TMPDIR:-/tmp}/agents-mon-pin"
  if [ -f "$PIN" ]; then rm -f "$PIN"; exit 0; fi
  touch "$PIN"
  width="$(tmux show-option -gqv @agents-mon-width)"
  height="$(tmux show-option -gqv @agents-mon-height)"
  if [ -z "$height" ]; then
    # fit the fleet: agent row + title row each, session headers, up to 2
    # header rows, popup border; floor 15 keeps the help screen readable
    # ponytail: sized from the last scan cache; first-ever open falls back to 15
    cache="${TMPDIR:-/tmp}/agents-mon-scan-cache"
    if [ -s "$cache" ]; then
      height=$(( $(wc -l < "$cache")
        + $(awk -F'\t' '$6 != "" {n++} END {print n+0}' "$cache")
        + $(cut -f2 "$cache" | cut -d: -f1 | sort -u | wc -l) + 5 ))
      max=$(( $(tmux display-message -p '#{client_height}') - 2 ))
      [ "$height" -gt "$max" ] && height=$max
      [ "$height" -lt 15 ] && height=15
    fi
  fi
  # Navigation Enter jumps (popup reopens over the new window); explicit close
  # removes the pin inside sidebar.sh and ends the loop.
  while [ -f "$PIN" ]; do
    popup_args=(-E -w "${width:-40}" -h "${height:-15}" -e "AGENTS_MON_PIN=$PIN")
    if [ -n "$client" ]; then
      popup_args+=(-c "$client" -e "AGENTS_MON_POPUP_CLIENT=$client")
    fi
    tmux display-popup "${popup_args[@]}" "$SIDEBAR_CMD"
    # popup closed for a jump — the client is free now, actually switch
    if [ -f "$PIN.jump" ]; then
      target="$(cat "$PIN.jump")"; rm -f "$PIN.jump"
      client="$(bash "$DIR/scripts/client.sh")"
      [ -n "$client" ] && tmux switch-client -c "$client" -t "$target" 2>/dev/null
      tmux select-window -t "$target"
      tmux select-pane -t "$target"
    else
      # If the popup command exits without an explicit jump or quit (for
      # example the sidebar/helper process was killed), do not reopen it from
      # the stale pin. The next key trigger will create a fresh popup.
      rm -f "$PIN"
      break
    fi
  done
  exit 0
fi

# Rust engine present: one headless daemon renders directly into processless
# preserved panes. Window switches never reflow ("bump") any layout.
if [ -x "$BIN" ]; then
  # The daemon records its tmux control client. Unlike a PID or a shared temp
  # file, this liveness check is scoped to exactly this tmux server.
  control="$(tmux show-option -gqv @agents-mon-control-client)"
  alive=0
  if [ -n "$control" ] &&
    tmux list-clients -F '#{client_name}' 2>/dev/null | grep -Fxq "$control"; then
    alive=1
  fi
  if [ "$(tmux show-option -gqv @agents-mon-on)" = 1 ] && [ "$alive" = 1 ]; then
    # already open — make sure this window has its preserved pane
    bash "$DIR/scripts/mirror-add.sh"
  else
    bash "$DIR/scripts/teardown.sh"   # clear any crash leftovers
    tmux set-option -g @agents-mon-on 1
    nohup "$BIN" daemon >/dev/null 2>&1 </dev/null &
    while read -r win; do
      bash "$DIR/scripts/mirror-add.sh" "$win"
    done <<EOF
$(tmux list-windows -a -F '#{window_id}')
EOF
    bash "$DIR/scripts/hooks.sh"
  fi
  # An already-running tmux server keeps its live bindings across plugin
  # upgrades. Refresh them once when the navigation contract changes.
  if [ "$(tmux show-option -gqv @agents-mon-nav-version)" != 12 ]; then
    bash "$DIR/scripts/hooks.sh"
  fi
  # Empty panes cannot own stdin. Keep focus in the work pane and route the
  # invoking client through the sidebar's tmux key table instead. Discovering
  # the client is required for old live bindings that supplied no second arg.
  client="${2:-$(bash "$DIR/scripts/client.sh")}"
  if [ -n "$client" ]; then
    # Make navigation visually honest: the native tmux selection border belongs
    # to the sidebar while its client key table owns input.
    win="$(tmux display-message -p -c "$client" '#{window_id}')"
    pane="$(tmux list-panes -t "$win" \
      -f '#{==:#{pane_title},agents-mon}' -F '#{pane_id}' | head -n 1)"
    [ -n "$pane" ] && tmux select-pane -t "$pane"
    tmux switch-client -c "$client" -T agents-mon 2>/dev/null
  fi
  exit 0
fi

# bash fallback: single sidebar pane that follows the active window.
# open if closed, focus if open — close keys inside the sidebar tear it down
cur="$(tmux show-option -gqv @agents-mon-sidebar)"
if [ -n "$cur" ] && tmux list-panes -a -F '#{pane_id}' | grep -qx "$cur"; then
  if [ "$(tmux display-message -p -t "$cur" '#{window_id}')" != "$(tmux display-message -p '#{window_id}')" ]; then
    # sidebar is open elsewhere — bring it to this window first
    bash "$DIR/scripts/hooks.sh"
    bash "$DIR/scripts/follow.sh"
  fi
  tmux select-pane -t "$cur"
else
  width="$(tmux show-option -gqv @agents-mon-width)"
  # save layout so follow.sh can restore pane sizes when the sidebar leaves
  tmux set-option -g "@agents-mon-layout-$(tmux display-message -p '#{window_id}')" "$(tmux display-message -p '#{window_layout}')"
  # -hf: full-height split on the window's left edge
  id="$(tmux split-window -hbf -d -l "${width:-30}" -P -F '#{pane_id}' "$PANE_CMD")"
  tmux set-option -p -t "$id" allow-rename off
  tmux select-pane -t "$id" -T 'agents-mon'
  tmux set-option -g @agents-mon-sidebar "$id"
  tmux set-option -g @agents-mon-sidebar-win "$(tmux display-message -p '#{window_id}')"
  tmux select-pane -t "$id"
  # follow window/session switches
  bash "$DIR/scripts/hooks.sh"
fi

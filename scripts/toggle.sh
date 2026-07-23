#!/usr/bin/env bash
# Toggle the agents view: left-split sidebar (follows window switches)
# or floating popup (set -g @agents-mon-display 'popup'; stays until q/Esc).
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

# mode from arg (bound key) or @agents-mon-display; default split sidebar
mode="${1:-$(tmux show-option -gqv @agents-mon-display)}"
if [ "$mode" = "popup" ] || [ "$mode" = "float" ]; then
  PIN="${TMPDIR:-/tmp}/agents-mon-pin"
  if [ -f "$PIN" ]; then rm -f "$PIN"; exit 0; fi
  touch "$PIN"
  width="$(tmux show-option -gqv @agents-mon-width)"
  height="$(tmux show-option -gqv @agents-mon-height)"
  if [ -z "$height" ]; then
    # fit the fleet: agent row + title row each, session headers, 2 header
    # rows, popup border; floor 15 keeps the help screen readable
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
  # pinned popup: Enter jumps (popup reopens over the new window), q/Esc
  # remove the pin inside sidebar.sh and end the loop
  while [ -f "$PIN" ]; do
    tmux display-popup -E -w "${width:-40}" -h "${height:-15}" \
      "AGENTS_MON_PIN='$PIN' $SIDEBAR_CMD"
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

# Rust engine present: live-mirror mode. One headless daemon renders; every
# window keeps a permanent mirror pane, so window switches never reflow
# ("bump") any layout. q/Esc in any mirror tears the whole thing down.
if [ -x "$BIN" ]; then
  if [ "$(tmux show-option -gqv @agents-mon-on)" = 1 ] && pgrep -qf 'agents-mon daemon'; then
    # already open — make sure this window has a mirror and focus it
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
  pane="$(tmux list-panes -F '#{pane_id}	#{pane_title}' |
    awk -F'\t' '$2 == "agents-mon" { print $1; exit }')"
  [ -n "$pane" ] && tmux select-pane -t "$pane"
  exit 0
fi

# bash fallback: single sidebar pane that follows the active window.
# open if closed, focus if open — only q/Esc inside the sidebar close it
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
  id="$(tmux split-window -hbf -d -l "${width:-30}" -P -F '#{pane_id}' "$SIDEBAR_CMD")"
  tmux set-option -p -t "$id" allow-rename off
  tmux select-pane -t "$id" -T 'agents-mon'
  tmux set-option -g @agents-mon-sidebar "$id"
  tmux set-option -g @agents-mon-sidebar-win "$(tmux display-message -p '#{window_id}')"
  tmux select-pane -t "$id"
  # follow window/session switches
  bash "$DIR/scripts/hooks.sh"
fi

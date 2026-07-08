#!/usr/bin/env bash
# Toggle the agents view: left-split sidebar (follows window switches)
# or floating popup (set -g @agents-mon-display 'popup'; stays until q/Esc).
DIR="$(cd "$(dirname "$0")/.." && pwd)"

# prefer the Rust binary when built; bash sidebar otherwise
BIN="$(tmux show-option -gqv @agents-mon-bin)"
[ -n "$BIN" ] || BIN="$DIR/target/release/agents-mon"
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
  # pinned popup: Enter jumps (popup reopens over the new window), q/Esc
  # remove the pin inside sidebar.sh and end the loop
  while [ -f "$PIN" ]; do
    tmux display-popup -E -w "${width:-40}" -h "${height:-15}" \
      "AGENTS_MON_PIN='$PIN' $SIDEBAR_CMD"
    # popup closed for a jump — the client is free now, actually switch
    if [ -f "$PIN.jump" ]; then
      target="$(cat "$PIN.jump")"; rm -f "$PIN.jump"
      client="$(tmux list-clients -f '#{?#{m:*control-mode*,#{client_flags}},0,1}' -F '#{client_name}' | head -n 1)"
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

# open if closed, focus if open — only q/Esc inside the sidebar close it
cur="$(tmux show-option -gqv @agents-mon-sidebar)"
if [ -n "$cur" ] && tmux list-panes -a -F '#{pane_id}' | grep -qx "$cur"; then
  if [ "$(tmux display-message -p -t "$cur" '#{window_id}')" != "$(tmux display-message -p '#{window_id}')" ]; then
    # sidebar is open elsewhere — bring it to this window first
    tmux set-hook -g 'after-select-window[42]' "run-shell 'bash $DIR/scripts/follow.sh'"
    tmux set-hook -g 'client-session-changed[42]' "run-shell 'bash $DIR/scripts/follow.sh'"
    tmux set-hook -g 'pane-exited[42]' "run-shell 'bash $DIR/scripts/orphan.sh'"
    tmux set-hook -g 'window-pane-changed[42]' "run-shell 'bash $DIR/scripts/orphan.sh'"
    bash "$DIR/scripts/follow.sh"
  fi
  tmux select-pane -t "$cur"
else
  width="$(tmux show-option -gqv @agents-mon-width)"
  # save layout so follow.sh can restore pane sizes when the sidebar leaves
  tmux set-option -g "@agents-mon-layout-$(tmux display-message -p '#{window_id}')" "$(tmux display-message -p '#{window_layout}')"
  # -hf: full-height split on the window's left edge
  id="$(tmux split-window -hbf -d -l "${width:-30}" -P -F '#{pane_id}' "$SIDEBAR_CMD")"
  tmux set-option -g @agents-mon-sidebar "$id"
  tmux select-pane -t "$id"
  # follow window/session switches
  tmux set-hook -g 'after-select-window[42]' "run-shell 'bash $DIR/scripts/follow.sh'"
  tmux set-hook -g 'client-session-changed[42]' "run-shell 'bash $DIR/scripts/follow.sh'"
  tmux set-hook -g 'pane-exited[42]' "run-shell 'bash $DIR/scripts/orphan.sh'"
  tmux set-hook -g 'window-pane-changed[42]' "run-shell 'bash $DIR/scripts/orphan.sh'"
fi

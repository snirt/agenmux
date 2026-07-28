#!/usr/bin/env bash
# End-to-end regression for the preserved sidebar's client key table.
# It intentionally invokes toggle.sh without a client argument, matching old
# live tmux bindings that survive a plugin update until config is reloaded.
set -euo pipefail

DIR="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${AGENTS_MON_BIN:-$DIR/target/release/agents-mon}"
[ -x "$BIN" ] || exit 0
command -v tmux >/dev/null || exit 0
command -v expect >/dev/null || exit 0

tmp="$(mktemp -d "${TMPDIR:-/tmp}/agents-mon-navigation.XXXXXX")"
sock="$tmp/sock"
input="$tmp/client-input"
client_pid=''
input_open=0
cleaned=0

cleanup() {
  [ "$cleaned" -eq 0 ] || return
  cleaned=1
  tmux -S "$sock" kill-server 2>/dev/null || true
  if [ "$input_open" -eq 1 ]; then
    exec 9>&-
  fi
  [ -z "$client_pid" ] || wait "$client_pid" 2>/dev/null || true
  rm -f "$input" "$sock" "$tmp/agents-mon-keys" \
    "$tmp/agents-mon-rows" "$tmp/agents-mon-scan-cache" "$tmp/codex" \
    "$tmp/script.log"
  rmdir "$tmp" 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 130' HUP INT TERM

printf '#!/bin/sh\nwhile :; do sleep 10; done\n' >"$tmp/codex"
chmod +x "$tmp/codex"
mkfifo "$input"

TMPDIR="$tmp" tmux -S "$sock" -f /dev/null new-session \
  -d -s navigation -x 120 -y 32 "$tmp/codex"
tmux -S "$sock" new-window -d -t navigation: "$tmp/codex"
tmux -S "$sock" set-option -g @agents-mon-bin "$BIN"
tmux -S "$sock" set-option -g @agents-mon-width 30
tmux -S "$sock" set-option -g prefix M-a
# The sidebar must behave like a regular pane for the user's root-table
# bindings. This deliberately differs from tmux's defaults so the test proves
# the configured command is inherited rather than hard-coded by the plugin.
tmux -S "$sock" bind-key -n C-l select-pane -R

# Attach through a genuine pseudo-terminal and relay test keys from the FIFO.
# Unlike script(1), expect does not require this test's own stdin to be a tty.
expect -f - "$input" "$sock" >"$tmp/script.log" 2>&1 <<'EXPECT' &
log_user 0
set timeout -1
set input [lindex $argv 0]
set socket [lindex $argv 1]
spawn tmux -S $socket attach-session -t navigation
set tmux_spawn $spawn_id
set keys [open $input r]
fconfigure $keys -blocking 0 -buffering none -translation binary
proc relay_key {} {
  set data [read $::keys]
  if {[string length $data] > 0} {
    send -i $::tmux_spawn -raw -- $data
  }
  if {[eof $::keys]} {
    close $::keys
    exit 0
  }
}
fileevent $keys readable relay_key
expect -i $tmux_spawn eof
EXPECT
client_pid=$!
# Opening the writer after expect starts unblocks its FIFO reader.
exec 9>"$input"
input_open=1

client=''
for _ in $(seq 1 30); do
  client="$(tmux -S "$sock" list-clients \
    -f '#{?#{m:*control-mode*,#{client_flags}},0,1}' \
    -F '#{client_name}' 2>/dev/null | head -n 1)"
  [ -n "$client" ] && break
  sleep 0.1
done
[ -n "$client" ] || {
  echo "FAIL navigation-key-table: no attached client"
  sed -n '1,20p' "$tmp/script.log"
  exit 1
}

server_pid="$(tmux -S "$sock" display-message -p '#{pid}')"
# No client argument is the compatibility case that regressed in production.
env TMPDIR="$tmp" TMUX="$sock,$server_pid,0" bash "$DIR/scripts/toggle.sh"

sidebar=''
first=''
for _ in $(seq 1 40); do
  sidebar="$(tmux -S "$sock" list-panes -t navigation: \
    -F '#{pane_id}	#{pane_title}' |
    awk -F'\t' '$2 == "agents-mon" { print $1; exit }')"
  if [ -n "$sidebar" ]; then
    first="$(tmux -S "$sock" capture-pane -p -t "$sidebar" |
      sed -n '/❯/p' | head -n 1)"
    [ -n "$first" ] && break
  fi
  sleep 0.1
done
[ -n "$first" ] || {
  echo "FAIL navigation-key-table: sidebar did not render a selection"
  exit 1
}

table="$(tmux -S "$sock" display-message -p -c "$client" '#{client_key_table}')"
initial_focus="$(tmux -S "$sock" display-message -p -c "$client" \
  '#{pane_title}')"

# Opening prefix+w from the sidebar zooms that pane while choose-tree is open.
# The temporary full-window width must never be adopted as the user's sidebar
# width by the daemon's border-drag detector.
printf '\033aw' >&9
chooser_open_unzoomed=0
for _ in $(seq 1 20); do
  chooser_state="$(tmux -S "$sock" display-message -p -t "$sidebar" \
    '#{pane_in_mode}/#{window_zoomed_flag}')"
  if [ "$chooser_state" = 1/0 ]; then
    chooser_open_unzoomed=1
    break
  fi
  sleep 0.05
done
sleep 2.5
printf 'q' >&9
for _ in $(seq 1 20); do
  chooser_state="$(tmux -S "$sock" display-message -p -t "$sidebar" \
    '#{pane_in_mode}/#{window_zoomed_flag}')"
  [ "$chooser_state" = 0/0 ] && break
  sleep 0.05
done
chooser_width="$(tmux -S "$sock" show-option -gqv @agents-mon-width)"

# A configured root-table binding must work from the selected sidebar. C-l
# moves right into the work pane and, because it does not re-enter the plugin
# table, leaves the client in the normal root table.
printf '\014' >&9
ctrl_l_works=0
for _ in $(seq 1 20); do
  ctrl_l_table="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{client_key_table}')"
  ctrl_l_focus="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{pane_title}')"
  if [ "$ctrl_l_table" = root ] && [ "$ctrl_l_focus" != agents-mon ]; then
    ctrl_l_works=1
    break
  fi
  sleep 0.05
done
# Re-enter the sidebar so the remaining plugin-navigation checks still start
# from the same state.
tmux -S "$sock" select-pane -t "$sidebar"
for _ in $(seq 1 20); do
  table="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{client_key_table}')"
  [ "$table" = agents-mon ] && break
  sleep 0.05
done

control=''
control_flags=''
for _ in $(seq 1 20); do
  control="$(tmux -S "$sock" show-option -gqv @agents-mon-control-client)"
  control_flags="$(tmux -S "$sock" list-clients \
    -f "#{==:#{client_name},$control}" -F '#{client_flags}' 2>/dev/null)"
  printf '%s' "$control_flags" | grep -Fq control-mode && break
  sleep 0.05
done
printf 'j' >&9
second="$first"
for _ in $(seq 1 20); do
  second="$(tmux -S "$sock" capture-pane -p -t "$sidebar" |
    sed -n '/❯/p' | head -n 1)"
  [ -n "$second" ] && [ "$second" != "$first" ] && break
  sleep 0.1
done
table_after_j="$(tmux -S "$sock" display-message -p -c "$client" '#{client_key_table}')"

printf 'k' >&9
third="$second"
for _ in $(seq 1 20); do
  third="$(tmux -S "$sock" capture-pane -p -t "$sidebar" |
    sed -n '/❯/p' | head -n 1)"
  [ "$third" = "$first" ] && break
  sleep 0.1
done

# Simulate leaving through an agent jump, then selecting the processless
# sidebar exactly as keyboard pane-navigation or a pane picker would.
tmux -S "$sock" switch-client -c "$client" -T root
work="$(tmux -S "$sock" list-panes -t navigation: \
  -F '#{pane_id}	#{pane_title}' |
  awk -F'\t' '$2 != "agents-mon" { print $1; exit }')"
tmux -S "$sock" select-pane -t "$work"
tmux -S "$sock" select-pane -t "$sidebar"
return_table="$(tmux -S "$sock" display-message -p -c "$client" \
  '#{client_key_table}')"
return_focus="$(tmux -S "$sock" display-message -p -c "$client" \
  '#{pane_title}')"
printf 'j' >&9
fourth="$third"
for _ in $(seq 1 20); do
  fourth="$(tmux -S "$sock" capture-pane -p -t "$sidebar" |
    sed -n '/❯/p' | head -n 1)"
  [ -n "$fourth" ] && [ "$fourth" != "$third" ] && break
  sleep 0.1
done
printf 'q' >&9
exit_table=agents-mon
q_left=0
for _ in $(seq 1 20); do
  exit_table="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{client_key_table}')"
  exit_focus="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{pane_title}')"
  sidebar_count="$(tmux -S "$sock" list-panes -a -F '#{pane_title}' \
    2>/dev/null | awk '$0 == "agents-mon" { n++ } END { print n+0 }')"
  if [ "$exit_table" = root ] && [ "$exit_focus" != agents-mon ] \
    && [ "$sidebar_count" -gt 0 ]; then
    q_left=1
    break
  fi
  sleep 0.05
done

# Escape also leaves navigation without closing the preserved sidebars. Reopen
# once and drive a literal escape byte through the attached client so this is
# not only a binding snapshot.
env TMPDIR="$tmp" TMUX="$sock,$server_pid,0" bash "$DIR/scripts/toggle.sh"
escape_ready=0
for _ in $(seq 1 40); do
  escape_table="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{client_key_table}')"
  escape_focus="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{pane_title}')"
  if [ "$escape_table" = agents-mon ] && [ "$escape_focus" = agents-mon ]; then
    escape_ready=1
    break
  fi
  sleep 0.05
done
printf '\033' >&9
escape_left=0
for _ in $(seq 1 20); do
  escape_table="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{client_key_table}')"
  escape_focus="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{pane_title}')"
  sidebar_count="$(tmux -S "$sock" list-panes -a -F '#{pane_title}' \
    2>/dev/null | awk '$0 == "agents-mon" { n++ } END { print n+0 }')"
  if [ "$escape_table" = root ] && [ "$escape_focus" != agents-mon ] \
    && [ "$sidebar_count" -gt 0 ]; then
    escape_left=1
    break
  fi
  sleep 0.05
done

# Uppercase Q is the explicit close action.
env TMPDIR="$tmp" TMUX="$sock,$server_pid,0" bash "$DIR/scripts/toggle.sh"
close_ready=0
for _ in $(seq 1 40); do
  close_table="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{client_key_table}')"
  close_focus="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{pane_title}')"
  if [ "$close_table" = agents-mon ] && [ "$close_focus" = agents-mon ]; then
    close_ready=1
    break
  fi
  sleep 0.05
done
printf 'Q' >&9
q_closed=0
for _ in $(seq 1 20); do
  close_table="$(tmux -S "$sock" display-message -p -c "$client" \
    '#{client_key_table}')"
  sidebar_count="$(tmux -S "$sock" list-panes -a -F '#{pane_title}' \
    2>/dev/null | awk '$0 == "agents-mon" { n++ } END { print n+0 }')"
  if [ "$close_table" = root ] && [ "$sidebar_count" -eq 0 ]; then
    q_closed=1
    break
  fi
  sleep 0.05
done

if [ "$table" = agents-mon ] && [ "$initial_focus" = agents-mon ] \
  && [ "$chooser_open_unzoomed" -eq 1 ] && [ "$chooser_width" = 30 ] \
  && [ "$ctrl_l_works" -eq 1 ] \
  && [ "$table_after_j" = agents-mon ] \
  && printf '%s' "$control_flags" | grep -Fq control-mode \
  && [ "$second" != "$first" ] && [ "$third" = "$first" ] \
  && [ "$return_table" = agents-mon ] && [ "$return_focus" = agents-mon ] \
  && [ "$fourth" != "$third" ] && [ "$exit_table" = root ] \
  && [ "$q_left" -eq 1 ] && [ "$escape_ready" -eq 1 ] \
  && [ "$escape_left" -eq 1 ] && [ "$close_ready" -eq 1 ] \
  && [ "$q_closed" -eq 1 ]; then
  echo "ok   attached-client-jk-navigation"
else
  echo "FAIL navigation-key-table: table=$table initial-focus=[$initial_focus] chooser=[$chooser_open_unzoomed/$chooser_state/$chooser_width] ctrl-l=[$ctrl_l_works/$ctrl_l_table/$ctrl_l_focus] after-j=$table_after_j control=[$control/$control_flags] first=[$first] second=[$second] third=[$third] return=[$return_table/$return_focus] fourth=[$fourth] q-leave=[$q_left/$exit_table/$exit_focus] escape=[$escape_ready/$escape_left/$escape_table/$escape_focus] Q-close=[$close_ready/$q_closed/$close_table]"
  exit 1
fi

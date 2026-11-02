#!/usr/bin/env bash
# A tmux server that stops answering for a few seconds (laptop sleep, heavy
# load) must not look like a server with no sidebars: the daemon has to wait
# it out and keep answering keys.
set -uo pipefail

DIR="$(cd "$(dirname "$0")/.." && pwd)"
. "$DIR/tests/helpers/poll.sh"
. "$DIR/tests/helpers/daemon.sh"
BIN="${AGENMUX_BIN:-$DIR/target/release/agenmux}"
[ -x "$BIN" ] || exit 0
command -v tmux >/dev/null || exit 0

tmp="$(mktemp -d "${TMPDIR:-/tmp}/agenmux-stall.XXXXXX")"
sock="$tmp/sock"
server_pid=''
cleaned=0
cleanup() {
  [ "$cleaned" -eq 0 ] || return
  cleaned=1
  [ -z "$server_pid" ] || kill -CONT "$server_pid" 2>/dev/null || true
  tmux -S "$sock" kill-server 2>/dev/null || true
  rm -rf "$tmp" 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 130' HUP INT TERM

printf '#!/bin/sh\nwhile :; do sleep 10; done\n' >"$tmp/codex"
chmod +x "$tmp/codex"

TMPDIR="$tmp" tmux -S "$sock" -f /dev/null new-session \
  -d -s stall -x 100 -y 30 "$tmp/codex"
tmux -S "$sock" set-option -g @agenmux-bin "$BIN"
tmux -S "$sock" set-option -g @agenmux-width 30
server_pid="$(tmux -S "$sock" display-message -p '#{pid}')"
env TMPDIR="$tmp" TMUX="$sock,$server_pid,0" AGENMUX_DIR="$DIR" \
  "$BIN" toggle split

daemon=''
for _ in $(tries 60); do
  daemon="$(scratch_daemon "$sock")" && break
  sleep 0.1
done
[ -n "$daemon" ] || {
  echo "FAIL daemon-survives-server-stall: no daemon started"
  exit 1
}
# toggle returns only after the daemon's readiness byte, written once it has
# measured a sidebar, so the stall can start right away.

# Long enough for the 2s reply deadline to pass and two periodic mirror
# ticks to run while the pipe is still failing fast.
kill -STOP "$server_pid"
sleep 7
kill -CONT "$server_pid"
# Past the point where a daemon that misread the stall has already exited.
sleep 6

alive=0
kill -0 "$daemon" 2>/dev/null && alive=1
responsive=0
if [ "$alive" -eq 1 ]; then
  env TMPDIR="$tmp" TMUX="$sock,$server_pid,0" "$BIN" key versions
  for _ in $(tries 20); do
    sidebar="$(tmux -S "$sock" list-panes -a -F '#{pane_id} #{pane_title}' |
      awk '$2 == "agenmux" { print $1; exit }')"
    [ -n "$sidebar" ] && tmux -S "$sock" capture-pane -p -t "$sidebar" |
      grep -Eq 'no releases found|↵ switch' && {
      responsive=1
      break
    }
    sleep 0.1
  done
fi
if [ "$alive" -eq 1 ] && [ "$responsive" -eq 1 ]; then
  echo "ok   daemon-survives-server-stall"
else
  echo "FAIL daemon-survives-server-stall: alive=$alive responsive=$responsive"
  exit 1
fi

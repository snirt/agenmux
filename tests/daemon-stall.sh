#!/usr/bin/env bash
# A tmux server that stops answering for a few seconds (laptop sleep, heavy
# load) must not look like a server whose sidebars are gone. Every command
# fails fast until the sync marker returns, and the daemon used to read two of
# those failures as empty mirror ticks and exit, leaving frozen sidebars.
set -uo pipefail

DIR="$(cd "$(dirname "$0")/.." && pwd)"
. "$DIR/tests/helpers/poll.sh"
BIN="${AGENMUX_BIN:-$DIR/target/release/agenmux}"
[ -x "$BIN" ] || exit 0
command -v tmux >/dev/null || exit 0

tmp="$(mktemp -d "${TMPDIR:-/tmp}/agenmux-stall.XXXXXX")"
sock="$tmp/sock"
daemon=''
server_pid=''
cleaned=0
cleanup() {
  [ "$cleaned" -eq 0 ] || return
  cleaned=1
  [ -z "$server_pid" ] || kill -CONT "$server_pid" 2>/dev/null || true
  tmux -S "$sock" kill-server 2>/dev/null || true
  [ -z "$daemon" ] || kill "$daemon" 2>/dev/null || true
  rm -rf "$tmp" 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 130' HUP INT TERM

printf '#!/bin/sh\nwhile :; do sleep 10; done\n' >"$tmp/codex"
chmod +x "$tmp/codex"

before="$(pgrep -f 'agenmux daemon' 2>/dev/null | sort)"

TMPDIR="$tmp" tmux -S "$sock" -f /dev/null new-session \
  -d -s stall -x 100 -y 30 "$tmp/codex"
tmux -S "$sock" set-option -g @agenmux-bin "$BIN"
tmux -S "$sock" set-option -g @agenmux-width 30
server_pid="$(tmux -S "$sock" display-message -p '#{pid}')"
env TMPDIR="$tmp" TMUX="$sock,$server_pid,0" AGENMUX_DIR="$DIR" \
  "$BIN" toggle split

for _ in $(tries 60); do
  after="$(pgrep -f 'agenmux daemon' 2>/dev/null | sort)"
  daemon="$(comm -13 <(printf '%s\n' "$before") <(printf '%s\n' "$after") | head -n 1)"
  [ -n "$daemon" ] && break
  sleep 0.1
done
[ -n "$daemon" ] || {
  echo "FAIL daemon-survives-server-stall: no daemon started"
  exit 1
}
sleep 3

# Long enough for the 2s reply deadline to pass and two periodic mirror
# ticks to run while the pipe is still failing fast.
kill -STOP "$server_pid"
sleep 7
kill -CONT "$server_pid"
sleep 6

alive=0
kill -0 "$daemon" 2>/dev/null && alive=1
mirrors="$(tmux -S "$sock" list-panes -a -F '#{pane_title}' 2>/dev/null | grep -c '^agenmux$')"
if [ "$alive" -eq 1 ] && [ "$mirrors" -ge 1 ]; then
  echo "ok   daemon-survives-server-stall"
else
  echo "FAIL daemon-survives-server-stall: alive=$alive mirrors=$mirrors"
  exit 1
fi

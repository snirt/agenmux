#!/usr/bin/env bash
# A superseded daemon must exit on its own, or orphans pile up forever.
# The panes stay put and only the control-client option changes, so nothing
# except the ownership check can end the process.
set -uo pipefail

DIR="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${AGENTS_MON_BIN:-$DIR/target/release/agents-mon}"
[ -x "$BIN" ] || exit 0
command -v tmux >/dev/null || exit 0

tmp="$(mktemp -d "${TMPDIR:-/tmp}/agents-mon-orphan.XXXXXX")"
sock="$tmp/sock"
daemon=''
cleaned=0
cleanup() {
  [ "$cleaned" -eq 0 ] || return
  cleaned=1
  tmux -S "$sock" kill-server 2>/dev/null || true
  [ -z "$daemon" ] || kill "$daemon" 2>/dev/null || true
  rm -rf "$tmp" 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 130' HUP INT TERM

printf '#!/bin/sh\nwhile :; do sleep 10; done\n' >"$tmp/codex"
chmod +x "$tmp/codex"

# only pids appearing after this are ours: other tmux servers run daemons too
before="$(pgrep -f 'agents-mon daemon' 2>/dev/null | sort)"

TMPDIR="$tmp" tmux -S "$sock" -f /dev/null new-session \
  -d -s orphan -x 100 -y 30 "$tmp/codex"
tmux -S "$sock" set-option -g @agents-mon-bin "$BIN"
tmux -S "$sock" set-option -g @agents-mon-width 30
server_pid="$(tmux -S "$sock" display-message -p '#{pid}')"
env TMPDIR="$tmp" TMUX="$sock,$server_pid,0" AGENTS_MON_DIR="$DIR" \
  "$BIN" toggle split

for _ in $(seq 1 60); do
  after="$(pgrep -f 'agents-mon daemon' 2>/dev/null | sort)"
  daemon="$(comm -13 <(printf '%s\n' "$before") <(printf '%s\n' "$after") | head -n 1)"
  [ -n "$daemon" ] && break
  sleep 0.1
done
[ -n "$daemon" ] || {
  echo "FAIL daemon-orphan-exits: no daemon started"
  exit 1
}

# without this the test would also pass if the check killed every daemon
sleep 4
survives_when_current=0
kill -0 "$daemon" 2>/dev/null && survives_when_current=1

env TMPDIR="$tmp" TMUX="$sock,$server_pid,0" "$BIN" key versions || {
  echo "FAIL daemon-orphan-exits: could not open version picker"
  exit 1
}
picker_open=0
for _ in $(seq 1 20); do
  sidebar="$(tmux -S "$sock" list-panes -a -F '#{pane_id} #{pane_title}' |
    awk '$2 == "agents-mon" { print $1; exit }')"
  if [ -n "$sidebar" ] && tmux -S "$sock" capture-pane -p -t "$sidebar" |
    grep -Fq 'agents — versions'; then
    picker_open=1
    break
  fi
  sleep 0.1
done
[ "$picker_open" -eq 1 ] || {
  echo "FAIL daemon-orphan-exits: version picker did not render"
  exit 1
}
tmux -S "$sock" set-option -g @agents-mon-control-client client-not-ours
exits_when_replaced=0
for _ in $(seq 1 60); do
  kill -0 "$daemon" 2>/dev/null || { exits_when_replaced=1; break; }
  sleep 0.2
done

if [ "$survives_when_current" -eq 1 ] && [ "$exits_when_replaced" -eq 1 ]; then
  echo "ok   daemon-orphan-exits"
else
  echo "FAIL daemon-orphan-exits: survives-while-current=$survives_when_current exits-when-replaced=$exits_when_replaced"
  exit 1
fi

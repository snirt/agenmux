#!/usr/bin/env bash
# Event-driven scanner regression using only a private tmux socket and
# synthetic panes. No user's server, configuration, or agent API is touched.
set -euo pipefail

DIR="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${AGENMUX_BIN:-$DIR/target/release/agenmux}"
[ -x "$BIN" ] || { echo "SKIP polling: release binary missing"; exit 0; }
command -v tmux >/dev/null || { echo "SKIP polling: tmux missing"; exit 0; }

tmp="$(mktemp -d "${TMPDIR:-/tmp}/agenmux-polling.XXXXXX")"
sock="$tmp/tmux.sock"
debug="$tmp/debug.log"
cache="$tmp/agenmux-scan-cache"
export XDG_CONFIG_HOME="$tmp/config"
mkdir -p "$XDG_CONFIG_HOME/agenmux/agents"

cleanup() {
  tmux -S "$sock" kill-server 2>/dev/null || true
  rm -rf "$tmp"
}
trap cleanup EXIT
trap 'exit 130' HUP INT TERM

cat >"$XDG_CONFIG_HOME/agenmux/agents/synthetic.conf" <<'CONF'
AGENT_BINS="sh"
WORKING_SCREEN='^WORKING'
IDLE_SCREEN='^IDLE'
CHECK_ORDER="ws is"
CONF

# The shell only emits on state changes, except stream which deliberately stays
# busy. This keeps the pane command and title constant across transitions.
cat >"$tmp/producer.sh" <<'PRODUCER'
state=
while :; do
  next="$(sed -n '1p' "$1" 2>/dev/null)"
  if [ "$next" != "$state" ]; then
    state="$next"
    case "$state" in
      idle) printf '\033[H\033[2JIDLE\n%%end 1 2\n' ;;
      working) printf '\033[H\033[2JWORKING\n%%end 1 2\n' ;;
    esac
  fi
  if [ "$state" = stream ]; then
    printf '\033[H\033[2JWORKING continuous\n%%end 1 2\n'
  fi
  sleep .05
done
PRODUCER
printf 'idle\n' >"$tmp/state-primary"
TMPDIR="$tmp" tmux -S "$sock" -f /dev/null new-session -d -s primary -x 100 -y 30 \
  "/bin/sh '$tmp/producer.sh' '$tmp/state-primary'"
primary="$(tmux -S "$sock" display-message -p -t primary: '#{pane_id}')"
tmux -S "$sock" select-pane -t "$primary" -T constant-title
server_pid="$(tmux -S "$sock" display-message -p '#{pid}')"

# Launch as a tmux job so the daemon outlives this harness's command runner.
tmux -S "$sock" run-shell -b \
  "env TMPDIR='$tmp' XDG_CONFIG_HOME='$XDG_CONFIG_HOME' TMUX='$sock,$server_pid,0' AGENMUX_DIR='$DIR' AGENMUX_DEBUG='$debug' '$BIN' daemon"

state_count() {
  awk -F '\t' -v state="$1" '$4 == state { n++ } END { print n + 0 }' "$cache" 2>/dev/null || printf '0\n'
}

wait_count() {
  local state="$1" expected="$2" label="$3"
  for _ in $(seq 1 60); do
    [ "$(state_count "$state")" = "$expected" ] && return 0
    sleep .1
  done
  echo "FAIL polling: $label (wanted $expected $state rows)"
  printf '%s\n' "runtime cache:"
  sed -n '1,20p' "$cache" 2>/dev/null || true
  printf '%s\n' "tmux panes:"
  tmux -S "$sock" list-panes -a -F '#{pane_id} #{pane_current_command} #{session_name}' 2>/dev/null || true
  sed -n '1,80p' "$debug" 2>/dev/null || true
  exit 1
}

wait_count idle 1 initial-discovery
TMPDIR="$tmp" TMUX="$sock,$server_pid,0" "$BIN" status | grep -Fq '#[fg=green]⣿#[default]1'

# Covered quiet panes should reuse their screen at periodic tracker ticks.
sleep 3
grep -Eq 'captured=0 reused=1' "$debug" || {
  echo "FAIL polling: unchanged covered pane was not reused"
  exit 1
}

# A constant-title, output-only change must trigger detection before the next
# periodic reconciliation, while marker-shaped screen text stays off protocol.
printf 'working\n' >"$tmp/state-primary"
wait_count working 1 output-only-working

# Continuous output cannot keep moving the dirty deadline. The final quiet
# redraw must also be captured, then Tracker's periodic tick settles idle.
printf 'stream\n' >"$tmp/state-primary"
sleep 2
[ "$(state_count working)" = 1 ] || { echo "FAIL polling: continuous output"; exit 1; }
stream_scans="$(grep -c 'scan .*captured=1' "$debug" || true)"
[ "$stream_scans" -ge 2 ] || { echo "FAIL polling: busy output postponed scans"; exit 1; }
printf 'idle\n' >"$tmp/state-primary"
wait_count idle 1 final-idle

# A background session is outside output coverage and must be discovered and
# refreshed by the ordinary two-second scan cadence.
printf 'idle\n' >"$tmp/state-background"
tmux -S "$sock" new-session -d -s background -x 90 -y 25 \
  "/bin/sh '$tmp/producer.sh' '$tmp/state-background'"
background="$(tmux -S "$sock" display-message -p -t background: '#{pane_id}')"
tmux -S "$sock" select-pane -t "$background" -T constant-title
wait_count idle 2 background-discovery
printf 'working\n' >"$tmp/state-background"
wait_count working 1 background-working-fallback

# Switching the monitoring client invalidates coverage. Resize then removal
# exercise metadata refresh and stale cache pruning.
control="$(tmux -S "$sock" show-option -gqv @agenmux-control-client)"
[ -n "$control" ] || { echo "FAIL polling: daemon did not publish client"; exit 1; }
tmux -S "$sock" switch-client -c "$control" -t background
sleep .7
tmux -S "$sock" resize-pane -t "$background" -x 70
before_resize="$(grep -c 'scan .*captured=' "$debug" || true)"
sleep 2.3
after_resize="$(grep -c 'scan .*captured=' "$debug" || true)"
[ "$after_resize" -gt "$before_resize" ] || { echo "FAIL polling: resize not reconciled"; exit 1; }
# Keep the attached session alive while removing the monitored pane; destroying
# a control client's entire attached session legitimately detaches that client.
tmux -S "$sock" split-window -d -t background: "tail -f /dev/null"
tmux -S "$sock" kill-pane -t "$background"
wait_count working 0 pane-removal
wait_count idle 1 pane-removal-survivor

echo "ok   polling-event-driven-cache-and-reconciliation"

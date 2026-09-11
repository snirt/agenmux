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

debug_lines() {
  wc -l <"$debug" 2>/dev/null | tr -d ' '
}

debug_since() {
  local checkpoint="$1"
  sed -n "$((checkpoint + 1)),\$p" "$debug" 2>/dev/null
}

wait_debug_since() {
  local checkpoint="$1" pattern="$2" label="$3"
  for _ in $(seq 1 30); do
    debug_since "$checkpoint" | grep -Eq "$pattern" && return 0
    sleep .1
  done
  echo "FAIL polling: $label"
  debug_since "$checkpoint" | sed -n '1,80p'
  exit 1
}

wait_count idle 1 initial-discovery
TMPDIR="$tmp" TMUX="$sock,$server_pid,0" "$BIN" status | grep -Fq '#[fg=green]⣿#[default]1'

# Covered quiet panes should reuse their screen at periodic tracker ticks.
sleep 3
grep -Eq 'reason=periodic captured=0 reused=1' "$debug" || {
  echo "FAIL polling: unchanged covered pane was not reused"
  exit 1
}

# A constant-title, output-only change must trigger detection before the next
# periodic reconciliation, while marker-shaped screen text stays off protocol.
# Anchor immediately after a periodic scan, then require an output-scheduled
# capture. A detector result alone could otherwise arrive on the 2s fallback.
periodic_checkpoint="$(debug_lines)"
wait_debug_since "$periodic_checkpoint" 'reason=periodic' periodic-anchor
working_checkpoint="$(debug_lines)"
printf 'working\n' >"$tmp/state-primary"
wait_debug_since "$working_checkpoint" 'reason=output captured=1' output-event-capture
wait_count working 1 output-only-working

# Continuous output cannot keep moving the dirty deadline. The final quiet
# redraw must also be captured, then Tracker's periodic tick settles idle.
stream_checkpoint="$(debug_lines)"
printf 'stream\n' >"$tmp/state-primary"
stream_scans=0
for _ in $(seq 1 25); do
  stream_scans="$(debug_since "$stream_checkpoint" |
    grep -Ec 'reason=output captured=1' || true)"
  [ "$stream_scans" -ge 2 ] && break
  sleep .1
done
[ "$(state_count working)" = 1 ] || { echo "FAIL polling: continuous output"; exit 1; }
[ "$stream_scans" -ge 2 ] || { echo "FAIL polling: busy output postponed scans"; exit 1; }
# Stop immediately after a periodic boundary so the final redraw has ample
# room to prove it was captured by the output deadline, not the fallback.
periodic_checkpoint="$(debug_lines)"
wait_debug_since "$periodic_checkpoint" 'reason=periodic' final-periodic-anchor
idle_checkpoint="$(debug_lines)"
printf 'idle\n' >"$tmp/state-primary"
wait_debug_since "$idle_checkpoint" 'reason=output captured=1' final-output-capture
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
# A single pane always fills its window and cannot be resized. Keep a plain
# companion so both the dimension change and later monitored-pane removal are
# real operations without introducing another detected agent.
tmux -S "$sock" split-window -d -h -t background: "tail -f /dev/null"
old_width="$(tmux -S "$sock" display-message -p -t "$background" '#{pane_width}')"
resize_width=$((old_width > 20 ? old_width - 5 : old_width + 5))
resize_checkpoint="$(debug_lines)"
tmux -S "$sock" resize-pane -t "$background" -x "$resize_width"
new_width="$(tmux -S "$sock" display-message -p -t "$background" '#{pane_width}')"
[ "$new_width" != "$old_width" ] || { echo "FAIL polling: pane did not resize"; exit 1; }
wait_debug_since "$resize_checkpoint" "capture-pane .* -t '$background'" \
  resize-pane-specific-capture
# Keep the attached session alive while removing the monitored pane; destroying
# a control client's entire attached session legitimately detaches that client.
tmux -S "$sock" kill-pane -t "$background"
wait_count working 0 pane-removal
wait_count idle 1 pane-removal-survivor

echo "ok   polling-event-driven-cache-and-reconciliation"

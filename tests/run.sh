#!/usr/bin/env bash
# Asserts detect_state over fixtures: tests/fixtures/<agent>-<state>[-x].txt
# Optional <fixture>.title sidecar supplies the pane title.
DIR="$(cd "$(dirname "$0")/.." && pwd)"
fail=0 count=0

# fixtures run against bash, and against the Rust engine when built —
# both stay honest
engines="bash"
BIN="${AGENTS_MON_BIN:-$DIR/target/release/agents-mon}"
[ -x "$BIN" ] && engines="bash rust"

for engine in $engines; do
  for fx in "$DIR"/tests/fixtures/*.txt; do
    base="$(basename "$fx" .txt)"
    name="$base"
    case "${name##*-}" in
      ''|*[!0-9]*) ;;
      *) name="${name%-*}" ;;
    esac
    agent="${name%-*}"
    expected="${name##*-}"
    title=""
    [ -f "${fx%.txt}.title" ] && title="$(cat "${fx%.txt}.title")"
    if [ "$engine" = rust ]; then
      got="$("$BIN" detect "$DIR/agents/$agent.conf" "$fx" "$title")"
    else
      got="$(bash "$DIR/scripts/scan.sh" detect "$DIR/agents/$agent.conf" "$fx" "$title")"
    fi
    count=$((count + 1))
    if [ "$got" = "$expected" ]; then
      echo "ok   $base ($engine)"
    else
      echo "FAIL $base ($engine): expected $expected, got $got"
      fail=1
    fi
  done
done

echo "$count fixtures"
if [ "$fail" -eq 0 ]; then
  tmp="$(mktemp -d)"
  mkdir -p "$tmp/bin"
  cat > "$tmp/bin/tmux" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$TMUX_STUB_LOG"
case "$1 $2 $3" in
  "show-option -gqv @agents-mon-key") printf 'E\n' ;;
  "show-option -gqv @agents-mon-popup-key") printf 'e\n' ;;
esac
exit 0
SH
  chmod +x "$tmp/bin/tmux"
  TMUX_STUB_LOG="$tmp/tmux.log" PATH="$tmp/bin:$PATH" bash "$DIR/agents-mon.tmux"
  if grep -q "^bind-key E run-shell -b " "$tmp/tmux.log" \
     && grep -q "^bind-key e run-shell -b " "$tmp/tmux.log"; then
    echo "ok   entrypoint-binds-toggle-in-background"
  else
    echo "FAIL entrypoint-binds-toggle-in-background: popup toggle would block tmux"
    cat "$tmp/tmux.log"
    fail=1
  fi
  rm -rf "$tmp"
fi
if [ "$fail" -eq 0 ]; then
  tmp="$(mktemp -d)"
  mkdir -p "$tmp/bin"
  cat > "$tmp/bin/tmux" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$TMUX_STUB_LOG"
case "$*" in
  "show-option -gqv @agents-mon-sidebar") printf '%%99\n' ;;
  "display-message -p -t %99 #{window_id}") printf '@sb\n' ;;
  "list-panes -t @sb -F x") printf 'x\n' ;;
  "display-message -p -t %99 #{session_id}") printf 's1\n' ;;
  "list-clients "*"-F #{client_name}") printf 'c1\n' ;;
  "display-message -p -c c1 #{window_id}") printf '@other\n' ;;
esac
exit 0
SH
  chmod +x "$tmp/bin/tmux"
  TMUX_STUB_LOG="$tmp/tmux.log" PATH="$tmp/bin:$PATH" bash "$DIR/scripts/orphan.sh"
  if grep -Eq '^(switch-client|last-window|next-window)' "$tmp/tmux.log"; then
    echo "FAIL orphan-does-not-move-unstranded-client: moved focus from another window"
    cat "$tmp/tmux.log"
    fail=1
  else
    echo "ok   orphan-does-not-move-unstranded-client"
  fi
  rm -rf "$tmp"
fi
if [ "$fail" -eq 0 ]; then
  tmp="$(mktemp -d)"
  mkdir -p "$tmp/bin"
  cat > "$tmp/bin/tmux" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$TMUX_STUB_LOG"
case "$*" in
  "show-option -gqv @agents-mon-sidebar") printf '%%99\n' ;;
  "display-message -p -t %99 #{window_id}") printf '@sb\n' ;;
  "list-panes -t @sb -F x") printf 'x\n' ;;
  "display-message -p -t %99 #{session_id}") printf 's1\n' ;;
  "list-clients "*"-F #{client_name}") printf 'c1\n' ;;
  "display-message -p -c c1 #{window_id}") printf '@sb\n' ;;
  "list-windows -t s1 -F #{window_id}\t#{window_last_flag}") printf '@sb\t0\n@last\t1\n' ;;
  "list-windows -t s1 -F #{window_id}") printf '@sb\n@last\n' ;;
esac
exit 0
SH
  chmod +x "$tmp/bin/tmux"
  TMUX_STUB_LOG="$tmp/tmux.log" PATH="$tmp/bin:$PATH" bash "$DIR/scripts/orphan.sh"
  if grep -q '^switch-client -c c1 -t @last$' "$tmp/tmux.log" \
     && ! grep -Eq '^(last-window|next-window|switch-client -l|switch-client -p)' "$tmp/tmux.log"; then
    echo "ok   orphan-moves-only-stranded-client"
  else
    echo "FAIL orphan-moves-only-stranded-client: did not target stranded client safely"
    cat "$tmp/tmux.log"
    fail=1
  fi
  rm -rf "$tmp"
fi
if [ "$fail" -eq 0 ]; then
  tmp="$(mktemp -d)"
  mkdir -p "$tmp/bin"
  cat > "$tmp/bin/tmux" <<'SH'
#!/usr/bin/env bash
case "$1" in
  show-option) exit 0 ;;
  display-popup) exit 0 ;;
  *) exit 0 ;;
esac
SH
  chmod +x "$tmp/bin/tmux"
  TMPDIR="$tmp" PATH="$tmp/bin:$PATH" bash "$DIR/scripts/toggle.sh" popup &
  pid=$!
  waited=0
  while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt 20 ]; do
    sleep 0.05
    waited=$((waited + 1))
  done
  if kill -0 "$pid" 2>/dev/null; then
    echo "FAIL popup-exits-when-helper-exits: toggle loop kept stale pin"
    kill "$pid" 2>/dev/null
    wait "$pid" 2>/dev/null
    fail=1
  else
    wait "$pid"
    echo "ok   popup-exits-when-helper-exits"
  fi
  rm -rf "$tmp"
fi
if [ "$fail" -eq 0 ]; then
  tmp="$(mktemp -d)"
  mkdir -p "$tmp/bin"
  cat > "$tmp/bin/tmux" <<'SH'
#!/usr/bin/env bash
if [ "$1" = "kill-pane" ]; then
  printf '%s\n' "$*" >> "$TMUX_STUB_LOG"
fi
exit 0
SH
  chmod +x "$tmp/bin/tmux"
  touch "$tmp/pin"
  TMUX_STUB_LOG="$tmp/tmux.log" TMPDIR="$tmp" PATH="$tmp/bin:$PATH" \
    AGENTS_MON_PIN="$tmp/pin" TMUX_PANE="%%99" bash "$DIR/scripts/sidebar.sh" >/dev/null 2>&1 &
  pid=$!
  sleep 0.1
  kill -TERM "$pid" 2>/dev/null || true
  waited=0
  while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt 20 ]; do
    sleep 0.05
    waited=$((waited + 1))
  done
  if [ -e "$tmp/pin" ]; then
    echo "FAIL popup-sidebar-signal-removes-pin: stale popup pin remained"
    fail=1
  elif grep -q 'kill-pane' "$tmp/tmux.log" 2>/dev/null; then
    echo "FAIL popup-sidebar-signal-removes-pin: popup cleanup created a real pane"
    fail=1
  else
    echo "ok   popup-sidebar-signal-removes-pin"
  fi
  kill "$pid" 2>/dev/null
  wait "$pid" 2>/dev/null
  rm -rf "$tmp"
fi
if [ "$fail" -eq 0 ]; then
  tmp="$(mktemp -d)"
  mkdir -p "$tmp/bin"
  cat > "$tmp/bin/tmux" <<'SH'
#!/usr/bin/env bash
exit 0
SH
  chmod +x "$tmp/bin/tmux"
  touch "$tmp/pin"
  printf '\004' | TMPDIR="$tmp" PATH="$tmp/bin:$PATH" \
    AGENTS_MON_PIN="$tmp/pin" TMUX_PANE="%%99" bash "$DIR/scripts/sidebar.sh" >/dev/null 2>&1
  if [ -e "$tmp/pin" ]; then
    echo "FAIL popup-sidebar-ctrl-d-removes-pin: stale popup pin remained"
    fail=1
  else
    echo "ok   popup-sidebar-ctrl-d-removes-pin"
  fi
  rm -rf "$tmp"
fi
exit $fail

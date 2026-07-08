#!/usr/bin/env bash
# Sidebar — runs inside the sidebar pane.
# Keys: j/k or arrows move selection, Enter jumps to agent, ? help, q closes.
DIR="$(cd "$(dirname "$0")/.." && pwd)"
STATE_FILE="${TMPDIR:-/tmp}/agents-mon-$$.state"
export AGENTS_MON_SELF="${TMUX_PANE:-}"
# row map for click-to-jump: line N of this file = agent row N in the sidebar
ROWS_FILE="${TMPDIR:-/tmp}/agents-mon-rows-${TMUX_PANE#%}"

SCAN_FILE="$STATE_FILE.scan"
# last scan survives across instances so a fresh popup renders instantly
CACHE_FILE="${TMPDIR:-/tmp}/agents-mon-scan-cache"

cleanup() {
  printf '\033[?25h'
  [ -n "${scan_pid:-}" ] && kill "$scan_pid" 2>/dev/null
  rm -f "$STATE_FILE" "$ROWS_FILE" "$SCAN_FILE" "$SCAN_FILE.partial"
  if [ -n "${AGENTS_MON_PIN:-}" ] && [ ! -f "$AGENTS_MON_PIN.jump" ]; then
    rm -f "$AGENTS_MON_PIN"
  fi
  exit 0
}
trap cleanup INT TERM EXIT
# resize rewraps the old frame into garbage; clear now — the signal also
# interrupts the key-loop read, so the next render comes instantly
trap 'printf "\033[2J"; force_render=1' WINCH
force_render=1

printf '\033[?25l\033[2J'
: > "$STATE_FILE"

E=$'\033'
C_C=$'\003'
C_D=$'\004'
NL=$'\n'
# arrow keys deliver their bytes together; only a bare Esc hits this timeout.
# bash >=4 can wait 50ms — old bash 3.2 is stuck with 1s (integer-only -t)
if [ "${BASH_VERSINFO[0]}" -ge 4 ]; then ESC_WAIT=0.05 READ_WAIT=0.25; else ESC_WAIT=1 READ_WAIT=1; fi
debounced=""
nrows=0
sel=1
SPIN='⠹⢸⣰⣤⣆⡇⠏⠛'   # 4-dot snake, clockwise; done = ⣿ (spinner complete)
tick=0
sel_pane=""  # selection sticks to this pane across rescans until moved
last_active=""

sync_sel_pane() { # remember which pane the cursor is on
  sel_pane="$(printf '%s' "$debounced" | awk -F'\t' -v n="$sel" 'NR == n { print $1 }')"
}

restore_sel() { # after a rescan, follow the remembered pane's new position
  local idx
  [ -n "$sel_pane" ] || { sync_sel_pane; return; }
  idx="$(printf '%s' "$debounced" | awk -F'\t' -v p="$sel_pane" '$1 == p { print NR; exit }')"
  if [ -n "$idx" ]; then
    sel="$idx"
  else
    [ "$sel" -gt "$nrows" ] && sel=$nrows
    [ "$sel" -lt 1 ] && sel=1
    sync_sel_pane
  fi
}

color_dot() { # sets $dot — no subshell, render runs hot
  case "$1" in
    blocked) # blink on/off every 2 ticks (~0.5s)
      if [ $(( tick / 2 % 2 )) -eq 0 ]; then dot="$E[31m⣿$E[0m"; else dot=" "; fi ;;
    working) dot="$E[33m${SPIN:tick % 8:1}$E[0m" ;;
    done) # finished, not viewed yet — blink green
      if [ $(( tick / 2 % 2 )) -eq 0 ]; then dot="$E[32m⣿$E[0m"; else dot=" "; fi ;;
    *)       dot="$E[32m⣿$E[0m" ;;
  esac
}

# scans run in the background so the key loop stays responsive
scan_pid=""
last_scan_start=0
start_scan() {
  { # remember current width so follow.sh can restore it after the sidebar
    # gets orphaned full-width (skip popup mode and the full-width state)
    if [ -z "${AGENTS_MON_PIN:-}" ] && [ -n "${TMUX_PANE:-}" ]; then
      w="$(tmux display-message -p -t "$TMUX_PANE" '#{pane_width}' 2>/dev/null)"
      ww="$(tmux display-message -p -t "$TMUX_PANE" '#{window_width}' 2>/dev/null)"
      [ -n "$w" ] && [ "$w" != "$ww" ] && tmux set-option -g @agents-mon-last-width "$w"
    fi
    bash "$DIR/scripts/scan.sh" list > "$SCAN_FILE.partial" 2>/dev/null \
      && mv "$SCAN_FILE.partial" "$SCAN_FILE"; } &
  scan_pid=$!
  last_scan_start=$SECONDS
}

scan_tick() { # consume a finished background scan from $SCAN_FILE
  local scan pane loc agent state cwd title prev prev_state ticks show new_state_file active
  scan="$(<"$SCAN_FILE")"
  rm -f "$SCAN_FILE"
  printf '%s\n' "$scan" > "$CACHE_FILE"
  active="$(tmux display-message -p -t "$(tmux list-clients -F '#{session_id}' | head -n 1)" '#{pane_id}' 2>/dev/null)"

  # idle debounce: show idle only after 2 consecutive idle ticks (redraws
  # flash idle-looking frames mid-render — ccmanager lesson)
  debounced=""
  new_state_file=""
  nrows=0
  while IFS=$'\t' read -r pane loc agent state cwd title; do
    [ -n "$pane" ] || continue
    prev="$(grep "^$pane " "$STATE_FILE" 2>/dev/null)"
    prev_state="$(printf '%s' "$prev" | awk '{print $2}')"
    ticks="$(printf '%s' "$prev" | awk '{print $3}')"
    # agents like codex only title the pane while working — keep last subject
    [ -z "$title" ] && title="$(printf '%s' "$prev" | cut -d' ' -f4-)"
    show="$state"
    if [ "$state" = "idle" ] && [ -n "$prev_state" ] && [ "$prev_state" != "idle" ] \
       && [ "$prev_state" != "done" ] && [ "${ticks:-0}" -lt 1 ]; then
      # debounce: hold the previous state one tick before trusting idle
      show="$prev_state"
      new_state_file="$new_state_file$pane $prev_state $(( ${ticks:-0} + 1 )) $title$NL"
    elif [ "$state" = "idle" ] && [ "$pane" != "$active" ] \
         && { [ "$prev_state" = "working" ] || [ "$prev_state" = "done" ]; }; then
      # finished while unfocused — flag as done until the pane is viewed
      show="done"
      new_state_file="$new_state_file$pane done 0 $title$NL"
    else
      new_state_file="$new_state_file$pane $state 0 $title$NL"
    fi
    debounced="$debounced$pane	$loc	$agent	$show	$cwd	$title$NL"
    nrows=$((nrows + 1))
  done <<EOF
$scan
EOF
  printf '%s' "$new_state_file" > "$STATE_FILE"
  [ "$sel" -gt "$nrows" ] && sel=$nrows
  [ "$sel" -lt 1 ] && sel=1
  restore_sel
}

render() {
  local frame n=0 pane loc agent state cwd title mark cols rows cap used rest avail
  local client active idx
  # tput can report the client size, not the pane's — ask tmux directly
  IFS=' ' read -r cols rows <<EOF
$(tmux display-message -p -t "${TMUX_PANE:-}" '#{pane_width} #{pane_height}' 2>/dev/null)
EOF
  [ -n "$cols" ] || cols="$(tput cols 2>/dev/null)"; cols="${cols:-30}"
  [ -n "$rows" ] || rows="$(tput lines 2>/dev/null)"; rows="${rows:-24}"
  cap=$((rows - 1))  # writing the last row's newline would scroll the pane
  # single cursor: when focus lands on an agent pane, the cursor snaps to it;
  # otherwise it stays where j/k left it
  # active pane of the client's current session (session id is target-safe
  # even when session names contain spaces/colons)
  client="$(tmux list-clients -F '#{session_id}' | head -n 1)"
  active="$(tmux display-message -p -t "$client" '#{pane_id}' 2>/dev/null)"
  if [ -n "$active" ] && [ "$active" != "$last_active" ]; then
    idx="$(printf '%s' "$debounced" | awk -F'\t' -v p="$active" '$1 == p { print NR; exit }')"
    if [ -n "$idx" ]; then sel="$idx"; sel_pane="$active"; fi
    last_active="$active"
  fi
  frame="$E[H$E[1magents$E[0m$E[K$NL$E[K$NL"
  # rows file mirrors visual lines from y=2 so clicks map 1:1 ("-" = header)
  local vis="" session="" used=2  # header + blank line already emitted
  if [ -z "$debounced" ]; then
    frame="$frame$E[2mno agents$E[0m$E[K$NL"
  else
    while IFS=$'\t' read -r pane loc agent state cwd title; do
      [ -n "$pane" ] || continue
      if [ "${loc%%:*}" != "$session" ]; then
        [ $((used + 2)) -gt "$cap" ] && break  # no room for header + record
        session="${loc%%:*}"
        frame="$frame$E[1;34m$session$E[0m$E[K$NL"
        vis="$vis-$NL"
        used=$((used + 1))
      fi
      [ "$used" -ge "$cap" ] && break  # pane full — clip, never scroll
      n=$((n + 1))
      if [ "$n" = "$sel" ]; then mark="$E[1m❯$E[0m "; else mark="  "; fi
      color_dot "$state"
      rest="${loc#*:} $cwd"                      # window.pane + dir
      avail=$((cols - 5 - ${#agent}))            # "❯ ● name " prefix
      [ "$avail" -gt 0 ] && rest="${rest:0:$avail}"
      frame="$frame$mark$dot $E[1m$agent$E[0m $E[2m$rest$E[0m$E[K$NL"
      vis="$vis$pane$NL"
      used=$((used + 1))
      if [ -n "$title" ] && [ "$used" -lt "$cap" ]; then  # subject line under the record
        frame="$frame    $E[2m${title:0:cols - 4}$E[0m$E[K$NL"
        vis="$vis$pane$NL"
        used=$((used + 1))
      fi
    done <<EOF
$debounced
EOF
  fi
  printf '%s' "$vis" > "$ROWS_FILE"
  printf '%s' "$frame$E[J"
}

jump() {
  local target client
  target="$(printf '%s' "$debounced" | awk -F'\t' -v n="$sel" 'NR == n { print $1 }')"
  case "$target" in %*) ;; *) return ;; esac
  if [ -n "${AGENTS_MON_PIN:-}" ]; then
    # popup holds the client — switch-client would fail cross-session.
    # Hand the target to toggle.sh, which jumps after the popup closes.
    printf '%s' "$target" > "$AGENTS_MON_PIN.jump"
    exit 0
  fi
  # move the sidebar into the target window BEFORE switching the view — the
  # join-pane reflow happens off-screen, so no flash/bump on arrival. The
  # select-window/switch-client hooks then no-op (sidebar already there).
  bash "$DIR/scripts/follow.sh" "$target"
  client="$(tmux list-clients -F '#{client_name}' | head -n 1)"
  [ -n "$client" ] && tmux switch-client -c "$client" -t "$target" 2>/dev/null
  tmux select-window -t "$target"
  tmux select-pane -t "$target"
}

quit() { [ -n "${AGENTS_MON_PIN:-}" ] && rm -f "$AGENTS_MON_PIN"; exit 0; }

show_help() { # blocks until a key; animations pause meanwhile
  printf '%s' "$E[2J$E[H$E[1magents — help$E[0m$NL$NL\
$E[1mstatus$E[0m$NL\
 $E[32m⣿$E[0m  idle$NL\
 $E[33m⠹$E[0m  working (spinner)$NL\
 $E[31m⣿$E[0m  blocked, waiting for input (blinks)$NL\
 $E[32m⣿$E[0m  done, not viewed yet (blinks)$NL$NL\
$E[1mkeys$E[0m$NL\
 j/k ↑/↓  move selection$NL\
 Enter/l  jump to agent$NL\
 q Esc    close sidebar$NL\
 ?        this help$NL$NL\
$E[2mpress any key to return$E[0m"
  IFS= read -rsn1
  printf '%s' "$E[2J"
}

# seed from the previous instance's scan for an instant first frame
[ -f "$CACHE_FILE" ] && cp "$CACHE_FILE" "$SCAN_FILE"
start_scan
while :; do
  tick=$(( (tick + 1) % 40 ))  # divisible by 8 (spin) and 4 (blink)
  [ -f "$SCAN_FILE" ] && { scan_tick; force_render=1; }
  # animated states need every tick; all-idle only redraws on scan/key/resize
  case "$debounced" in *working*|*blocked*|*done*) force_render=1 ;; esac
  if [ -n "$force_render" ]; then render; force_render=""; fi
  # relaunch a scan every ~2s once the previous one finished
  if ! kill -0 "$scan_pid" 2>/dev/null && [ ! -f "$SCAN_FILE" ] \
     && [ $((SECONDS - last_scan_start)) -ge 2 ]; then
    start_scan
  fi
  if IFS= read -rsn1 -t "$READ_WAIT" key; then
    case "$key" in
      j) sel=$((sel + 1)) ;;
      k) sel=$((sel - 1)) ;;
      q) quit ;;
      "$C_C") quit ;;
      "$C_D") quit ;;
      l) jump ;;
      '?') show_help ;;
      '') jump ;;  # Enter
      "$E")
        rest=""
        read -rsn2 -t "$ESC_WAIT" rest
        case "$rest" in
          '[A') sel=$((sel - 1)) ;;
          '[B') sel=$((sel + 1)) ;;
          '') quit ;;  # bare Esc
        esac
        ;;
    esac
    [ "$sel" -lt 1 ] && sel=1
    [ "$sel" -gt "$nrows" ] && sel=$nrows
    [ "$sel" -lt 1 ] && sel=1
    sync_sel_pane
    force_render=1
  else
    # Ctrl-D can arrive as EOF rather than a literal byte. Timeouts return
    # >128; EOF returns 1. Treat EOF as an explicit close so the popup process
    # exits and toggle.sh can tear down the popup instead of leaving a shell in
    # the floating window.
    [ "$?" -eq 1 ] && quit
  fi
done

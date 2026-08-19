#!/usr/bin/env bash
# Sidebar — runs inside the sidebar pane.
# Vim-style navigator: / searches; f selects next status; Esc clears filters.
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
# Focused-sidebar title bar: one step lighter than a dark terminal background.
# 234-239 walk the same greyscale if this reads too heavy.
BAR_BG=$'\033[48;5;236m'
C_C=$'\003'
C_D=$'\004'
C_L=$'\014'
C_N=$'\016'
C_P=$'\020'
C_U=$'\025'
BS=$'\177'
NL=$'\n'
# arrow keys deliver their bytes together; only a bare Esc hits this timeout.
# bash >=4 can wait 50ms — old bash 3.2 is stuck with 1s (integer-only -t)
if [ "${BASH_VERSINFO[0]}" -ge 4 ]; then ESC_WAIT=0.05 READ_WAIT=0.25; else ESC_WAIT=1 READ_WAIT=1; fi
# update notice: install-bin.sh records the newest release at most once a day
VERSION="$(bash "$DIR/scripts/version.sh" tag 2>/dev/null)"
LATEST="$(sed -n '1p' "$DIR/target/release/.agents-mon-latest" 2>/dev/null)"
NOTICE="" HINT="" NOTICE_LEN=0
# only a strictly newer release is an update — a checkout ahead of every tag
# must not be told to "update" to the older one behind it
newer() { [ "$(printf '%s\n%s\n' "${1#v}" "${2#v}" | sort -V | tail -n 1)" = "${1#v}" ] &&
          [ "$1" != "$2" ]; }
case "$LATEST" in
  v[0-9]*)
    if newer "$LATEST" "$VERSION"; then
      NOTICE=" $E[2m↑${LATEST#v}$E[22m"
      NOTICE_LEN=$((2 + ${#LATEST} - 1))
      # shown as a contextual row when no search/status filter is active
      HINT="$E[2mpress u to update$E[0m"
    fi
    ;;
esac
debounced=""  # complete scan; cache/status stay unfiltered
visible=""    # filtered rows used by render/navigation/clicks
query=""
state_filter=""
search_focused=""
total_rows=0
nrows=0
sel=1
SPIN='⠹⢸⣰⣤⣆⡇⠏⠛'   # 4-dot snake, clockwise; done = ⣿ (spinner complete)
tick=0
sel_pane=""  # selection sticks to this pane across rescans until moved
last_active=""

sync_sel_pane() { # remember which visible pane the cursor is on
  sel_pane="$(printf '%s' "$visible" | awk -F'\t' -v n="$sel" 'NR == n { print $1 }')"
}

restore_sel() { # after a rescan/filter, follow pane if it remains visible
  local idx
  [ -n "$sel_pane" ] || { sync_sel_pane; return; }
  idx="$(printf '%s' "$visible" | awk -F'\t' -v p="$sel_pane" '$1 == p { print NR; exit }')"
  if [ -n "$idx" ]; then
    sel="$idx"
  else
    [ "$sel" -gt "$nrows" ] && sel=$nrows
    [ "$sel" -lt 1 ] && sel=1
    sync_sel_pane
  fi
}

# Status filter and text query are separate. Session-name hits keep every child
# agent as context; row hits keep only matching agents.
apply_filters() {
  visible="$(printf '%s' "$debounced" | AGENTS_MON_QUERY="$query" \
    awk -F'\t' -v st="$state_filter" '
      BEGIN { q = tolower(ENVIRON["AGENTS_MON_QUERY"]); gsub(/^[[:space:]]+|[[:space:]]+$/, "", q) }
      NF >= 4 {
        line[NR] = $0; state[NR] = $4
        split($2, loc, ":"); session[NR] = loc[1]
        if (q != "" && index(tolower(loc[1]), q)) session_match[loc[1]] = 1
      }
      END {
        for (i = 1; i <= NR; i++) {
          if (st != "") hit = state[i] == st
          else hit = q == "" || session_match[session[i]] || index(tolower(line[i]), q)
          if (hit) print line[i]
        }
      }
    ')"
  nrows="$(printf '%s\n' "$visible" | awk 'NF { n++ } END { print n+0 }')"
  [ "$sel" -gt "$nrows" ] && sel=$nrows
  [ "$sel" -lt 1 ] && sel=1
  restore_sel
}

select_first_match() {
  sel=1
  sync_sel_pane
}

focus_search() {
  state_filter=""
  search_focused=1
  apply_filters
}

cycle_state_filter() {
  query=""
  search_focused=""
  case "$state_filter" in
    '') state_filter=blocked ;;
    blocked) state_filter=working ;;
    working) state_filter=idle ;;
    idle) state_filter=done ;;
    done) state_filter="" ;;
  esac
  apply_filters
  select_first_match
}

clear_filter() {
  query="" state_filter="" search_focused=""
  apply_filters
}

# Paint a selected row with a state-colored background, reasserting it after
# embedded resets and padding it to the pane edge.
state_bg() {
  case "$1${2:+-focused}" in
    blocked-focused) row_bg=$'\033[48;2;42;16;16m' ;;
    blocked) row_bg=$'\033[48;2;32;12;12m' ;;
    working-focused) row_bg=$'\033[48;2;38;32;16m' ;;
    working) row_bg=$'\033[48;2;29;24;12m' ;;
    *-focused) row_bg=$'\033[48;2;15;36;16m' ;;
    *) row_bg=$'\033[48;2;11;27;12m' ;;
  esac
}

bar() {
  [ -n "$3" ] || return 0
  local body="${1//"$E[0m"/"$E[0m$3"}" width=$((cols - $2))
  line="$3$body"
  [ "$width" -gt 0 ] && line="$line$(printf '%*s' "$width" '')"
  line="$line$E[0m"
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
  { bash "$DIR/scripts/scan.sh" list > "$SCAN_FILE.partial" 2>/dev/null \
      && mv "$SCAN_FILE.partial" "$SCAN_FILE"; } &
  scan_pid=$!
  last_scan_start=$SECONDS
}

scan_tick() { # consume a finished background scan from $SCAN_FILE
  local scan pane loc agent state cwd title prev prev_state ticks show new_state_file active
  scan="$(<"$SCAN_FILE")"
  rm -f "$SCAN_FILE"
  printf '%s\n' "$scan" > "$CACHE_FILE"
  active="$(tmux display-message -p -t "$(bash "$DIR/scripts/client.sh" '#{session_id}')" '#{pane_id}' 2>/dev/null)"

  # idle debounce: show idle only after 2 consecutive idle ticks (redraws
  # flash idle-looking frames mid-render — ccmanager lesson)
  debounced=""
  new_state_file=""
  total_rows=0
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
    total_rows=$((total_rows + 1))
  done <<EOF
$scan
EOF
  printf '%s' "$new_state_file" > "$STATE_FILE"
  apply_filters
}

render() {
  local frame n=0 pane loc agent state cwd title mark cols rows cap used rest avail
  local client active idx filter_info="" header_hint room fg row_bg line clipped
  local hdr="" pad="" width
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
  client="$(bash "$DIR/scripts/client.sh" '#{session_id}')"
  active="$(tmux display-message -p -t "$client" '#{pane_id}' 2>/dev/null)"
  if [ -n "$active" ] && [ "$active" != "$last_active" ]; then
    idx="$(printf '%s' "$visible" | awk -F'\t' -v p="$active" '$1 == p { print NR; exit }')"
    if [ -n "$idx" ]; then sel="$idx"; sel_pane="$active"; fi
    last_active="$active"
  fi
  # Filters ride header; nonempty contextual/update hints add one mapped row.
  if [ -n "$state_filter" ]; then
    filter_info=" [$state_filter]"
  elif [ -n "$query" ] || [ -n "$search_focused" ]; then
    filter_info=" /$query"
  fi
  if [ -n "$query" ] || [ -n "$state_filter" ]; then
    filter_info="$filter_info $nrows/$total_rows"
  fi
  room=$((cols - 6 - NOTICE_LEN)); [ "$room" -lt 0 ] && room=0
  filter_info="${filter_info:0:room}"
  if [ -n "$search_focused" ]; then
    header_hint='↵ nav · ^u clear · esc clear'
  elif [ -n "$state_filter" ]; then
    header_hint='f status · j/k · esc clear'
  elif [ -n "$query" ]; then
    header_hint='j/k · ↵ open · esc clear'
  elif [ -n "$HINT" ]; then
    header_hint='u update · / search'
  else
    header_hint=''
  fi
  header_hint="${header_hint:0:cols}"
  # focused sidebar wears a title bar; pad it because $E[K clears to the
  # default background instead of carrying this one to the pane edge.
  if [ -n "${AGENTS_MON_PIN:-}" ] || [ "$active" = "${TMUX_PANE:-}" ]; then
    hdr="$BAR_BG"
    width=$((cols - 6 - ${#filter_info} - NOTICE_LEN))
    [ "$width" -gt 0 ] && pad="$(printf '%*s' "$width" '')"
  fi
  frame="$E[H$hdr$E[1magents$E[22m$E[2m$filter_info$E[22m$NOTICE$pad$E[0m$E[K$NL"
  # rows file mirrors visual lines below y=0; "-" marks non-agent rows.
  local vis="" session="" used=1
  if [ -n "$header_hint" ]; then
    frame="$frame$E[2m$header_hint$E[0m$E[K$NL"
    vis="-$NL"
    used=2
  fi
  if [ -z "$debounced" ]; then
    frame="$frame$E[2mno agents$E[0m$E[K$NL"
  elif [ -z "$visible" ]; then
    frame="$frame$E[2mno matches · Esc shows all$E[0m$E[K$NL"
  else
    while IFS=$'\t' read -r pane loc agent state cwd title; do
      [ -n "$pane" ] || continue
      if [ "${loc%%:*}" != "$session" ]; then
        [ $((used + 2)) -gt "$cap" ] && break
        session="${loc%%:*}"
        frame="$frame$E[1;34m${session:0:cols}$E[0m$E[K$NL"
        vis="$vis-$NL"
        used=$((used + 1))
      fi
      [ "$used" -ge "$cap" ] && break
      n=$((n + 1))
      if [ "$n" = "$sel" ]; then
        case "$state" in blocked) fg=31 ;; working) fg=33 ;; *) fg=32 ;; esac
        if [ -n "${AGENTS_MON_PIN:-}" ] || [ "$active" = "${TMUX_PANE:-}" ]; then
          mark="$E[1;${fg}m❯$E[0m "
          state_bg "$state" focused
        else
          mark="$E[${fg}m❯$E[0m "
          state_bg "$state"
        fi
      else
        mark="  "
        row_bg=""
      fi
      color_dot "$state"
      rest="${loc#*:} $cwd"
      avail=$((cols - 6 - ${#agent}))
      [ "$avail" -gt 0 ] && rest="${rest:0:$avail}"
      line=" $mark$dot $E[1m$agent$E[0m $E[2m$rest$E[0m"
      bar "$line" $((6 + ${#agent} + ${#rest})) "$row_bg"
      frame="$frame$line$E[K$NL"
      vis="$vis$pane$NL"
      used=$((used + 1))
      if [ -n "$title" ] && [ "$used" -lt "$cap" ]; then
        clipped="${title:0:cols-5}"
        line="     $E[2m$clipped$E[0m"
        bar "$line" $((5 + ${#clipped})) "$row_bg"
        frame="$frame$line$E[K$NL"
        vis="$vis$pane$NL"
        used=$((used + 1))
      fi
    done <<EOF
$visible
EOF
  fi
  printf '%s' "$vis" > "$ROWS_FILE"
  printf '%s' "$frame$E[J"
}

jump() {
  local target client
  target="$(printf '%s' "$visible" | awk -F'\t' -v n="$sel" 'NR == n { print $1 }')"
  case "$target" in %*) ;; *) return ;; esac
  # Clear navigator state on switch; restore full persistent sidebar.
  clear_filter
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
  client="$(bash "$DIR/scripts/client.sh")"
  [ -n "$client" ] && tmux switch-client -c "$client" -t "$target" 2>/dev/null
  tmux select-window -t "$target"
  tmux select-pane -t "$target"
}

quit() { [ -n "${AGENTS_MON_PIN:-}" ] && rm -f "$AGENTS_MON_PIN"; exit 0; }

# nohup: update.sh kills this pane partway through the switch. No version
# picker here — this engine only serves until the native one lands; roll back
# with `bash scripts/update.sh v0.1.5` from a shell.
update() { nohup bash "$DIR/scripts/update.sh" latest >/dev/null 2>&1 & }

show_help() { # blocks until a key; animations pause meanwhile
  printf '%s' "$E[2J$E[H$E[1magents — help$E[0m $E[2m$VERSION$E[0m$NL$NL\
$E[1mstatus$E[0m$NL\
 $E[32m⣿$E[0m  idle$NL\
 $E[33m⠹$E[0m  working (spinner)$NL\
 $E[31m⣿$E[0m  blocked, waiting for input (blinks)$NL\
 $E[32m⣿$E[0m  done, not viewed yet (blinks)$NL$NL\
$E[1mkeys$E[0m$NL\
 j/k ↑/↓  move selection$NL\
 Enter/l  jump to agent$NL\
 /        live search; Enter enables j/k$NL\
 f        select next state filter$NL\
 Esc      clear filters / show all$NL\
 u        update to the latest release$NL\
 q        close sidebar$NL\
 Esc      clear filters$NL\
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
  case "$visible" in *working*|*blocked*|*done*) force_render=1 ;; esac
  if [ -n "$force_render" ]; then render; force_render=""; fi
  # relaunch a scan every ~2s once the previous one finished
  if ! kill -0 "$scan_pid" 2>/dev/null && [ ! -f "$SCAN_FILE" ] \
     && [ $((SECONDS - last_scan_start)) -ge 2 ]; then
    start_scan
  fi
  if [ -n "$search_focused" ]; then
    if IFS= read -rsn1 -t "$READ_WAIT" key; then
      case "$key" in
        '') search_focused="" ;; # Enter enables filtered j/k navigation
        "$C_C"|"$C_D") search_focused="" ;;
        "$C_L") clear_filter ;;
        "$BS"|$'\010') query="${query%?}"; apply_filters; select_first_match ;;
        "$C_U") query=""; state_filter=""; apply_filters ;;
        "$C_N") sel=$((sel + 1)) ;;
        "$C_P") sel=$((sel - 1)) ;;
        "$E")
          rest=""
          read -rsn2 -t "$ESC_WAIT" rest
          case "$rest" in
            '[A'|'OA') sel=$((sel - 1)) ;;
            '[B'|'OB') sel=$((sel + 1)) ;;
            '') clear_filter ;; # clear query and return to navigator
          esac
          ;;
        *)
          if [ "${#query}" -lt 256 ]; then
            state_filter=""
            query="$query$key"
            apply_filters
            select_first_match
          fi
          ;;
      esac
      [ "$sel" -lt 1 ] && sel=1
      [ "$sel" -gt "$nrows" ] && sel=$nrows
      [ "$sel" -lt 1 ] && sel=1
      sync_sel_pane
      force_render=1
    else
      [ "$?" -eq 1 ] && quit
    fi
    continue
  fi
  if IFS= read -rsn1 -t "$READ_WAIT" key; then
    case "$key" in
      j) sel=$((sel + 1)) ;;
      k) sel=$((sel - 1)) ;;
      q|Q) quit ;;
      "$C_C") quit ;;
      "$C_D") quit ;;
      "$C_L") clear_filter ;;
      l) jump ;;
      /) focus_search ;;
      f) cycle_state_filter ;;
      u) update ;;
      '?') show_help ;;
      '') jump ;;  # Enter
      "$E")
        rest=""
        read -rsn2 -t "$ESC_WAIT" rest
        case "$rest" in
          # CSI normally, SS3 (ESC O A) in application-cursor mode
          '[A'|'OA') sel=$((sel - 1)) ;;
          '[B'|'OB') sel=$((sel + 1)) ;;
          '') clear_filter ;;  # bare Esc resets filters
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

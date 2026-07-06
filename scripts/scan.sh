#!/usr/bin/env bash
# tmux-agents-mon core: enumerate panes, identify agents, detect state.
# Usage: scan.sh list | scan.sh status | scan.sh detect <conf> <screen-file> [title]

DIR="$(cd "$(dirname "$0")/.." && pwd)"
BUILTIN_AGENTS_DIR="$DIR/agents"
USER_AGENTS_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/tmux-agents-mon/agents"

# ponytail: parallel indexed arrays, bash 3.2 (macOS) has no assoc arrays
N=0
declare -a A_NAME A_BINS A_HINTS A_BT A_BS A_WT A_WS A_IS A_ORDER

load_conf() {
  AGENT_BINS="" AGENT_PATH_HINTS="" BLOCKED_TITLE="" BLOCKED_SCREEN=""
  WORKING_TITLE="" WORKING_SCREEN="" IDLE_SCREEN="" CHECK_ORDER=""
  . "$1"
  local name idx i=0
  name="$(basename "$1" .conf)"
  idx=$N
  while [ "$i" -lt "$N" ]; do
    [ "${A_NAME[$i]}" = "$name" ] && idx=$i
    i=$((i + 1))
  done
  A_NAME[$idx]="$name"
  A_BINS[$idx]="$AGENT_BINS"
  A_HINTS[$idx]="$AGENT_PATH_HINTS"
  A_BT[$idx]="$BLOCKED_TITLE"
  A_BS[$idx]="$BLOCKED_SCREEN"
  A_WT[$idx]="$WORKING_TITLE"
  A_WS[$idx]="$WORKING_SCREEN"
  A_IS[$idx]="$IDLE_SCREEN"
  A_ORDER[$idx]="${CHECK_ORDER:-bt wt bs ws}"
  [ "$idx" = "$N" ] && N=$((N + 1))
}

load_all_confs() {
  local f
  for f in "$BUILTIN_AGENTS_DIR"/*.conf; do
    [ -f "$f" ] && load_conf "$f"
  done
  for f in "$USER_AGENTS_DIR"/*.conf; do
    [ -f "$f" ] && load_conf "$f"  # same name overrides builtin
  done
}

# --- agent identification -------------------------------------------------

normalize_bin() { # path/wrapper -> bare name
  local b="${1##*/}"
  b="${b%.js}"; b="${b%.cmd}"; b="${b%.exe}"
  printf '%s' "$b"
}

agent_for_bin() { # $1=normalized name -> prints agent index
  local i=0 b
  while [ "$i" -lt "$N" ]; do
    for b in ${A_BINS[$i]}; do
      [ "$b" = "$1" ] && { printf '%s' "$i"; return 0; }
    done
    i=$((i + 1))
  done
  return 1
}

agent_for_cmdline() { # $1=full command line of a wrapped process
  # first token is the runtime; first non-flag arg after it decides (herdr mod.rs)
  local tok first=1 i h
  for tok in $1; do
    if [ "$first" = 1 ]; then first=0; continue; fi
    case "$tok" in
      -e|--eval|-c|-p|--print) return 1 ;;  # inline payload, never an agent
      -*) continue ;;
    esac
    agent_for_bin "$(normalize_bin "$tok")" && return 0
    i=0
    while [ "$i" -lt "$N" ]; do
      for h in ${A_HINTS[$i]}; do
        case "$tok" in *"$h"*) printf '%s' "$i"; return 0 ;; esac
      done
      i=$((i + 1))
    done
    return 1  # only the first script arg counts
  done
  return 1
}

PS_CACHE=""
descendant_cmdlines() { # $1=root pid -> command lines of all descendants
  [ -z "$PS_CACHE" ] && PS_CACHE="$(ps -axo pid=,ppid=,command=)"
  printf '%s\n' "$PS_CACHE" | awk -v root="$1" '
    { pid = $1; ppid = $2
      line = $0; sub(/^[ \t]*[0-9]+[ \t]+[0-9]+[ \t]+/, "", line)
      cmd[pid] = line; par[pid] = ppid }
    END { q[1] = root; n = 1
      if (root in cmd) print cmd[root]  # pane root itself (agent as pane command)
      for (i = 1; i <= n; i++)
        for (p in par)
          if (par[p] == q[i]) { q[++n] = p; print cmd[p] } }'
}

identify_agent() { # $1=pane_pid $2=pane_current_command -> prints agent index
  local idx line
  agent_for_bin "$(normalize_bin "$2")" && return 0
  # no direct match: walk pane's process tree (wrappers, node CLIs, odd shells)
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    idx="$(agent_for_bin "$(normalize_bin "${line%% *}")")" && { printf '%s' "$idx"; return 0; }
    idx="$(agent_for_cmdline "$line")" && { printf '%s' "$idx"; return 0; }
  done <<EOF
$(descendant_cmdlines "$1")
EOF
  return 1
}

# --- state detection -------------------------------------------------------

gre() { # haystack, pattern -> grep hit (empty pattern never matches)
  [ -n "$2" ] && printf '%s\n' "$1" | grep -Eiq -- "$2"
}

detect_state() { # $1=agent idx $2=title $3=screen text -> prints state
  local i="$1" title="$2" screen step
  # menus/prompt box live at the bottom of the pane
  screen="$(printf '%s\n' "$3" | tail -n 20)"
  for step in ${A_ORDER[$i]}; do
    case "$step" in
      bt) gre "$title" "${A_BT[$i]}"  && { echo blocked; return; } ;;
      bs) gre "$screen" "${A_BS[$i]}" && { echo blocked; return; } ;;
      wt) gre "$title" "${A_WT[$i]}"  && { echo working; return; } ;;
      ws) gre "$screen" "${A_WS[$i]}" && { echo working; return; } ;;
      is) gre "$screen" "${A_IS[$i]}" && { echo idle; return; } ;;
    esac
  done
  echo idle
}

# --- commands --------------------------------------------------------------

scan() { # one line per agent pane: pane_id \t loc \t agent \t state \t dir
  local pane pid cmd path loc title idx state
  PS_CACHE="$(ps -axo pid=,ppid=,command=)"  # one ps per scan; subshells inherit
  while IFS=$'\t' read -r pane pid cmd path loc title; do
    [ -n "${AGENTS_MON_SELF:-}" ] && [ "$pane" = "$AGENTS_MON_SELF" ] && continue  # sidebar skips itself
    idx="$(identify_agent "$pid" "$cmd")" || continue
    state="$(detect_state "$idx" "$title" "$(tmux capture-pane -p -t "$pane" 2>/dev/null)")"
    printf '%s\t%s\t%s\t%s\t%s\n' "$pane" "$loc" "${A_NAME[$idx]}" "$state" "${path##*/}"
  done <<EOF
$(tmux list-panes -a -F '#{pane_id}	#{pane_pid}	#{pane_current_command}	#{pane_current_path}	#{session_name}:#{window_index}.#{pane_index}	#{pane_title}')
EOF
}

status() { # compact segment for status-line, empty when no agents
  scan | awk -F'\t' '
    $4 == "blocked" { b++ } $4 == "working" { w++ } $4 == "idle" { i++ }
    END {
      out = ""
      if (b) out = out "#[fg=red]⣿#[default]" b " "
      if (w) out = out "#[fg=yellow]⣾#[default]" w " "
      if (i) out = out "#[fg=green]⣿#[default]" i " "
      sub(/ $/, "", out); printf "%s", out
    }'
}

case "${1:-list}" in
  list)   load_all_confs; scan ;;
  status) load_all_confs; status ;;
  detect) # detect <conf-file> <screen-file> [title]  (for tests)
    load_conf "$2"
    detect_state 0 "${4:-}" "$(cat "$3")"
    ;;
  *) echo "usage: scan.sh [list|status|detect <conf> <screen-file> [title]]" >&2; exit 1 ;;
esac

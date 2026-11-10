#!/usr/bin/env bash
# Live check against the running Rex app, not part of tests/run.sh: drag the
# sidebar in the window you are in, visit every other window, and confirm each
# sidebar settles at the dragged width without bumps and keeps it. It moves the
# app between sessions, then returns to your window and restores your width.
#
#   tests/rex-width-live.sh [cols...]    (default: 32 26)
#
# Needs the Rex app showing a window, the agenmux sidebar on, rex, jq, python3.
set -euo pipefail

export REX_SERVER="${REX_SERVER:-unix://$HOME/Library/Application%20Support/rex/server.sock}"
width_file="${XDG_STATE_HOME:-$HOME/.local/state}/agenmux/rex-sidebar-width"
targets=("$@")
[ ${#targets[@]} -gt 0 ] || targets=(32 26)

# Every window: its sidebar's columns, its divider, and how many real blocks
# it holds. With op=resize, sets one window's divider instead.
read -r -d '' probe <<'LUA' || true
local t, a = "com.superlogical.terminal", rex.args or {}
if a.op == "resize" then
  rex.session.attach{session_id = a.session}
  for _, w in ipairs(rex.call("session.view", {session_id = a.session}).windows) do
    if w.window_id == a.window then
      local sp = w.layers[1].layout.split
      rex.call("session.resize_pane", {session_id = a.session, direction = "horizontal",
        before_block_id = sp.before.block_id, after_block_id = sp.after.block_id, ratio = a.ratio})
    end
  end
  return true
end
local out = {}
for _, s in ipairs(rex.call("session.list").sessions or {}) do
  rex.session.attach{session_id = s.session_id}
  local v = rex.call("session.view", {session_id = s.session_id})
  for _, w in ipairs(v.windows or {}) do
    local l = w.layers and w.layers[1]
    local row = {session = s.session_id, label = s.label, window = w.window_id, real = 0}
    for _, b in ipairs(l and l.blocks or {}) do
      if b.label == "agenmux" then
        local z = rex.block.call(t, "size", {session_id = s.session_id, block_id = b.block_id}) or {}
        row.cols, row.measured = z.columns, next(z.desired or {}) ~= nil
        local split = l.layout.split
        if split then row.ratio, row.before = split.ratio, split.before.block_id == b.block_id end
      elseif b.label ~= "agenmux-popup" then
        row.real = row.real + 1
      end
    end
    out[#out + 1] = row
  end
end
return out
LUA

client="$(rex client ls --json | jq -r '[.clients[] | select(.info.kind == "app")][0].client_id')"
[ "$client" != null ] || { echo "no Rex app client" >&2; exit 1; }
windows() { rex do -e "$probe" --json | jq -c 'if type == "array" then . else [] end'; }
row() { windows | jq -c --arg w "$1" '.[] | select(.window == $w)'; }
show() { rex -C "$client" do session.select "session_id=$1" "window_id=$2" >/dev/null; }

# Wait until the window's sidebar is measured and its divider and width hold
# for 2s. Prints "cols moves", moves counting each divider position seen.
settle() {
  local session="$1" window="$2" last="" same=0 seen="" r key
  for _ in $(seq 1 80); do
    r="$(row "$window")"
    key="$(jq -r '"\(.cols) \(.ratio)"' <<<"$r")"
    if [ "$(jq -r '.measured // false' <<<"$r")" = true ] && [ "$key" = "$last" ]; then
      same=$((same + 1))
      [ "$same" -lt 8 ] || break
    else
      same=0
    fi
    case " $seen " in *" ${key#* } "*) ;; *) seen="$seen ${key#* }" ;; esac
    last="$key"
    sleep 0.25
  done
  set -- $seen
  echo "${last%% *} $(($# - 1))"
}

# A drag: move the divider so the sidebar gets about `cols` columns.
drag() {
  local session="$1" window="$2" cols="$3" r ratio
  r="$(row "$window")"
  ratio="$(jq -r --argjson want "$cols" '
    ((if .before then .ratio else 1 - .ratio end) * ($want + 0.5) / (.cols + 0.5)) as $share
    | if .before then $share else 1 - $share end' <<<"$r")"
  rex do -e "$probe" --args "{\"op\":\"resize\",\"session\":\"$session\",\"window\":\"$window\",\"ratio\":$ratio}" >/dev/null
}

shown="$(defaults export com.superlogical.rex - | python3 -I -c '
import json, plistlib, sys
print(json.loads(plistlib.loads(sys.stdin.buffer.read())["Workspace.lastFocusedSession"])["sessionID"])')"
home="$(windows | jq -r --arg s "$shown" '[.[] | select(.session == $s and .cols != null)][0].window')"
[ "$home" != null ] || { echo "no sidebar in the shown session; open it first" >&2; exit 1; }
original="$(cat "$width_file")"
others="$(windows | jq -r --arg h "$home" '.[] | select(.window != $h and .real > 0) | "\(.session) \(.window) \(.label)"')"
fail=0
check() { # name want got moves
  if [ "$2" = "$3" ] && [ "$4" -le 1 ]; then
    printf 'ok   %-44s %s cols, %s move(s)\n' "$1" "$3" "$4"
  else
    printf 'FAIL %-44s want %s, got %s cols, %s move(s)\n' "$1" "$2" "$3" "$4"
    fail=1
  fi
}

restore() {
  show "$shown" "$home"
  settle "$shown" "$home" >/dev/null
  # a drag lands within a column: retry until it is exact
  for _ in 1 2 3; do
    [ "$(row "$home" | jq -r .cols)" != "$original" ] || break
    drag "$shown" "$home" "$original"
    settle "$shown" "$home" >/dev/null
  done
  echo "restored: $(row "$home" | jq -r .cols) cols (was $original), saved $(cat "$width_file")"
}
trap restore EXIT

for target in "${targets[@]}"; do
  show "$shown" "$home"
  settle "$shown" "$home" >/dev/null
  drag "$shown" "$home" "$target"
  read -r cols _ < <(settle "$shown" "$home")
  echo "dragged to $cols cols (aimed $target)"
  check "saved width" "$cols" "$(cat "$width_file")" 0
  while read -r session window label; do
    [ -n "$session" ] || continue
    show "$session" "$window"
    read -r got moves < <(settle "$session" "$window")
    check "$label ${window: -8}" "$cols" "$got" "$moves"
  done <<<"$others"
  show "$shown" "$home"
  read -r got moves < <(settle "$shown" "$home")
  check "back home ${home: -8}" "$cols" "$got" "$moves"
done
exit "$fail"

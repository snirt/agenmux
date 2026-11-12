#!/usr/bin/env bash
# agents/pi.conf SUBJECT_CMD against synthetic session stores: which session
# file a pane gets, and that the cost stays bounded however many and however
# large the files are (#256: one cwd with 171 files / 140 MB took ~18s).
DIR="$(cd "$(dirname "$0")/.." && pwd)"
fail=0
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
. "$DIR/agents/pi.conf"

# The script captures $pane's scrollback through tmux; serve a fixture instead.
mkdir -p "$tmp/bin"
printf '#!/usr/bin/env bash\ncat "$SEEN" 2>/dev/null\n' >"$tmp/bin/tmux"
chmod +x "$tmp/bin/tmux"

store() { printf '%s/home/.pi/agent/sessions/-%s--' "$tmp" "${1//\//-}"; }
# session <cwd> <file stem> <mtime YYYYMMDDhhmm> <first prompt> <last prompt> [name] [pad bytes]
session() {
  local d f
  d="$(store "$1")"
  f="$d/$2.jsonl"
  mkdir -p "$d"
  {
    printf '{"type":"session","version":3,"id":"%s","cwd":"%s"}\n' "$2" "$1"
    printf '{"type":"message","id":"u1","message":{"role":"user","content":[{"type":"text","text":"%s"}]}}\n' "$4"
    if [ -n "${7:-}" ]; then
      printf '{"type":"message","id":"a1","message":{"role":"assistant","content":[{"type":"text","text":"'
      head -c "$7" /dev/zero | tr '\0' x
      printf '"}]}}\n'
    fi
    printf '{"type":"message","id":"u2","message":{"role":"user","content":[{"type":"text","text":"%s"}]}}\n' "$5"
    [ -z "${6:-}" ] || printf '{"type":"session_info","name":"%s"}\n' "$6"
  } >"$f"
  touch -t "$3" "$f"
}
subj() { # <cwd> <scrollback text> [started]
  printf '%s\n' "$2" >"$tmp/seen"
  PATH="$tmp/bin:$PATH" SEEN="$tmp/seen" HOME="$tmp/home" \
    path="$1" pane=%1 started="${3:-}" bash -c "$SUBJECT_CMD"
}
check() { # <name> <got> <want>
  if [ "$2" = "$3" ]; then
    echo "ok   pi-subject-$1"
  else
    echo "FAIL pi-subject-$1: got [$2] want [$3]"
    fail=1
  fi
}

# /work: newest is an empty stub (pi writes one on launch), then alpha, bravo.
session /work 2026-01-01T00-00-03-000Z_c 202601010003 "" "" ""
: >"$(store /work)/2026-01-01T00-00-03-000Z_c.jsonl"
touch -t 202601010003 "$(store /work)/2026-01-01T00-00-03-000Z_c.jsonl"
session /work 2026-01-01T00-00-02-000Z_a 202601010002 "alpha first" "alpha last"
session /work 2026-01-01T00-00-01-000Z_b 202601010001 "bravo first" "bravo last" "Bravo named"
long="a long prompt that runs well past the forty character prefix"
session /work 2026-01-01T00-00-00-000Z_l 202601010000 "long first" "$long"

check no-scrollback-match-takes-newest-yield "$(subj /work "")" "alpha first"
check scrollback-picks-its-session "$(subj /work "$(printf 'x\n bravo last\ny')")" "Bravo named"
check short-prompt-matches-whole-line-only "$(subj /work " bravo last and more")" "alpha first"
check long-prompt-matches-its-prefix "$(subj /work " $long (wrapped)")" "long first"
# a file created after the agent started outranks a newer-modified one that
# predates it (a resumed session another pane writes to)
session /own 2026-01-03T00-00-00-000Z_mine 202601030000 "mine first" "mine last"
session /own 2026-01-01T00-00-00-000Z_resumed 202601040000 "resumed first" "resumed last"
check started-session-comes-first "$(subj /own "" 2026-01-02T00-00-00)" "mine first"
check without-start-newest-wins "$(subj /own "")" "resumed first"
check unknown-dir-is-empty "$(subj /nowhere "")" ""

# Only the 10 newest files are candidates: an 11th that matches is ignored.
for i in $(seq -w 1 10); do
  session /many "2026-01-02T00-00-$i-000Z_n$i" "2026010200$i" "newer $i" "newer last $i"
done
session /many 2026-01-01T00-00-00-000Z_old 202601010000 "old first" "old last"
check only-newest-ten-searched "$(subj /many " old last")" "newer 10"

# A last prompt behind megabytes of output is still read from the file tail.
session /big 2026-01-01T00-00-00-000Z_t 202601010001 "big first" "big last" "" 4000000
session /big 2026-01-01T00-00-01-000Z_u 202601010002 "other first" "other last"
check prompt-behind-large-output "$(subj /big " big last")" "big first"

# Bounded cost: 40 files x 4 MB with no scrollback match used to walk and
# regex every byte of every file.
d="$(store /huge)"
session /huge seed 202601010000 "huge first" "huge last" "" 4000000
for i in $(seq -w 1 40); do
  cp "$d/seed.jsonl" "$d/2026-01-01T00-00-$i-000Z_h$i.jsonl"
  touch -t "2026010100$i" "$d/2026-01-01T00-00-$i-000Z_h$i.jsonl"
done
rm "$d/seed.jsonl"
start=$(date +%s%N 2>/dev/null)
case "$start" in *N | "") start=$(($(date +%s) * 1000000000)) ;; esac
got="$(subj /huge "nothing here")"
end=$(date +%s%N 2>/dev/null)
case "$end" in *N | "") end=$(($(date +%s) * 1000000000)) ;; esac
ms=$(((end - start) / 1000000))
if [ "$got" = "huge first" ] && [ "$ms" -lt 1500 ]; then
  echo "ok   pi-subject-bounded-on-large-store (${ms}ms)"
else
  echo "FAIL pi-subject-bounded-on-large-store: got [$got] in ${ms}ms"
  fail=1
fi

exit "$fail"

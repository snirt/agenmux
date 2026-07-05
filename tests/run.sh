#!/usr/bin/env bash
# Asserts detect_state over fixtures: tests/fixtures/<agent>-<state>[-x].txt
# Optional <fixture>.title sidecar supplies the pane title.
DIR="$(cd "$(dirname "$0")/.." && pwd)"
fail=0 count=0

for fx in "$DIR"/tests/fixtures/*.txt; do
  base="$(basename "$fx" .txt)"
  agent="${base%%-*}"
  expected="${base#*-}"; expected="${expected%%-*}"
  title=""
  [ -f "${fx%.txt}.title" ] && title="$(cat "${fx%.txt}.title")"
  got="$(bash "$DIR/scripts/scan.sh" detect "$DIR/agents/$agent.conf" "$fx" "$title")"
  count=$((count + 1))
  if [ "$got" = "$expected" ]; then
    echo "ok   $base"
  else
    echo "FAIL $base: expected $expected, got $got"
    fail=1
  fi
done

echo "$count fixtures"
exit $fail

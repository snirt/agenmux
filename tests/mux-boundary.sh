#!/usr/bin/env bash
# Shared code reaches the multiplexer through the Multiplexer trait. Outside
# src/mux/ nothing starts tmux, and only the modules that run from tmux
# bindings and hooks name the tmux backend directly. Test modules are exempt:
# they drive real tmux servers.
set -euo pipefail

DIR="$(cd "$(dirname "$0")/.." && pwd)"
integration='^src/(setup|toggle|panes|input|release|notifications)\.rs$'

# A file's lines before its `#[cfg(test)] mod tests`, prefixed `path:line:`.
production() {
  awk -v path="$1" '
    held != "" && /^(pub\(crate\) )?mod tests/ { exit }
    held != "" { print held; held = "" }
    /^#\[cfg\(test\)\]$/ { held = path ":" NR ":" $0; next }
    { print path ":" NR ":" $0 }
  ' "$1"
}

cd "$DIR"
found=""
while IFS= read -r file; do
  case "$file" in src/mux/*) continue ;; esac
  lines="$(production "$file")"
  spawns="$(printf '%s\n' "$lines" | grep -E '\("tmux"[,)]' || true)"
  named=""
  if ! [[ $file =~ $integration ]]; then
    named="$(printf '%s\n' "$lines" | grep -E 'mux::tmux' || true)"
  fi
  found="$found${spawns:+$spawns$'\n'}${named:+$named$'\n'}"
done < <(find src -name '*.rs' | LC_ALL=C sort)

if [ -n "$found" ]; then
  printf 'tmux reached outside the multiplexer seam:\n%s' "$found" >&2
  exit 1
fi

#!/usr/bin/env bash
# Shared code reaches the multiplexer through the Multiplexer trait and the
# backend-neutral functions in src/mux/. Outside src/mux/ nothing starts tmux
# or rex, and nothing names either backend module, in any import form
# (`mux::tmux`, `mux::{self, rex}`). Test modules are exempt: they drive real
# tmux servers. AGENMUX_REF_ROOT points the check at another tree.
set -euo pipefail

DIR="${AGENMUX_REF_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"

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
  spawns="$(printf '%s\n' "$lines" | grep -E '\("(tmux|rex)"[,)]|rex_cli|REX_BIN_DIR' || true)"
  named="$(printf '%s\n' "$lines" |
    grep -E 'mux::(tmux|rex)\b|mux::\{[^}]*\b(tmux|rex)\b' || true)"
  found="$found${spawns:+$spawns$'\n'}${named:+$named$'\n'}"
done < <(find src -name '*.rs' | LC_ALL=C sort)

if [ -n "$found" ]; then
  printf 'multiplexer reached outside the seam:\n%s' "$found" >&2
  exit 1
fi

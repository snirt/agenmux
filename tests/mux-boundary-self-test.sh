#!/usr/bin/env bash
# tests/mux-boundary.sh must reject every way shared code can name a backend.
set -euo pipefail

DIR="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/src/mux/tmux" "$tmp/src/sidebar"
gate() { AGENMUX_REF_ROOT="$tmp" "$DIR/tests/mux-boundary.sh" >/dev/null 2>&1; }

# Backends may name each other; test modules may drive tmux.
printf 'use crate::mux::tmux;\n' >"$tmp/src/mux/runtime.rs"
printf 'use crate::mux::rex;\n' >"$tmp/src/mux/tmux/keys.rs"
printf 'fn run() {}\n\n#[cfg(test)]\nmod tests {\n    use crate::mux::tmux::Tmux;\n}\n' \
  >"$tmp/src/sidebar/render.rs"
printf 'fn main() {\n    crate::mux::runtime_dir();\n}\n' >"$tmp/src/main.rs"
if ! gate; then
  echo 'gate rejected code that stays behind the seam' >&2
  exit 1
fi

for planted in \
  'use crate::mux::tmux;' \
  'use crate::mux::{self, tmux};' \
  'let _ = crate::mux::rex::teardown();' \
  'use crate::mux::{rex, Multiplexer};' \
  'let _ = std::process::Command::new("tmux");'; do
  printf 'fn main() {}\n%s\n' "$planted" >"$tmp/src/main.rs"
  if gate; then
    echo "gate accepted: $planted" >&2
    exit 1
  fi
done

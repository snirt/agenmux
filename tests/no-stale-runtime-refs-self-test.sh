#!/usr/bin/env bash
set -euo pipefail

DIR="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp"/{scripts,src,tests/fixtures,.github/workflows,docs/plans}
touch "$tmp/agenmux.tmux" "$tmp/agents-mon.tmux" "$tmp/Cargo.toml" \
  "$tmp/README.md" "$tmp/CONTRIBUTING.md" "$tmp/Makefile" \
  "$tmp/tests/legacy-name-allowlist.txt"
printf '    let toggle = plugin_dir.join("scripts/toggle.sh");\n' >"$tmp/src/release.rs"

AGENMUX_REF_ROOT="$tmp" "$DIR/tests/no-stale-runtime-refs.sh"
printf 'scripts/toggle.sh\n' >"$tmp/README.md"
if AGENMUX_REF_ROOT="$tmp" "$DIR/tests/no-stale-runtime-refs.sh" >/dev/null 2>&1; then
  echo 'gate accepted an additional toggle reference' >&2
  exit 1
fi
: >"$tmp/README.md"
: >"$tmp/src/release.rs"
if AGENMUX_REF_ROOT="$tmp" "$DIR/tests/no-stale-runtime-refs.sh" >/dev/null 2>&1; then
  echo 'gate accepted a missing compatibility probe' >&2
  exit 1
fi

#!/usr/bin/env bash
# Fail when active source/config/docs refer to removed Bash runtime entrypoints.
# Historical plans and captured screens are evidence, not executable contracts.
set -euo pipefail

DIR="$(cd "$(dirname "$0")/.." && pwd)"
removed=""
for name in scan sidebar client follow click scroll hooks mirror-add orphan pin restore teardown update; do
  removed="${removed}${removed:+|}scripts/${name}\\.sh"
done
removed="$removed|agents-mon"' mirror'

stale_removed="$({
  find "$DIR/agents-mon.tmux" "$DIR/scripts" "$DIR/src" "$DIR/tests" \
    "$DIR/README.md" "$DIR/CONTRIBUTING.md" "$DIR/Makefile" "$DIR/.github" "$DIR/docs" \
    -type f \
    ! -path "$DIR/docs/plans/*" \
    ! -path "$DIR/docs/superpowers/plans/*" \
    ! -path "$DIR/tests/fixtures/*" \
    -exec grep -nHE "$removed" {} +
} 2>/dev/null || true)"

# The native updater intentionally probes an old target tree's toggle wrapper.
# Current production/config/docs must not invoke that removed current-tree path.
stale_toggle="$({
  find "$DIR/agents-mon.tmux" "$DIR/scripts" "$DIR/src" \
    "$DIR/README.md" "$DIR/CONTRIBUTING.md" "$DIR/Makefile" "$DIR/.github" \
    -type f ! -path "$DIR/src/release.rs" \
    -exec grep -nHE 'scripts/toggle\.sh' {} +
} 2>/dev/null || true)"

if [ -n "$stale_removed$stale_toggle" ]; then
  printf 'stale Rust-only runtime references:\n%s%s\n' "$stale_removed" "$stale_toggle" >&2
  exit 1
fi

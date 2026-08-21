#!/usr/bin/env bash
# Fail when active source/config/docs refer to removed Bash runtime entrypoints.
# Historical plans and captured screens are evidence, not executable contracts.
set -euo pipefail

DIR="${AGENTS_MON_REF_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}"
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
# Require that exact probe once; reject missing, changed, or additional refs.
production=("$DIR/agents-mon.tmux" "$DIR/scripts" "$DIR/src" "$DIR/README.md" \
  "$DIR/CONTRIBUTING.md" "$DIR/Makefile" "$DIR/.github")
toggle_refs="$({
  find "${production[@]}" -type f -exec grep -nHF 'scripts/toggle.sh' {} +
} 2>/dev/null || true)"
expected_toggle='    let toggle = plugin_dir.join("scripts/toggle.sh");'
expected_ref="$(grep -nHFx "$expected_toggle" "$DIR/src/release.rs" 2>/dev/null || true)"
toggle_count="$(printf '%s\n' "$toggle_refs" | grep -c . || true)"

if [ -n "$stale_removed" ]; then
  printf 'stale Rust-only runtime references:\n%s\n' "$stale_removed" >&2
  exit 1
fi
if [ "$toggle_count" -ne 1 ] || [ "$toggle_refs" != "$expected_ref" ]; then
  printf 'expected exactly one old-target compatibility probe in src/release.rs; found:\n%s\n' \
    "${toggle_refs:-<none>}" >&2
  exit 1
fi

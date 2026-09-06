#!/usr/bin/env bash
set -eu

DIR="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$(mktemp -d)"
trap '[ -n "${KEEP_TMP:-}" ] || rm -rf "$tmp"' EXIT
mkdir -p "$tmp/plugin/scripts" "$tmp/plugin/target/release" "$tmp/plugin/target/debug" "$tmp/bin"
cp "$DIR/agenmux.tmux" "$tmp/plugin/agenmux.tmux"
cp "$DIR/scripts/version.sh" "$tmp/plugin/scripts/version.sh"
cp "$DIR/Cargo.toml" "$tmp/plugin/Cargo.toml"
version="$(bash "$DIR/scripts/version.sh")"

cat >"$tmp/bin/tmux" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$TEST_LOG"
case "$*" in
  "show-options -gq @agenmux-bin")
    [ -f "$TEST_CLEARED" ] || printf '@agenmux-bin /removed/worktree/target/debug/agenmux\n'
    ;;
  "show-option -gqv @agenmux-bin")
    [ -f "$TEST_CLEARED" ] || printf '/removed/worktree/target/debug/agenmux\n'
    ;;
  "set-option -gu @agenmux-bin") touch "$TEST_CLEARED" ;;
  "wait-for -L agenmux-install"|"wait-for -U agenmux-install") ;;
esac
SH
cat >"$tmp/bin/git" <<'SH'
#!/usr/bin/env bash
exit 1
SH
cat >"$tmp/plugin/scripts/install-bin.sh" <<SH
#!/usr/bin/env bash
printf 'install\n' >>"$tmp/runtime.log"
cat >"$tmp/plugin/target/release/agenmux" <<'BIN'
#!/usr/bin/env bash
if [ "\${1:-}" = --version ]; then printf 'agenmux $version\n'; else printf '%s\n' "\$*" >>"$tmp/runtime.log"; fi
BIN
chmod +x "$tmp/plugin/target/release/agenmux"
printf 'v$version\n-\n' >"$tmp/plugin/target/release/.agenmux-version"
SH
cat >"$tmp/plugin/target/debug/agenmux" <<SH
#!/usr/bin/env bash
printf '%s\n' "\$*" >>"$tmp/runtime.log"
SH
chmod +x "$tmp/plugin/target/debug/agenmux"
chmod +x "$tmp/bin/tmux" "$tmp/bin/git" "$tmp/plugin/scripts/install-bin.sh"

TEST_LOG="$tmp/tmux.log" TEST_CLEARED="$tmp/cleared" PATH="$tmp/bin:$PATH" \
  bash "$tmp/plugin/agenmux.tmux" activate popup test-client

grep -Fq 'set-option -gu @agenmux-bin' "$tmp/tmux.log"
grep -Fq "set-option -g @agenmux-bin $tmp/plugin/target/debug/agenmux" "$tmp/tmux.log"
! grep -qx install "$tmp/runtime.log"
grep -qx setup "$tmp/runtime.log"
grep -qx 'toggle popup test-client' "$tmp/runtime.log"
echo 'ok   stale-activation-binary-recovers'

rm -rf "$tmp"
tmp="$(mktemp -d)"
mkdir -p "$tmp/plugin/scripts" "$tmp/plugin/target/debug" "$tmp/bin"
cp "$DIR/scripts/dev-bin.sh" "$tmp/plugin/scripts/dev-bin.sh"

cat >"$tmp/bin/cargo" <<'SH'
#!/usr/bin/env bash
cat >"$TEST_PLUGIN/target/debug/agenmux" <<'BIN'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$TEST_RUNTIME"
BIN
chmod +x "$TEST_PLUGIN/target/debug/agenmux"
SH
cat >"$tmp/bin/tmux" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$TEST_LOG"
case "$*" in
  "show-options -gq @agenmux-bin") printf '@agenmux-bin /removed/worktree/target/debug/agenmux\n' ;;
  "show-option -gqv @agenmux-bin") printf '/removed/worktree/target/debug/agenmux\n' ;;
  "show-option -gqv @agenmux-on") printf '1\n' ;;
esac
SH
chmod +x "$tmp/bin/cargo" "$tmp/bin/tmux"

TEST_PLUGIN="$tmp/plugin" TEST_RUNTIME="$tmp/runtime.log" TEST_LOG="$tmp/tmux.log" \
  PATH="$tmp/bin:$PATH" bash "$tmp/plugin/scripts/dev-bin.sh" use >/dev/null

grep -qx setup "$tmp/runtime.log"
grep -qx toggle "$tmp/runtime.log"
grep -Fq "set-option -g @agenmux-bin $tmp/plugin/target/debug/agenmux" "$tmp/tmux.log"
echo 'ok   stale-dev-switch-binary-recovers'

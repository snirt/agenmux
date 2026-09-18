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
cat >"$tmp/bin/docker" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$@" >>"$TEST_DOCKER"
SH
chmod +x "$tmp/bin/cargo" "$tmp/bin/tmux" "$tmp/bin/docker"

TEST_PLUGIN="$tmp/plugin" TEST_RUNTIME="$tmp/runtime.log" TEST_LOG="$tmp/tmux.log" \
  PATH="$tmp/bin:$PATH" bash "$tmp/plugin/scripts/dev-bin.sh" use >/dev/null

grep -qx setup "$tmp/runtime.log"
grep -qx toggle "$tmp/runtime.log"
grep -Fq "set-option -g @agenmux-bin $tmp/plugin/target/debug/agenmux" "$tmp/tmux.log"

mkdir -p "$tmp/home/.config/agenmux/agents"
touch "$tmp/home/.tmux.conf" "$tmp/home/.config/agenmux/config.toml"
TEST_DOCKER="$tmp/docker-default.log" HOME="$tmp/home" REF=master PATH="$tmp/bin:$PATH" \
  bash "$tmp/plugin/scripts/dev-bin.sh" docker
grep -Fxq 'AGENMUX_REF=master' "$tmp/docker-default.log"
grep -Fxq "$tmp/home/.tmux.conf:/root/.tmux.conf:ro" "$tmp/docker-default.log"
grep -Fxq "$tmp/home/.config/agenmux/config.toml:/root/.config/agenmux/config.toml:ro" "$tmp/docker-default.log"
grep -Fxq "$tmp/home/.config/agenmux/agents:/root/.config/agenmux/agents:ro" "$tmp/docker-default.log"
grep -Fxq 'TMUX_CONFIG=/root/.tmux.conf' "$tmp/docker-default.log"
grep -Fxq 'build' "$tmp/docker-default.log"
grep -Fxq 'agenmux-dev' "$tmp/docker-default.log"
grep -Fq 'agenmux-cargo-registry:/root/.cargo/registry' "$tmp/docker-default.log"
grep -Fq 'agenmux-cargo-git:/root/.cargo/git' "$tmp/docker-default.log"
grep -Fq 'agenmux-build-cache:/tmp/agenmux-target' "$tmp/docker-default.log"
grep -Fq 'agenmux-pi-home:/root/.pi/agent' "$tmp/docker-default.log"
grep -Fq 'sed -E "/(agents-mon|agenmux)\.tmux/d;' "$tmp/docker-default.log"
grep -Fq 'default-(shell|command)' "$tmp/docker-default.log"
grep -Fq 'choose-tree[[:space:]]+-[A-Za-z]*)y' "$tmp/docker-default.log"
grep -Fq 'width=[^,' "$tmp/docker-default.log"
grep -Fq 'align=[^,' "$tmp/docker-default.log"
grep -Fq 'set -g default-shell /bin/bash' "$tmp/docker-default.log"
grep -Fq 'set -g default-terminal tmux-256color' "$tmp/docker-default.log"
grep -Fq 'set -as terminal-features ,xterm-256color:RGB' "$tmp/docker-default.log"
grep -Fq 'set -g @agenmux-bin /tmp/agenmux-target/debug/agenmux' "$tmp/docker-default.log"
grep -Fq 'printf "\033[2J\033[H"' "$tmp/docker-default.log"
grep -Fq 'AGENMUX_SKIP_UPDATE=1 AGENMUX_FORCE_WIZARD=1 AGENMUX_TMUX_CONF=/tmp/tmux.conf sh /workspace/install.sh' "$tmp/docker-default.log"
! grep -Fq 'install.sh >/dev/null' "$tmp/docker-default.log"
grep -Fq 'git clone --depth 1 --branch "$AGENMUX_REF" https://github.com/snirt/agenmux' "$tmp/docker-default.log"
grep -Fq 'tmux -f "$TMUX_CONFIG" new-session -d -s agenmux -c /workspace &&' "$tmp/docker-default.log"
grep -Fq 'client-attached[99]' "$tmp/docker-default.log"
grep -Fq 'toggle split #{q:client_name}' "$tmp/docker-default.log"
! grep -Fq 'new-session -d -s agenmux -c /workspace pi' "$tmp/docker-default.log"
! grep -Fq 'tmux set-option -g @agenmux-bin' "$tmp/docker-default.log"
! grep -Fq 'tmux run-shell "AGENMUX_DIR=$src /tmp/agenmux-target/debug/agenmux setup"' "$tmp/docker-default.log"
! grep -Fq 'tmux send-keys' "$tmp/docker-default.log"
! grep -Fq 'agenmux:0.0' "$tmp/docker-default.log"

touch "$tmp/tmux.conf"
TEST_DOCKER="$tmp/docker-config.log" REF=local TMUX_CONFIG="$tmp/tmux.conf" PATH="$tmp/bin:$PATH" \
  bash "$tmp/plugin/scripts/dev-bin.sh" docker
grep -Fxq "AGENMUX_REF=local" "$tmp/docker-config.log"
grep -Fxq "$tmp/tmux.conf:/root/.tmux.conf:ro" "$tmp/docker-config.log"
grep -Fxq 'TMUX_CONFIG=/root/.tmux.conf' "$tmp/docker-config.log"
grep -Fq 'FROM node:24-trixie-slim' "$DIR/scripts/Dockerfile.dev"
grep -Fq 'apt-get install -y --no-install-recommends bash build-essential ca-certificates curl git ripgrep tmux zsh' "$DIR/scripts/Dockerfile.dev"
grep -Fq 'npm install -g --ignore-scripts @earendil-works/pi-coding-agent' "$DIR/scripts/Dockerfile.dev"
echo 'ok   dev-docker-uses-host-tmux-config'
echo 'ok   stale-dev-switch-binary-recovers'

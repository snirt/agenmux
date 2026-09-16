#!/usr/bin/env bash
set -euo pipefail

workspace="${AGENMUX_WORKSPACE:-/workspace}"
cd "$workspace"

prepare_tmux_conf() {
  tmux_conf=/dev/null
  if [ -n "${AGENMUX_CONTAINER_TMUX_CONF:-}" ]; then
    [ -f "$AGENMUX_CONTAINER_TMUX_CONF" ] || {
      printf 'agenmux: tmux config is not a file: %s\n' "$AGENMUX_CONTAINER_TMUX_CONF" >&2
      exit 1
    }
    awk '
      tolower($0) ~ /(agenmux|agents[-_]mon)/ { next }
      tolower($0) ~ /tpm\/tpm/ { next }
      tolower($0) ~ /^[[:space:]]*(set|set-option)[[:space:]].*@plugin([[:space:]]|$)/ { next }
      /^[[:space:]]*(set|set-option)[[:space:]].*default-(shell|command)([[:space:]]|$)/ { next }
      { print }
      END {
        print "set -g default-shell /bin/bash"
        print "set -g default-command \"\""
      }
    ' "$AGENMUX_CONTAINER_TMUX_CONF" >"$HOME/.tmux.conf"
    tmux_conf="$HOME/.tmux.conf"
  fi
}

case "${1:-test}" in
test)
  bash tests/sanity.sh
  ;;
use)
  export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/agenmux-target}"
  cargo build --release --locked
  harness="$(mktemp -d /tmp/agenmux-container.XXXXXX)"
  socket="agenmux-container-$$"
  cleanup() {
    tmux -L "$socket" kill-server 2>/dev/null || true
    rm -rf "$harness"
  }
  trap cleanup EXIT HUP INT TERM
  mkdir -p "$harness/home" "$harness/config" "$harness/state"
  rustc tests/helpers/fake-agent.rs -o "$harness/codex"
  export HOME="$harness/home"
  export XDG_CONFIG_HOME="$harness/config"
  export XDG_STATE_HOME="$harness/state"
  export TERM="${TERM:-xterm-256color}"
  prepare_tmux_conf
  mkdir -p "$XDG_CONFIG_HOME/agenmux"
  printf '[tmux_management]\nenabled = true\n' \
    >"$XDG_CONFIG_HOME/agenmux/config.toml"
  tmux -L "$socket" -f "$tmux_conf" new-session -d -s agenmux -n shell \
    -x 120 -y 40 -c "$workspace" /bin/bash
  tmux -L "$socket" new-window -d -t agenmux: -n mock-agent \
    -c "$workspace" "$harness/codex"
  tmux -L "$socket" set-option -p -t agenmux:mock-agent allow-rename off
  tmux -L "$socket" select-pane -t agenmux:mock-agent -T 'Action Required'
  tmux -L "$socket" select-window -t agenmux:shell
  tmux -L "$socket" set-option -g mouse on
  tmux -L "$socket" set-option -g @agenmux-bin "$CARGO_TARGET_DIR/release/agenmux"
  tmux -L "$socket" run-shell "bash '$workspace/agenmux.tmux'"
  printf '%s\n' \
    'Interactive Agenmux harness' \
    '  starts in a real shell; mock Codex runs in window 1' \
    '  tmux management enabled: cc window, cs session, r rename, dd delete' \
    '  prefix+A  open/enter the sidebar' \
    '  prefix+a  open the popup' \
    '  detach or exit tmux to stop the container'
  tmux -L "$socket" attach-session -t agenmux
  ;;
install | install-local)
  harness="$(mktemp -d /tmp/agenmux-install.XXXXXX)"
  socket="agenmux-install-$$"
  if [ "$1" = install-local ]; then
    local_installer=1
    installer_label="current checkout installer"
  else
    local_installer=""
    installer="curl -fsSL https://snirt.github.io/agenmux/install.sh | sh"
    installer_label="public website installer"
  fi
  cleanup() {
    tmux -L "$socket" kill-server 2>/dev/null || true
    rm -rf "$harness"
  }
  trap cleanup EXIT HUP INT TERM
  mkdir -p "$harness/home" "$harness/config" "$harness/state"
  if [ -n "$local_installer" ]; then
    mkdir -p "$harness/repo"
    tar --exclude=.git --exclude=.pi --exclude=target -C "$workspace" -cf - . |
      tar -C "$harness/repo" -xf -
    git -C "$harness/repo" init -q -b main
    git -C "$harness/repo" add -A
    git -C "$harness/repo" -c user.name=fixture -c user.email=fixture@example.invalid \
      commit -q -m fixture
    installer="AGENMUX_REPO='$harness/repo' sh '$workspace/install.sh'"
  fi
  rustc tests/helpers/fake-agent.rs -o "$harness/codex"
  export HOME="$harness/home"
  export XDG_CONFIG_HOME="$harness/config"
  export XDG_STATE_HOME="$harness/state"
  export TERM="${TERM:-xterm-256color}"
  prepare_tmux_conf
  mkdir -p "$XDG_CONFIG_HOME/agenmux"
  printf '[tmux_management]\nenabled = true\n' \
    >"$XDG_CONFIG_HOME/agenmux/config.toml"
  tmux -L "$socket" -f "$tmux_conf" new-session -d -s agenmux -n shell \
    -x 120 -y 40 -c "$HOME" /bin/bash
  tmux -L "$socket" new-window -d -t agenmux: -n mock-agent \
    -c "$HOME" "$harness/codex"
  tmux -L "$socket" set-option -p -t agenmux:mock-agent allow-rename off
  tmux -L "$socket" select-pane -t agenmux:mock-agent -T 'Action Required'
  tmux -L "$socket" select-window -t agenmux:shell
  tmux -L "$socket" set-option -g mouse on
  tmux -L "$socket" send-keys -t agenmux:shell "$installer" Enter
  printf '%s\n' \
    'Full installer harness' \
    "  running the $installer_label in a clean HOME" \
    '  answer its prompts, then use prefix+A to open the sidebar' \
    '  mock Codex runs in window 1' \
    '  detach or exit tmux to stop the container'
  tmux -L "$socket" attach-session -t agenmux
  ;;
shell)
  exec bash
  ;;
*)
  exec "$@"
  ;;
esac

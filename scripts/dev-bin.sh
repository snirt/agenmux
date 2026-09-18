#!/usr/bin/env bash
set -u

DIR="$(cd "$(dirname "$0")/.." && pwd)"
DEBUG="$DIR/target/debug/agenmux"
CANONICAL_RELEASE="$DIR/target/release/agenmux"
LEGACY_RELEASE="$DIR/target/release/agents-mon"
RELEASE="$CANONICAL_RELEASE"
[ -x "$RELEASE" ] || RELEASE="$LEGACY_RELEASE"
ACTION="${1:-}"

if [ "$ACTION" = docker ]; then
  ref="${REF:-local}"
  config="${TMUX_CONFIG:-$HOME/.tmux.conf}"
  [ -f "$config" ] || { echo "agenmux: tmux config not found: $config" >&2; exit 1; }
  [ -L "$config" ] && config="$(realpath "$config")"
  docker build -t agenmux-dev -f "$DIR/scripts/Dockerfile.dev" "$DIR/scripts" || exit 1
  case "${XDG_CONFIG_HOME:-}" in
    /*) app_config="$XDG_CONFIG_HOME/agenmux" ;;
    *) app_config="$HOME/.config/agenmux" ;;
  esac
  config_file="$app_config/config.toml"
  [ -L "$config_file" ] && config_file="$(realpath "$config_file")"
  agents_dir="$app_config/agents"
  [ -L "$agents_dir" ] && agents_dir="$(realpath "$agents_dir")"
  docker_args=(create --rm -it -e TERM=xterm-256color -e COLORTERM=truecolor -e "AGENMUX_REF=$ref" -e TMUX_CONFIG=/root/.tmux.conf -v "$DIR:/workspace" -v agenmux-pi-home:/root/.pi/agent -v agenmux-cargo-registry:/root/.cargo/registry -v agenmux-cargo-git:/root/.cargo/git -v agenmux-build-cache:/tmp/agenmux-target)
  docker_args+=(-w /workspace)
  container="$(docker "${docker_args[@]}" agenmux-dev bash -lc '
    mkdir -p /root/.config/agenmux &&
    cp /tmp/agenmux-tmux.conf /root/.tmux.conf &&
    if [ -f /tmp/agenmux-config.toml ]; then cp /tmp/agenmux-config.toml /root/.config/agenmux/config.toml && chmod u+rw /root/.config/agenmux/config.toml; fi &&
    if [ -d /tmp/agenmux-agents ]; then cp -R /tmp/agenmux-agents /root/.config/agenmux/agents && chmod -R u+rwX /root/.config/agenmux/agents; fi &&
    if [ "$AGENMUX_REF" = local ]; then
      src=/workspace
    else
      git clone --depth 1 --branch "$AGENMUX_REF" https://github.com/snirt/agenmux /tmp/agenmux &&
      src=/tmp/agenmux
    fi &&
    CARGO_TARGET_DIR=/tmp/agenmux-target cargo build --manifest-path "$src/Cargo.toml" &&
    sed -E "/(agents-mon|agenmux)\.tmux/d; /^[[:space:]]*(set|set-option)[[:space:]].*default-(shell|command)([[:space:]]|$)/d; s/(choose-tree[[:space:]]+-[A-Za-z]*)y([A-Za-z]*)/\1\2/g; s/,(width=[^,\"]+|align=[^,\"]+)//g" "$TMUX_CONFIG" >/tmp/tmux.conf &&
    echo "set -g default-shell /bin/bash" >>/tmp/tmux.conf &&
    echo "set -g default-terminal tmux-256color" >>/tmp/tmux.conf &&
    echo "set -as terminal-features ,xterm-256color:RGB" >>/tmp/tmux.conf &&
    echo "set -g @agenmux-bin /tmp/agenmux-target/debug/agenmux" >>/tmp/tmux.conf &&
    printf "\033[2J\033[H" &&
    AGENMUX_DIR="$src" AGENMUX_SKIP_UPDATE=1 AGENMUX_FORCE_WIZARD=1 AGENMUX_TMUX_CONF=/tmp/tmux.conf sh /workspace/install.sh &&
    TMUX_CONFIG=/tmp/tmux.conf &&
    tmux -f "$TMUX_CONFIG" new-session -d -s agenmux -c /workspace &&
    tmux set-hook -g "client-attached[99]" "run-shell -b \"AGENMUX_DIR=$src /tmp/agenmux-target/debug/agenmux toggle split #{q:client_name}; tmux set-hook -gu client-attached[99]\"" &&
    exec tmux attach -t agenmux
  ')" || exit 1
  trap 'docker rm -f "$container" >/dev/null 2>&1 || true' EXIT
  docker cp "$config" "$container:/tmp/agenmux-tmux.conf" || exit 1
  [ ! -f "$config_file" ] || docker cp "$config_file" "$container:/tmp/agenmux-config.toml" || exit 1
  [ ! -d "$agents_dir" ] || docker cp "$agents_dir" "$container:/tmp/agenmux-agents" || exit 1
  docker start -ai "$container"
  exit $?
fi

case "$ACTION" in
use)
  AGENMUX_DEV_BUILD_ID="$$-$(date +%s)" \
    cargo build --manifest-path "$DIR/Cargo.toml" || exit 1
  next="$DEBUG"
  ;;
stop) next="$RELEASE" ;;
*)
  echo "usage: $0 use|docker|stop" >&2
  exit 2
  ;;
esac
[ -x "$next" ] || {
  echo "agenmux: binary not found: $next" >&2
  exit 1
}

old_option_name=""
if [ -n "$(tmux show-options -gq @agenmux-bin)" ]; then
  old_option_name="@agenmux-bin"
elif [ -n "$(tmux show-options -gq @agents-mon-bin)" ]; then
  old_option_name="@agents-mon-bin"
fi
old_option=""
[ -z "$old_option_name" ] || old_option="$(tmux show-option -gqv "$old_option_name")"
current="${old_option:-$RELEASE}"
was_on="$(tmux show-option -gqv @agenmux-on)"
[ -n "$was_on" ] || was_on="$(tmux show-option -gqv @agents-mon-on)"
old_control="$(tmux show-option -gqv @agenmux-control-client)"
[ -n "$old_control" ] || old_control="$(tmux show-option -gqv @agents-mon-control-client)"

select_bin() {
  tmux set-option -gu @agents-mon-bin 2>/dev/null || true
  if [ "$1" = "$RELEASE" ]; then
    tmux set-option -gu @agenmux-bin
  else
    tmux set-option -g @agenmux-bin "$1"
  fi
}

start_bin() {
  AGENMUX_DIR="$DIR" "$1" setup &&
    { [ -z "$was_on" ] || AGENMUX_DIR="$DIR" "$1" toggle; }
}

if [ -x "$current" ]; then
  "$current" teardown || exit 1
  if [ -n "$old_control" ]; then
    for ((i = 0; i < 80; i++)); do
      tmux list-clients -F '#{client_name}' 2>/dev/null | grep -Fxq "$old_control" || break
      sleep 0.1
    done
  fi
fi

select_bin "$next" || exit 1
if ! start_bin "$next"; then
  "$next" teardown >/dev/null 2>&1 || true
  tmux set-option -gu @agenmux-bin 2>/dev/null || true
  tmux set-option -gu @agents-mon-bin 2>/dev/null || true
  if [ -x "$current" ]; then
    [ -z "$old_option_name" ] || tmux set-option -g "$old_option_name" "$old_option"
    start_bin "$current" >/dev/null 2>&1 || true
    echo "agenmux: switch failed; restored previous binary" >&2
  else
    echo "agenmux: switch failed; previous binary unavailable" >&2
  fi
  exit 1
fi

printf 'agenmux: using %s\n' "$next"

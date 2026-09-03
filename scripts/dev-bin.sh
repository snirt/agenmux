#!/usr/bin/env bash
set -u

DIR="$(cd "$(dirname "$0")/.." && pwd)"
DEBUG="$DIR/target/debug/agenmux"
CANONICAL_RELEASE="$DIR/target/release/agenmux"
LEGACY_RELEASE="$DIR/target/release/agents-mon"
RELEASE="$CANONICAL_RELEASE"
[ -x "$RELEASE" ] || RELEASE="$LEGACY_RELEASE"
ACTION="${1:-}"

case "$ACTION" in
use)
  cargo build --manifest-path "$DIR/Cargo.toml" || exit 1
  next="$DEBUG"
  ;;
stop) next="$RELEASE" ;;
*)
  echo "usage: $0 use|stop" >&2
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
[ -x "$current" ] || {
  echo "agenmux: current binary not found: $current" >&2
  exit 1
}
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

"$current" teardown || exit 1
if [ -n "$old_control" ]; then
  for ((i = 0; i < 80; i++)); do
    tmux list-clients -F '#{client_name}' 2>/dev/null | grep -Fxq "$old_control" || break
    sleep 0.1
  done
fi

select_bin "$next" || exit 1
if ! start_bin "$next"; then
  "$next" teardown >/dev/null 2>&1 || true
  tmux set-option -gu @agenmux-bin 2>/dev/null || true
  tmux set-option -gu @agents-mon-bin 2>/dev/null || true
  [ -z "$old_option_name" ] || tmux set-option -g "$old_option_name" "$old_option"
  start_bin "$current" >/dev/null 2>&1 || true
  echo "agenmux: switch failed; restored previous binary" >&2
  exit 1
fi

printf 'agenmux: using %s\n' "$next"

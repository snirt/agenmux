#!/usr/bin/env bash

# Install and execute the published binary, then build this checkout and run it
# through the plugin in an isolated tmux server.
set -euo pipefail

DIR="$(cd "$(dirname "$0")/.." && pwd)"
if [ "${AGENTS_MON_SANITY_NIX:-}" != 1 ]; then
  exec nix-shell "$DIR/tests/sanity.nix" \
    --run "AGENTS_MON_SANITY_NIX=1 bash '$DIR/tests/sanity.sh'"
fi

root="$(mktemp -d "${TMPDIR:-/tmp}/agents-mon-sanity.XXXXXX")"
plugin="$root/plugin"
active_socket=""
started=$SECONDS

cleanup() {
  [ -z "$active_socket" ] || tmux -L "$active_socket" kill-server 2>/dev/null || true
  rm -rf "$root"
}
trap cleanup EXIT

mkdir -p "$plugin" "$root/home" "$root/tmp" "$root/bin"
cp -R "$DIR/agents" "$DIR/scripts" "$plugin/"
cp "$DIR/agents-mon.tmux" "$plugin/agents-mon.tmux"
# install-bin.sh resolves the release to fetch from the manifest version
cp "$DIR/Cargo.toml" "$plugin/Cargo.toml"
export HOME="$root/home"
export XDG_CONFIG_HOME="$root/home/.config"
export TMPDIR="$root/tmp"
export TERM=xterm-256color
case "$(tmux -V)" in
  'tmux 3.7'*) ;;
  *) printf 'FAIL: tmux 3.7 required, found %s\n' "$(tmux -V)" >&2; exit 1 ;;
esac

# A real executable name identifies Codex; the pane title supplies its state.
rustc "$DIR/tests/helpers/fake-agent.rs" -o "$root/bin/codex"

run_tmux_case() {
  local name="$1" bin="$2" socket
  local socket_path server_pid tmux_env rows status sidebar frame i
  socket="agents-mon-sanity-$name-$$"
  active_socket="$socket"

  tmux -L "$socket" -f /dev/null new-session -d -s sanity -x 100 -y 30 \
    -c "$plugin" "$root/bin/codex"
  tmux -L "$socket" set-option -p allow-rename off
  tmux -L "$socket" select-pane -T 'Action Required'
  tmux -L "$socket" set-option -g @agents-mon-bin "$bin"
  tmux -L "$socket" set-option -g status-right '#{agents_mon}'
  tmux -L "$socket" run-shell "bash '$plugin/agents-mon.tmux'"

  tmux -L "$socket" list-keys -T prefix \
    | grep -Fq '/scripts/toggle.sh'
  socket_path="$(tmux -L "$socket" display-message -p '#{socket_path}')"
  server_pid="$(tmux -L "$socket" display-message -p '#{pid}')"
  tmux_env="$socket_path,$server_pid,0"

  rows=""
  i=0
  while [ "$i" -lt 50 ]; do
    rows="$(TMUX="$tmux_env" "$bin" list)"
    case "$rows" in *$'\tcodex\tblocked\t'*) break ;; esac
    sleep 0.1
    i=$((i + 1))
  done
  case "$rows" in
    *$'\tcodex\tblocked\t'*) ;;
    *)
      printf 'FAIL %s: Codex blocked row not found\n%s\n' "$name" "$rows" >&2
      tmux -L "$socket" list-panes -a -F '#{pane_id} #{pane_pid} #{pane_current_command} #{pane_title}' >&2
      return 1
      ;;
  esac

  status="$(TMUX="$tmux_env" "$bin" status)"
  [ "$status" = '#[fg=red]⣿#[default]1' ] || {
    printf 'FAIL %s: unexpected status: %s\n' "$name" "$status" >&2
    return 1
  }

  tmux -L "$socket" run-shell "bash '$plugin/scripts/toggle.sh'"
  frame=""
  i=0
  while [ "$i" -lt 50 ]; do
    # mirror mode marks panes by title (no @agents-mon-sidebar option)
    sidebar="$(tmux -L "$socket" list-panes -a -F '#{pane_id}	#{pane_title}' \
      | awk -F'\t' '$2 == "agents-mon" { print $1; exit }')"
    if [ -n "$sidebar" ]; then
      frame="$(tmux -L "$socket" capture-pane -p -t "$sidebar" 2>/dev/null || true)"
      printf '%s\n' "$frame" | grep -Fq codex && break
    fi
    sleep 0.1
    i=$((i + 1))
  done
  printf '%s\n' "$frame" | grep -Fq codex || {
    printf 'FAIL %s: sidebar did not render Codex\n%s\n' "$name" "$frame" >&2
    return 1
  }

  tmux -L "$socket" kill-server
  active_socket=""
  printf 'ok   %s binary in real tmux\n' "$name"
}

# Clean checkout: source the TPM entrypoint and activate immediately, before a
# native engine exists. The toggle must wait for the eager verified installer
# and then open the requested split in the same action. Popup lock/failure paths
# use deterministic command stubs in tests/run.sh.
bootstrap_socket="agents-mon-sanity-bootstrap-$$"
active_socket="$bootstrap_socket"
tmux -L "$bootstrap_socket" -f /dev/null new-session -d -s bootstrap -x 100 -y 30 \
  -c "$plugin" "$root/bin/codex"
tmux -L "$bootstrap_socket" set-option -g status-right '#{agents_mon}'
tmux -L "$bootstrap_socket" run-shell "bash '$plugin/agents-mon.tmux'"
bootstrap_path="$(tmux -L "$bootstrap_socket" display-message -p '#{socket_path}')"
bootstrap_pid="$(tmux -L "$bootstrap_socket" display-message -p '#{pid}')"
env TMPDIR="$TMPDIR" TMUX="$bootstrap_path,$bootstrap_pid,0" \
  bash "$plugin/scripts/toggle.sh"
for _ in $(seq 1 80); do
  tmux -L "$bootstrap_socket" list-panes -a -F '#{pane_title}' | grep -qx agents-mon && break
  sleep 0.1
done
[ -x "$plugin/target/release/agents-mon" ]
tmux -L "$bootstrap_socket" list-panes -a -F '#{pane_title}' | grep -qx agents-mon
tmux -L "$bootstrap_socket" show-option -gqv status-right | grep -Fq 'agents-mon status'
printf 'ok   clean checkout first activation installs and opens native split\n'
tmux -L "$bootstrap_socket" kill-server
active_socket=""

phase=$SECONDS
bash "$plugin/scripts/install-bin.sh"
download_seconds=$((SECONDS - phase))
downloaded="$plugin/target/release/agents-mon"
[ -x "$downloaded" ]
[ -s "$plugin/target/release/.agents-mon-version" ]
# the sidebar's update notice and version picker read these
[ -s "$plugin/target/release/.agents-mon-latest" ]
[ -s "$plugin/target/release/.agents-mon-tags" ]
state="$("$downloaded" detect "$plugin/agents/codex.conf" "$DIR/tests/fixtures/codex-blocked.txt")"
[ "$state" = blocked ]
printf 'ok   downloaded binary verified and executed\n'
printf 'time download: %ss\n' "$download_seconds"

phase=$SECONDS
CARGO_HOME="$root/cargo" CARGO_TARGET_DIR="$root/build" \
  cargo build --release --locked --manifest-path "$DIR/Cargo.toml"
build_seconds=$((SECONDS - phase))
mkdir -p "$plugin/target/source"
cp "$root/build/release/agents-mon" "$plugin/target/source/agents-mon"

phase=$SECONDS
run_tmux_case source "$plugin/target/source/agents-mon"
tmux_seconds=$((SECONDS - phase))
printf 'time source build: %ss\n' "$build_seconds"
printf 'time real tmux: %ss\n' "$tmux_seconds"
printf 'time total: %ss\n' "$((SECONDS - started))"

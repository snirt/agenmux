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
cp -R "$DIR/agents" "$DIR/scripts" "$DIR/src" "$plugin/"
cp "$DIR/agents-mon.tmux" "$DIR/Cargo.toml" "$DIR/Cargo.lock" "$plugin/"
export HOME="$root/home"
export XDG_CONFIG_HOME="$root/home/.config"
export TMPDIR="$root/tmp"
export TERM=xterm-256color
case "$(tmux -V)" in
'tmux 3.7'*) ;;
*)
  printf 'FAIL: tmux 3.7 required, found %s\n' "$(tmux -V)" >&2
  exit 1
  ;;
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

  tmux -L "$socket" list-keys -T prefix |
    grep -Fq '/agents-mon.tmux' \
    && tmux -L "$socket" list-keys -T prefix | grep -Fq ' activate '
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

  TMUX="$tmux_env" AGENTS_MON_DIR="$plugin" "$bin" toggle split
  frame=""
  i=0
  while [ "$i" -lt 50 ]; do
    # mirror mode marks panes by title (no @agents-mon-sidebar option)
    sidebar="$(tmux -L "$socket" list-panes -a -F '#{pane_id}	#{pane_title}' |
      awk -F'\t' '$2 == "agents-mon" { print $1; exit }')"
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

run_immediate_popup_bootstrap() { # verified|cargo|bad-checksum
  local mode="$1" case_root="$root/popup-$1" case_plugin="$root/popup-$1/plugin"
  local case_bin="$root/popup-$1/bin" downloads="$root/popup-$1/downloads"
  local marker="$root/popup-$1/opened" socket="agents-mon-popup-$1-$$"
  local package tag archive expect_pid opened=0 i
  mkdir -p "$case_plugin" "$case_bin" "$downloads"
  cp -R "$DIR/agents" "$DIR/scripts" "$case_plugin/"
  cp "$DIR/agents-mon.tmux" "$DIR/Cargo.toml" "$case_plugin/"
  tag="$(bash "$DIR/scripts/version.sh" tag)"
  case "$(uname -s):$(uname -m)" in
    Darwin:arm64) package=tmux-agents-mon-macos-aarch64 ;;
    Darwin:x86_64) package=tmux-agents-mon-macos-x86_64 ;;
    Linux:aarch64 | Linux:arm64) package=tmux-agents-mon-linux-aarch64 ;;
    Linux:x86_64 | Linux:amd64) package=tmux-agents-mon-linux-x86_64 ;;
    *) printf 'FAIL popup bootstrap: unsupported platform\n' >&2; return 1 ;;
  esac

  if [ "$mode" = cargo ]; then
    cat >"$case_plugin/Cargo.toml" <<TOML
[package]
name = "agents-mon"
version = "${tag#v}"
edition = "2021"
TOML
    mkdir -p "$case_plugin/src"
    cat >"$case_plugin/src/main.rs" <<RS
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.get(0).map(String::as_str) == Some("--version") {
        println!("agents-mon ${tag#v}");
    } else if args.get(0).map(String::as_str) == Some("toggle")
        && args.get(1).map(String::as_str) == Some("popup")
    {
        std::fs::write("$marker", "opened").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
}
RS
    printf '#!/usr/bin/env bash\nexit 1\n' >"$case_bin/curl"
    printf '#!/usr/bin/env bash\nexit 1\n' >"$case_bin/git"
  else
    mkdir -p "$downloads/$tag/$package/target/release"
    cat >"$downloads/$tag/$package/target/release/agents-mon" <<SH
#!/usr/bin/env bash
if [ "\${1:-}" = --version ]; then
  printf 'agents-mon ${tag#v}\n'
elif [ "\${1:-}" = toggle ] && [ "\${2:-}" = popup ]; then
  printf opened >"$marker"
  sleep 0.3
fi
SH
    chmod +x "$downloads/$tag/$package/target/release/agents-mon"
    archive="$downloads/$tag/$package.tar.gz"
    tar -czf "$archive" -C "$downloads/$tag" "$package"
    rm -rf "$downloads/$tag/$package"
    if [ "$mode" = bad-checksum ]; then
      printf '%064d  ./%s.tar.gz\n' 0 "$package" >"$downloads/$tag/SHA256SUMS"
      printf '#!/usr/bin/env bash\nexit 1\n' >"$case_bin/cargo"
    elif command -v sha256sum >/dev/null; then
      (cd "$downloads/$tag" && sha256sum "./$package.tar.gz" >SHA256SUMS)
    else
      (cd "$downloads/$tag" && shasum -a 256 "./$package.tar.gz" >SHA256SUMS)
    fi
    cat >"$case_bin/curl" <<'SH'
#!/usr/bin/env bash
url=""; out=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o) shift; out="$1" ;;
    http*) url="$1" ;;
  esac
  shift
done
case "$url" in
  */releases/latest) printf '%s/tag/%s' "$url" "$BOOTSTRAP_TAG" ;;
  *) cp "$BOOTSTRAP_DOWNLOADS/$BOOTSTRAP_TAG/${url##*/}" "$out" ;;
esac
SH
    printf '#!/usr/bin/env bash\nexit 1\n' >"$case_bin/git"
  fi
  chmod +x "$case_bin"/*

  PATH="$case_bin:$PATH" BOOTSTRAP_TAG="$tag" BOOTSTRAP_DOWNLOADS="$downloads" \
    tmux -L "$socket" -f /dev/null new-session -d -s popup -x 100 -y 30
  active_socket="$socket"
  tmux -L "$socket" set-environment -g PATH "$case_bin:$PATH"
  tmux -L "$socket" set-environment -g BOOTSTRAP_TAG "$tag"
  tmux -L "$socket" set-environment -g BOOTSTRAP_DOWNLOADS "$downloads"
  [ -z "${RUSTUP_HOME:-}" ] || tmux -L "$socket" set-environment -g RUSTUP_HOME "$RUSTUP_HOME"
  tmux -L "$socket" set-environment -g CARGO_HOME "$case_root/cargo-home"
  tmux -L "$socket" set-environment -g CARGO_TARGET_DIR "$case_plugin/target"
  tmux -L "$socket" set-option -g @agents-mon-popup-key e
  tmux -L "$socket" run-shell "bash '$case_plugin/agents-mon.tmux'"
  expect -c "log_user 0; set timeout -1; spawn tmux -L $socket attach-session -t popup; after 50; send \\002e; expect eof" &
  expect_pid=$!

  for i in $(seq 1 600); do
    if [ -e "$marker" ]; then opened=1; break; fi
    if [ "$mode" = bad-checksum ] && tmux -L "$socket" show-messages 2>/dev/null \
        | grep -Fq 'agents-mon: native engine installation failed'; then
      break
    fi
    sleep 0.1
  done
  kill "$expect_pid" 2>/dev/null || true
  wait "$expect_pid" 2>/dev/null || true

  if [ "$mode" = bad-checksum ]; then
    [ ! -e "$case_plugin/target/release/agents-mon" ]
    [ ! -e "$marker" ]
    tmux -L "$socket" show-messages \
      | grep -Fq 'agents-mon: native engine installation failed'
  else
    [ "$opened" -eq 1 ]
    [ -x "$case_plugin/target/release/agents-mon" ]
  fi
  tmux -L "$socket" kill-server 2>/dev/null || true
  active_socket=""
  printf 'ok   source then immediate popup (%s)\n' "$mode"
}

run_immediate_popup_bootstrap verified
run_immediate_popup_bootstrap cargo
run_immediate_popup_bootstrap bad-checksum

# Clean checkout: source the TPM entrypoint and activate immediately, before a
# native engine exists. Activation must wait for the eager installer and then
# open the requested split in the same action. Verified, Cargo, and bad-checksum
# popup bootstrap paths are covered above.
bootstrap_socket="agents-mon-sanity-bootstrap-$$"
active_socket="$bootstrap_socket"
# This checkout is ahead of the latest published binary, whose CLI may not yet
# include native toggle. Force the already-covered Cargo fallback so this case
# executes the current source without restoring a Bash runtime path.
mkdir -p "$root/bootstrap-bin"
printf '#!/usr/bin/env bash\nexit 1\n' >"$root/bootstrap-bin/curl"
printf '#!/usr/bin/env bash\nexit 1\n' >"$root/bootstrap-bin/git"
chmod +x "$root/bootstrap-bin/curl" "$root/bootstrap-bin/git"
PATH="$root/bootstrap-bin:$PATH" tmux -L "$bootstrap_socket" -f /dev/null \
  new-session -d -s bootstrap -x 100 -y 30 -c "$plugin" "$root/bin/codex"
tmux -L "$bootstrap_socket" set-environment -g PATH "$root/bootstrap-bin:$PATH"
tmux -L "$bootstrap_socket" set-option -g status-right '#{agents_mon}'
tmux -L "$bootstrap_socket" run-shell "bash '$plugin/agents-mon.tmux'"
bootstrap_path="$(tmux -L "$bootstrap_socket" display-message -p '#{socket_path}')"
bootstrap_pid="$(tmux -L "$bootstrap_socket" display-message -p '#{pid}')"
env PATH="$root/bootstrap-bin:$PATH" TMPDIR="$TMPDIR" \
  TMUX="$bootstrap_path,$bootstrap_pid,0" bash "$plugin/agents-mon.tmux" activate '' ''
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

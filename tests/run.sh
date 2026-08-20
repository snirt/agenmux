#!/usr/bin/env bash
# Asserts detect_state over fixtures: tests/fixtures/<agent>-<state>[-x].txt
# Optional <fixture>.title sidecar supplies the pane title.
DIR="$(cd "$(dirname "$0")/.." && pwd)"
fail=0 count=0

# fixtures run against bash, and against the Rust engine when built —
# both stay honest
engines="bash"
BIN="${AGENTS_MON_BIN:-$DIR/target/release/agents-mon}"
[ -x "$BIN" ] && engines="bash rust"

for engine in $engines; do
  for fx in "$DIR"/tests/fixtures/*.txt; do
    base="$(basename "$fx" .txt)"
    name="$base"
    case "${name##*-}" in
      ''|*[!0-9]*) ;;
      *) name="${name%-*}" ;;
    esac
    agent="${name%-*}"
    expected="${name##*-}"
    title=""
    [ -f "${fx%.txt}.title" ] && title="$(cat "${fx%.txt}.title")"
    if [ "$engine" = rust ]; then
      got="$("$BIN" detect "$DIR/agents/$agent.conf" "$fx" "$title")"
    else
      got="$(bash "$DIR/scripts/scan.sh" detect "$DIR/agents/$agent.conf" "$fx" "$title")"
    fi
    count=$((count + 1))
    if [ "$got" = "$expected" ]; then
      echo "ok   $base ($engine)"
    else
      echo "FAIL $base ($engine): expected $expected, got $got"
      fail=1
    fi
  done
done

echo "$count fixtures"
if [ "$fail" -eq 0 ]; then
  version="$(bash "$DIR/scripts/version.sh")"
  tag="$(bash "$DIR/scripts/version.sh" tag)"
  if [ "$tag" = "v$version" ] \
     && bash "$DIR/scripts/version.sh" check-tag "$tag" \
     && ! bash "$DIR/scripts/version.sh" check-tag "v0.0.0" 2>/dev/null; then
    echo "ok   version-derived-from-cargo-manifest"
  else
    echo "FAIL version-derived-from-cargo-manifest"
    fail=1
  fi
fi
if [ "$fail" -eq 0 ]; then
  tmp="$(mktemp -d)"
  package="tmux-agents-mon-macos-aarch64"
  mkdir -p "$tmp/plugin/scripts" "$tmp/downloads" "$tmp/bin"
  cp "$DIR/scripts/install-bin.sh" "$DIR/scripts/version.sh" \
    "$DIR/scripts/install-app.sh" "$tmp/plugin/scripts/"
  # a release whose engine prints its own tag, so which one got installed is
  # visible in the assertions below
  mk_release() {
    local t="$1" d="$tmp/downloads/$1"
    mkdir -p "$d/$package/target/release"
    printf '#!/usr/bin/env bash\nprintf "%s\\n"\n' "$t" \
      > "$d/$package/target/release/agents-mon"
    printf '#!/usr/bin/env bash\nexit 0\n' \
      > "$d/$package/target/release/agents-mon-notifier"
    chmod +x "$d/$package/target/release/agents-mon" \
      "$d/$package/target/release/agents-mon-notifier"
    tar -czf "$d/$package.tar.gz" -C "$d" "$package"
    rm -rf "${d:?}/$package"
    if command -v sha256sum >/dev/null; then
      (cd "$d" && sha256sum "./$package.tar.gz" > SHA256SUMS)
    else
      (cd "$d" && shasum -a 256 "./$package.tar.gz" > SHA256SUMS)
    fi
  }
  set_version() {
    printf '[package]\nname = "agents-mon"\nversion = "%s"\n' "$1" \
      > "$tmp/plugin/Cargo.toml"
    # a stale marker forces a check past the once-a-day throttle
    printf 'v0.0.0\nold-revision\n' > "$tmp/plugin/target/release/.agents-mon-version"
  }
  install_bin() {
    # HOME sandbox: sync_app must never touch the real ~/Applications
    DOWNLOADS="$tmp/downloads" LATEST_TAG="$1" PATH="$tmp/bin:$PATH" \
      HOME="$tmp/home" bash "$tmp/plugin/scripts/install-bin.sh"
  }
  mk_release v0.1.0
  mk_release v0.1.1
  cat > "$tmp/bin/uname" <<'SH'
#!/usr/bin/env bash
[ "$1" = "-s" ] && printf 'Darwin\n' || printf 'arm64\n'
SH
  cat > "$tmp/bin/curl" <<'SH'
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
  */releases/latest) printf '%s/tag/%s' "$url" "$LATEST_TAG" ;;
  *)
    file="${url##*/}"; rest="${url%/*}"; tag="${rest##*/}"
    [ -f "$DOWNLOADS/$tag/$file" ] || exit 22   # no such release asset
    cp "$DOWNLOADS/$tag/$file" "$out"
    ;;
esac
SH
  # rev-parse must fail (no repo); ls-remote feeds the version picker.
  # v0.1.2 is a tag with no release behind it (build still running, or failed):
  # it must never reach the recorded list, or the picker offers a version
  # whose binaries do not exist.
  cat > "$tmp/bin/git" <<'SH'
#!/usr/bin/env bash
case "$*" in
  *ls-remote*) printf 'ccc\trefs/tags/v0.1.2\naaa\trefs/tags/v0.1.1\nbbb\trefs/tags/v0.1.0\n' ;;
  *) exit 1 ;;
esac
SH
  chmod +x "$tmp/bin/uname" "$tmp/bin/curl" "$tmp/bin/git"
  engine() { "$tmp/plugin/target/release/agents-mon" 2>/dev/null; }
  marker() { sed -n '1p' "$tmp/plugin/target/release/.agents-mon-version" 2>/dev/null; }

  mkdir -p "$tmp/plugin/target/release"
  # 1. the engine follows the checkout's own version, not the newest release
  set_version 0.1.0
  install_bin v0.1.1
  if [ "$(engine)" = "v0.1.0" ] && [ "$(marker)" = "v0.1.0" ] \
     && [ -x "$tmp/plugin/target/release/agents-mon-notifier" ] \
     && [ -f "$tmp/home/Applications/AgentsMon.app/Contents/MacOS/agents-mon-notifier" ] \
     && [ "$(sed -n '1p' "$tmp/plugin/target/release/.agents-mon-latest")" = "v0.1.1" ] \
     && [ "$(sed -n '1p' "$tmp/plugin/target/release/.agents-mon-tags")" = "v0.1.1" ]; then
    echo "ok   native-engine-matches-checkout-version"
  else
    echo "FAIL native-engine-matches-checkout-version: got $(engine)/$(marker)"
    fail=1
  fi
  # 2. a checkout ahead of every release (master) falls back to the newest one
  set_version 0.9.9
  install_bin v0.1.1
  if [ "$(engine)" = "v0.1.1" ] && [ "$(marker)" = "v0.1.1" ]; then
    echo "ok   native-engine-falls-back-to-latest-release"
  else
    echo "FAIL native-engine-falls-back-to-latest-release: got $(engine)/$(marker)"
    fail=1
  fi
  # 3. rolling the source back pins the matching engine — no extra state
  set_version 0.1.0
  install_bin v0.1.1
  if [ "$(engine)" = "v0.1.0" ] && [ "$(marker)" = "v0.1.0" ]; then
    echo "ok   native-engine-follows-rollback"
  else
    echo "FAIL native-engine-follows-rollback: got $(engine)/$(marker)"
    fail=1
  fi
  # 4. an install with nothing to do still learns what is released. The
  #    regression: the release check sat behind the engine check, so a healthy
  #    up-to-date install had no notice and an empty picker for a whole day.
  rm -f "$tmp/plugin/target/release/.agents-mon-latest" \
        "$tmp/plugin/target/release/.agents-mon-tags"
  printf 'v0.1.0\n-\n' > "$tmp/plugin/target/release/.agents-mon-version"
  install_bin v0.1.1
  if [ "$(sed -n '1p' "$tmp/plugin/target/release/.agents-mon-latest")" = "v0.1.1" ] \
     && [ "$(sed -n '1p' "$tmp/plugin/target/release/.agents-mon-tags")" = "v0.1.1" ]; then
    echo "ok   release-list-recorded-when-engine-is-current"
  else
    echo "FAIL release-list-recorded-when-engine-is-current: no release list written"
    fail=1
  fi
  rm -rf "$tmp"
fi
if [ "$fail" -eq 0 ]; then
  # codex subjects come from ~/.codex rollouts, not the screen. The regression:
  # only the 20 newest rollouts were searched, so one busy worktree filled the
  # window and every other codex pane showed no subject at all.
  tmp="$(mktemp -d)"
  day="$tmp/home/.codex/sessions/2026/01/01"
  mkdir -p "$day"
  roll() { # <name> <cwd> <first user text>
    printf '{"cwd":"%s","id":"%s"}\n' "$2" "$1" > "$day/rollout-$1.jsonl"
    printf '{"role":"user","content":[{"text":"<environment_context>\\n x"}]}\n' \
      >> "$day/rollout-$1.jsonl"
    printf '{"role":"user","content":[{"text":"%s"}]}\n' "$3" >> "$day/rollout-$1.jsonl"
  }
  # oldest: the only rollout for /want — 25 newer ones for a busy worktree bury it
  roll 000-target /want "the real prompt"
  for i in $(seq -w 1 25); do
    roll "$i-busy" /busy "noise $i"
  done
  # a rollout whose first user message is codex's injected AGENTS.md preamble
  roll 900-agentsmd /withagents "# AGENTS.md instructions for /withagents"
  printf '{"role":"user","content":[{"text":"what the user actually asked"}]}\n' \
    >> "$day/rollout-900-agentsmd.jsonl"
  subj() ( . "$DIR/agents/codex.conf"; HOME="$tmp/home" path="$1" bash -c "$SUBJECT_CMD" )
  buried="$(subj /want)"
  agentsmd="$(subj /withagents)"
  none="$(subj /no/such/dir)"
  if [ "$buried" = "the real prompt" ] \
     && [ "$agentsmd" = "what the user actually asked" ] \
     && [ -z "$none" ]; then
    echo "ok   codex-subject-survives-a-busy-worktree"
  else
    echo "FAIL codex-subject-survives-a-busy-worktree: buried=[$buried] agentsmd=[$agentsmd] none=[$none]"
    fail=1
  fi
  rm -rf "$tmp"
fi
if [ "$fail" -eq 0 ]; then
  tmp="$(mktemp -d)"
  mkdir -p "$tmp/bin" "$tmp/repo/scripts"
  for s in update.sh version.sh install-bin.sh teardown.sh; do
    cp "$DIR/scripts/$s" "$tmp/repo/scripts/"
  done
  # offline: no releases to fetch, no local build, no tmux server to restart
  printf '#!/usr/bin/env bash\nexit 1\n' > "$tmp/bin/curl"
  printf '#!/usr/bin/env bash\nexit 1\n' > "$tmp/bin/cargo"
  cat > "$tmp/bin/tmux" <<'SH'
#!/usr/bin/env bash
[ "$1" = "info" ] && exit 1   # no server: update.sh skips the restart
exit 0
SH
  chmod +x "$tmp/bin/curl" "$tmp/bin/cargo" "$tmp/bin/tmux"
  (
    cd "$tmp/repo" || exit 1
    git init -q . && git config user.email t@t && git config user.name t
    printf '[package]\nname = "agents-mon"\nversion = "0.1.0"\n' > Cargo.toml
    git add -A && git commit -qm one && git tag v0.1.0
    printf '[package]\nname = "agents-mon"\nversion = "0.1.1"\n' > Cargo.toml
    git commit -qam two && git tag v0.1.1
  ) >/dev/null 2>&1
  switch() { PATH="$tmp/bin:$PATH" bash "$tmp/repo/scripts/update.sh" "$1" >/dev/null 2>&1; }
  at() { git -C "$tmp/repo" describe --tags --exact-match 2>/dev/null; }

  switch v0.1.0
  rolled_back="$(at)"
  printf 'scratch\n' > "$tmp/repo/uncommitted"
  switch v0.1.1
  refused="$(at)"
  rm -f "$tmp/repo/uncommitted"
  switch v0.1.1
  if [ "$rolled_back" = "v0.1.0" ] && [ "$refused" = "v0.1.0" ] \
     && [ "$(at)" = "v0.1.1" ]; then
    echo "ok   update-switches-and-guards-dirty-tree"
  else
    echo "FAIL update-switches-and-guards-dirty-tree: back=$rolled_back dirty=$refused now=$(at)"
    fail=1
  fi
  rm -rf "$tmp"
fi
if [ "$fail" -eq 0 ]; then
  tmp="$(mktemp -d)"
  mkdir -p "$tmp/bin" "$tmp/repo/scripts"
  for s in update.sh version.sh install-bin.sh teardown.sh; do
    cp "$DIR/scripts/$s" "$tmp/repo/scripts/"
  done
  cat >"$tmp/repo/agents-mon.tmux" <<'SH'
#!/usr/bin/env bash
printf 'agents-mon.tmux\n' >>"$RESTART_LOG"
SH
  cat >"$tmp/repo/scripts/toggle.sh" <<'SH'
#!/usr/bin/env bash
printf 'toggle.sh\n' >>"$RESTART_LOG"
SH
  printf '#!/usr/bin/env bash\nexit 1\n' >"$tmp/bin/curl"
  printf '#!/usr/bin/env bash\nexit 1\n' >"$tmp/bin/cargo"
  cat >"$tmp/bin/tmux" <<'SH'
#!/usr/bin/env bash
printf 'tmux %s\n' "$*" >>"$RESTART_LOG"
case "$*" in
  info) exit 0 ;;
  "show-option -gqv @agents-mon-on") printf '1\n' ;;
  "show-option -gqv @agents-mon-control-client") printf 'old-control\n' ;;
  "list-clients -F #{client_name}")
    n="$(cat "$CLIENT_POLLS" 2>/dev/null || printf 0)"
    n=$((n + 1))
    printf '%s\n' "$n" >"$CLIENT_POLLS"
    [ "$n" -lt 3 ] && printf 'old-control\n'
    ;;
esac
exit 0
SH
  chmod +x "$tmp/bin/curl" "$tmp/bin/cargo" "$tmp/bin/tmux" \
    "$tmp/repo/agents-mon.tmux" "$tmp/repo/scripts/toggle.sh"
  (
    cd "$tmp/repo" || exit 1
    git init -q . && git config user.email t@t && git config user.name t
    printf '[package]\nname = "agents-mon"\nversion = "0.1.0"\n' >Cargo.toml
    git add -A && git commit -qm one && git tag v0.1.0
    printf '[package]\nname = "agents-mon"\nversion = "0.1.1"\n' >Cargo.toml
    git commit -qam two && git tag v0.1.1
  ) >/dev/null 2>&1
  RESTART_LOG="$tmp/restart.log" CLIENT_POLLS="$tmp/polls" \
    PATH="$tmp/bin:$PATH" bash "$tmp/repo/scripts/update.sh" v0.1.0 \
    >/dev/null 2>&1
  last_poll="$(grep -n '^tmux list-clients -F #{client_name}$' \
    "$tmp/restart.log" | tail -n 1 | cut -d: -f1)"
  entry="$(grep -n '^agents-mon.tmux$' "$tmp/restart.log" |
    head -n 1 | cut -d: -f1)"
  if [ "$(cat "$tmp/polls" 2>/dev/null)" = 3 ] \
     && [ -n "$last_poll" ] && [ -n "$entry" ] \
     && [ "$last_poll" -lt "$entry" ]; then
    echo "ok   update-waits-for-old-daemon-before-restart"
  else
    echo "FAIL update-waits-for-old-daemon-before-restart: polls=$(cat "$tmp/polls" 2>/dev/null) last=$last_poll entry=$entry"
    cat "$tmp/restart.log"
    fail=1
  fi
  rm -rf "$tmp"
fi
if [ "$fail" -eq 0 ]; then
  tmp="$(mktemp -d)"
  mkdir -p "$tmp/bin"
  cat > "$tmp/bin/tmux" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$TMUX_STUB_LOG"
case "$1 $2 $3" in
  "show-option -gqv @agents-mon-key") printf 'E\n' ;;
  "show-option -gqv @agents-mon-popup-key") printf 'e\n' ;;
esac
exit 0
SH
  chmod +x "$tmp/bin/tmux"
  TMUX_STUB_LOG="$tmp/tmux.log" PATH="$tmp/bin:$PATH" bash "$DIR/agents-mon.tmux"
  if grep -q "^bind-key E run-shell -b " "$tmp/tmux.log" \
     && grep -q "^bind-key e run-shell -b " "$tmp/tmux.log"; then
    echo "ok   entrypoint-binds-toggle-in-background"
  else
    echo "FAIL entrypoint-binds-toggle-in-background: popup toggle would block tmux"
    cat "$tmp/tmux.log"
    fail=1
  fi
  rm -rf "$tmp"
fi
if [ "$fail" -eq 0 ]; then
  tmp="$(mktemp -d)"
  mkdir -p "$tmp/bin"
  cat > "$tmp/bin/tmux" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$TMUX_STUB_LOG"
case "$*" in
  "show-option -gqv @agents-mon-sidebar") printf '%%99\n' ;;
  "display-message -p -t %99 #{window_id}") printf '@sb\n' ;;
  "list-panes -t @sb -F x") printf 'x\n' ;;
  "display-message -p -t %99 #{session_id}") printf 's1\n' ;;
  "list-clients "*"-F #{client_name}") printf 'c1\n' ;;
  "display-message -p -c c1 #{window_id}") printf '@other\n' ;;
esac
exit 0
SH
  chmod +x "$tmp/bin/tmux"
  TMUX_STUB_LOG="$tmp/tmux.log" PATH="$tmp/bin:$PATH" bash "$DIR/scripts/orphan.sh"
  if grep -Eq '^(switch-client|last-window|next-window)' "$tmp/tmux.log"; then
    echo "FAIL orphan-does-not-move-unstranded-client: moved focus from another window"
    cat "$tmp/tmux.log"
    fail=1
  else
    echo "ok   orphan-does-not-move-unstranded-client"
  fi
  rm -rf "$tmp"
fi
if [ "$fail" -eq 0 ]; then
  tmp="$(mktemp -d)"
  mkdir -p "$tmp/bin"
  cat > "$tmp/bin/tmux" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$TMUX_STUB_LOG"
case "$*" in
  "show-option -gqv @agents-mon-sidebar") printf '%%99\n' ;;
  "display-message -p -t %99 #{window_id}") printf '@sb\n' ;;
  "list-panes -t @sb -F x") printf 'x\n' ;;
  "display-message -p -t %99 #{session_id}") printf 's1\n' ;;
  "list-clients "*"-F #{client_name}") printf 'c1\n' ;;
  "display-message -p -c c1 #{window_id}") printf '@sb\n' ;;
  "list-windows -t s1 -F #{window_id}\t#{window_last_flag}") printf '@sb\t0\n@last\t1\n' ;;
  "list-windows -t s1 -F #{window_id}") printf '@sb\n@last\n' ;;
esac
exit 0
SH
  chmod +x "$tmp/bin/tmux"
  TMUX_STUB_LOG="$tmp/tmux.log" PATH="$tmp/bin:$PATH" bash "$DIR/scripts/orphan.sh"
  if grep -q '^switch-client -c c1 -t @last$' "$tmp/tmux.log" \
     && ! grep -Eq '^(last-window|next-window|switch-client -l|switch-client -p)' "$tmp/tmux.log"; then
    echo "ok   orphan-moves-only-stranded-client"
  else
    echo "FAIL orphan-moves-only-stranded-client: did not target stranded client safely"
    cat "$tmp/tmux.log"
    fail=1
  fi
  rm -rf "$tmp"
fi
if [ "$fail" -eq 0 ]; then
  tmp="$(mktemp -d)"
  mkdir -p "$tmp/bin"
  cat > "$tmp/bin/tmux" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$TMUX_STUB_LOG"
case "$1" in
  show-option) exit 0 ;;
  display-popup) exit 0 ;;
  *) exit 0 ;;
esac
SH
  chmod +x "$tmp/bin/tmux"
  TMUX_STUB_LOG="$tmp/tmux.log" TMPDIR="$tmp" PATH="$tmp/bin:$PATH" \
    bash "$DIR/scripts/toggle.sh" popup popup-client &
  pid=$!
  waited=0
  while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt 20 ]; do
    sleep 0.05
    waited=$((waited + 1))
  done
  owner_ok=0
  if grep -Fq -- '-c popup-client' "$tmp/tmux.log" \
     && grep -Fq -- '-e AGENTS_MON_POPUP_CLIENT=popup-client' "$tmp/tmux.log"; then
    owner_ok=1
  fi
  if kill -0 "$pid" 2>/dev/null; then
    echo "FAIL popup-exits-when-helper-exits: toggle loop kept stale pin"
    kill "$pid" 2>/dev/null
    wait "$pid" 2>/dev/null
    fail=1
  elif [ "$owner_ok" -ne 1 ]; then
    echo "FAIL popup-exits-when-helper-exits: popup owner was not propagated"
    cat "$tmp/tmux.log"
    wait "$pid"
    fail=1
  else
    wait "$pid"
    echo "ok   popup-exits-when-helper-exits"
  fi
  rm -rf "$tmp"
fi
if [ "$fail" -eq 0 ]; then
  tmp="$(mktemp -d)"
  mkdir -p "$tmp/bin"
  cat > "$tmp/bin/tmux" <<'SH'
#!/usr/bin/env bash
if [ "$1" = "kill-pane" ]; then
  printf '%s\n' "$*" >> "$TMUX_STUB_LOG"
fi
exit 0
SH
  chmod +x "$tmp/bin/tmux"
  touch "$tmp/pin"
  TMUX_STUB_LOG="$tmp/tmux.log" TMPDIR="$tmp" PATH="$tmp/bin:$PATH" \
    AGENTS_MON_PIN="$tmp/pin" TMUX_PANE="%%99" bash "$DIR/scripts/sidebar.sh" >/dev/null 2>&1 &
  pid=$!
  sleep 0.1
  kill -TERM "$pid" 2>/dev/null || true
  waited=0
  while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt 20 ]; do
    sleep 0.05
    waited=$((waited + 1))
  done
  if [ -e "$tmp/pin" ]; then
    echo "FAIL popup-sidebar-signal-removes-pin: stale popup pin remained"
    fail=1
  elif grep -q 'kill-pane' "$tmp/tmux.log" 2>/dev/null; then
    echo "FAIL popup-sidebar-signal-removes-pin: popup cleanup created a real pane"
    fail=1
  else
    echo "ok   popup-sidebar-signal-removes-pin"
  fi
  kill "$pid" 2>/dev/null
  wait "$pid" 2>/dev/null
  rm -rf "$tmp"
fi
if [ "$fail" -eq 0 ]; then
  tmp="$(mktemp -d)"
  mkdir -p "$tmp/bin"
  cat > "$tmp/bin/tmux" <<'SH'
#!/usr/bin/env bash
exit 0
SH
  chmod +x "$tmp/bin/tmux"
  touch "$tmp/pin"
  printf '\004' | TMPDIR="$tmp" PATH="$tmp/bin:$PATH" \
    AGENTS_MON_PIN="$tmp/pin" TMUX_PANE="%%99" bash "$DIR/scripts/sidebar.sh" >/dev/null 2>&1
  if [ -e "$tmp/pin" ]; then
    echo "FAIL popup-sidebar-ctrl-d-removes-pin: stale popup pin remained"
    fail=1
  else
    echo "ok   popup-sidebar-ctrl-d-removes-pin"
  fi
  rm -rf "$tmp"
fi
if [ "$fail" -eq 0 ] && command -v tmux >/dev/null; then
  # real server on a private scratch socket: sidebar must follow into a NEW
  # window (new-window fires session-window-changed, not after-select-window)
  tmp="$(mktemp -d)"
  T="tmux -S $tmp/sock -f /dev/null"
  $T new-session -d -s t -x 200 -y 50
  sb="$($T split-window -hbf -d -l 30 -P -F '#{pane_id}' -t t: 'exec sleep 60')"
  $T set-option -g @agents-mon-sidebar "$sb"
  $T set-option -g @agents-mon-sidebar-win "$($T display-message -p -t t: '#{window_id}')"
  # hooks.sh calls bare tmux — point it at the scratch socket (absolute
  # path: bare "tmux" would resolve back to this shim and recurse)
  mkdir -p "$tmp/bin"
  printf '#!/bin/sh\nexec %s -S %s "$@"\n' "$(command -v tmux)" "$tmp/sock" \
    > "$tmp/bin/tmux"
  chmod +x "$tmp/bin/tmux"
  PATH="$tmp/bin:$PATH" bash "$DIR/scripts/hooks.sh"
  $T new-window -t t
  sleep 0.5
  sb_win="$($T display-message -p -t "$sb" '#{window_id}')"
  cur_win="$($T display-message -p -t t: '#{window_id}')"
  if [ -n "$sb_win" ] && [ "$sb_win" = "$cur_win" ]; then
    echo "ok   sidebar-follows-into-new-window"
  else
    echo "FAIL sidebar-follows-into-new-window: sidebar in '$sb_win', current window '$cur_win'"
    fail=1
  fi
  $T kill-server 2>/dev/null || true
  rm -rf "$tmp"
fi
if [ "$fail" -eq 0 ] && command -v tmux >/dev/null && [ -x "$BIN" ]; then
  # mirror mode end to end: toggle puts a mirror pane in every window, window
  # switches change NO layout (the whole point — no reflow bump), new windows
  # get a mirror via hook, and q tears everything down.
  # NOTE: must pin @agents-mon-bin to $BIN — on CI the build lives at the
  # musl target path, and target/release/ holds the DOWNLOADED old release
  # (auto-install test side effect) which lacks the mirror/daemon commands.
  BIN_ABS="$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")"
  tmp="$(mktemp -d)"
  T="tmux -S $tmp/sock -f /dev/null"
  mkdir -p "$tmp/bin"
  printf '#!/bin/sh\nexec %s -S %s "$@"\n' "$(command -v tmux)" "$tmp/sock" \
    > "$tmp/bin/tmux"
  chmod +x "$tmp/bin/tmux"
  TMPDIR="$tmp" $T new-session -d -s t -x 200 -y 50 'exec sleep 60'
  $T new-window -t t: 'exec sleep 60'
  $T set-option -g @agents-mon-bin "$BIN_ABS"
  env TMPDIR="$tmp" TMUX="$tmp/sock,0,0" PATH="$tmp/bin:$PATH" \
    bash "$DIR/scripts/toggle.sh"
  sleep 2
  mirrors=0
  processless=0
  for w in $($T list-windows -t t -F '#{window_id}'); do
    pane_info="$($T list-panes -t "$w" -F '#{pane_title} #{pane_pid}' |
      awk '$1 == "agents-mon" { print; exit }')"
    if [ -n "$pane_info" ]; then
      mirrors=$((mirrors + 1))
      [ "${pane_info##* }" = 0 ] && processless=$((processless + 1))
    fi
  done
  focus_kept=0
  [ "$($T display-message -p -t t: '#{pane_title}')" != agents-mon ] && focus_kept=1
  keys_ok=0
  $T list-keys -T agents-mon | grep -Fq "key 'close'" && keys_ok=1
  control="$($T show-option -gqv @agents-mon-control-client)"
  control_ok=0
  [ -n "$control" ] &&
    $T list-clients -F '#{client_name}' | grep -Fxq "$control" &&
    control_ok=1
  before="$($T list-windows -t t -F '#{window_id} #{window_layout}')"
  $T last-window -t t; $T last-window -t t
  sleep 0.5
  after="$($T list-windows -t t -F '#{window_id} #{window_layout}')"
  live_sidebar="$($T list-panes -t t: -F '#{pane_id}	#{pane_title}' |
    awk -F'\t' '$2 == "agents-mon" { print $1; exit }')"
  live_frame="$($T capture-pane -p -t "$live_sidebar")"
  $T new-window -t t: 'exec sleep 60'
  sleep 1.5
  neww="$($T display-message -p -t t: '#{window_id}')"
  new_ok=0
  $T list-panes -t "$neww" -F '#{pane_title}' | grep -qx agents-mon && new_ok=1
  # concurrent adds must not double-split. One window switch fires two [43]
  # hooks, so racing mirror-add.sh calls are routine, and the old
  # check-then-split let every one of them through.
  racew="$($T new-window -d -a -t t: -P -F '#{window_id}' 'exec sleep 60')"
  $T list-panes -t "$racew" -F '#{pane_id}	#{pane_title}' |
    awk -F'\t' '$2 == "agents-mon" { print $1 }' |
    while read -r p; do $T kill-pane -t "$p"; done
  for _ in 1 2 3 4 5 6 7 8; do
    env TMPDIR="$tmp" TMUX="$tmp/sock,0,0" PATH="$tmp/bin:$PATH" \
      bash "$DIR/scripts/mirror-add.sh" "$racew" &
  done
  wait
  raced="$($T list-panes -t "$racew" -F '#{pane_title}' | grep -cx agents-mon)"
  $T kill-window -t "$racew"

  mir="$($T list-panes -t t: -F '#{pane_id}	#{pane_title}' |
    awk -F'\t' '$2 == "agents-mon" { print $1; exit }')"
  # dragging one mirror's border adopts the width everywhere (sync-width.sh
  # via window-layout-changed hook)
  $T resize-pane -t "$mir" -x 45
  # the drag guard needs two same-window-size measures (2s scan apart) when
  # the resized mirror lives in a window created moments ago
  sleep 4
  widths="$($T list-panes -a -F '#{pane_title}	#{pane_width}' |
    awk -F'\t' '$1 == "agents-mon" { print $2 }' | sort -u | tr -d '\n')"
  optw="$($T show-option -gqv @agents-mon-width)"
  # closing the last real pane hands all its columns to the mirror without
  # changing the window size — that must NOT read as a border drag (the pane
  # count changed), or the full window width gets adopted globally
  $T new-window -t t: 'exec sleep 60'
  sleep 4  # mirror via hook + a daemon baseline measure with both panes open
  closew="$($T display-message -p -t t: '#{window_id}')"
  agentp="$($T list-panes -t "$closew" -F '#{pane_id}	#{pane_title}' |
    awk -F'\t' '$2 != "agents-mon" { print $1; exit }')"
  $T kill-pane -t "$agentp"
  sleep 4
  optw2="$($T show-option -gqv @agents-mon-width)"
  env TMPDIR="$tmp" TMUX="$tmp/sock,0,0" "$BIN_ABS" key q
  sleep 0.2
  stayed="$($T list-panes -a -F '#{pane_title}' 2>/dev/null | grep -cx agents-mon)"
  env TMPDIR="$tmp" TMUX="$tmp/sock,0,0" "$BIN_ABS" key close
  sleep 2
  left="$($T list-panes -a -F '#{pane_title}' 2>/dev/null | grep -cx agents-mon)"
  if [ "$mirrors" -eq 2 ] && [ "$processless" -eq 2 ] && [ "$focus_kept" -eq 1 ] \
     && [ "$keys_ok" -eq 1 ] && [ "$control_ok" -eq 1 ] && [ "$stayed" -gt 0 ] \
     && printf '%s\n' "$live_frame" | grep -Fq agents \
     && [ "$before" = "$after" ] && [ "$new_ok" -eq 1 ] \
     && [ "$raced" -eq 1 ] && [ "$widths" = 45 ] && [ "$optw" = 45 ] \
     && [ "$optw2" = 45 ] \
     && [ "$left" -eq 0 ] && [ ! -f "$tmp/agents-mon-frame" ]; then
    echo "ok   mirror-mode-no-bump-lifecycle"
  else
    echo "FAIL mirror-mode-no-bump-lifecycle: mirrors=$mirrors processless=$processless focus=$focus_kept keys=$keys_ok control=$control_ok stayed=$stayed live=$([ -n "$live_frame" ] && echo y || echo n) layout-same=$([ "$before" = "$after" ] && echo y || echo n) new=$new_ok raced=$raced widths=$widths optw=$optw optw2=$optw2 left=$left"
    fail=1
  fi
  $T kill-server 2>/dev/null || true
  rm -rf "$tmp"
fi
if [ "$fail" -eq 0 ] && command -v tmux >/dev/null && [ -x "$BIN" ]; then
  # Full-screen overlays block the normal scan loop while waiting for a key.
  # They must still render through the visible pane writer and accept keys
  # through the daemon FIFO.
  BIN_ABS="$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")"
  tmp="$(mktemp -d)"
  T="tmux -S $tmp/sock -f /dev/null"
  mkdir -p "$tmp/bin"
  printf '#!/bin/sh\nexec %s -S %s "$@"\n' "$(command -v tmux)" "$tmp/sock" \
    > "$tmp/bin/tmux"
  chmod +x "$tmp/bin/tmux"
  printf '#!/bin/sh\nwhile :; do sleep 10; done\n' >"$tmp/codex"
  chmod +x "$tmp/codex"
  TMPDIR="$tmp" $T new-session -d -s t -x 80 -y 15 "$tmp/codex"
  $T new-window -t t: "$tmp/codex"
  $T set-option -g @agents-mon-bin "$BIN_ABS"
  env TMPDIR="$tmp" TMUX="$tmp/sock,0,0" PATH="$tmp/bin:$PATH" \
    bash "$DIR/scripts/toggle.sh"
  sleep 2
  mirrors() { $T list-panes -a -F '#{pane_title}' 2>/dev/null | grep -cx agents-mon; }
  mir="$($T list-panes -t t: -F '#{pane_id}	#{pane_title}' |
    awk -F'\t' '$2 == "agents-mon" { print $1; exit }')"
  opened="$(mirrors)"
  env TMPDIR="$tmp" TMUX="$tmp/sock,0,0" "$BIN_ABS" key help
  sleep 1
  help_alive="$(mirrors)"
  help_frame="$($T capture-pane -p -t "$mir")"
  env TMPDIR="$tmp" TMUX="$tmp/sock,0,0" "$BIN_ABS" key space
  sleep 1
  list_before="$($T capture-pane -p -t "$mir")"
  env TMPDIR="$tmp" TMUX="$tmp/sock,0,0" "$BIN_ABS" key versions
  sleep 3
  vers_alive="$(mirrors)"
  vers_frame="$($T capture-pane -p -t "$mir")"
  env TMPDIR="$tmp" TMUX="$tmp/sock,0,0" "$BIN_ABS" key close
  sleep 1
  list_after="$($T capture-pane -p -t "$mir")"
  env TMPDIR="$tmp" TMUX="$tmp/sock,0,0" "$BIN_ABS" key j
  sleep 1
  list_moved="$($T capture-pane -p -t "$mir")"
  help_fits=0
  versions_fit=0
  printf '%s\n' "$help_frame" | head -n 1 | grep -Fq 'agents — help' \
    && printf '%s\n' "$help_frame" | grep -Fq 'any key returns' \
    && help_fits=1
  printf '%s\n' "$vers_frame" | head -n 1 | grep -Fq 'agents — versions' \
    && printf '%s\n' "$vers_frame" | grep -Fq 'q back' \
    && versions_fit=1
  if [ "$opened" -eq 2 ] && [ "$help_alive" -eq 2 ] && [ "$vers_alive" -eq 2 ] \
     && [ "$help_fits" -eq 1 ] && [ "$versions_fit" -eq 1 ] \
     && printf '%s\n' "$list_before" | grep -Fq codex \
     && [ "$list_after" = "$list_before" ] \
     && ! printf '%s\n' "$list_moved" | grep -Fq '❯'; then
    echo "ok   overlays-render-in-processless-panes"
  else
    echo "FAIL overlays-render-in-processless-panes: opened=$opened help=$help_alive versions=$vers_alive help_fits=$help_fits versions_fit=$versions_fit list=[$list_before/$list_after/$list_moved]"
    fail=1
  fi
  $T kill-server 2>/dev/null || true
  rm -rf "$tmp"
fi
if [ "$fail" -eq 0 ] && [ "$(uname -s)" = Darwin ] \
   && [ -x "$DIR/target/release/agents-mon-notifier" ]; then
  # install-app.sh must assemble a signed AgentsMon.app around the notifier
  tmp="$(mktemp -d)"
  plist="$tmp/apps/AgentsMon.app/Contents/Info.plist"
  if AGENTS_MON_NOTIFIER_BIN="$DIR/target/release/agents-mon-notifier" \
       bash "$DIR/scripts/install-app.sh" "$tmp/apps" >/dev/null 2>&1 \
     && [ -x "$tmp/apps/AgentsMon.app/Contents/MacOS/agents-mon-notifier" ] \
     && grep -q 'io.github.snirt.agents-mon' "$plist" \
     && grep -q 'LSUIElement' "$plist" \
     && codesign --verify "$tmp/apps/AgentsMon.app" 2>/dev/null; then
    echo "ok   install-app-assembles-signed-bundle"
  else
    echo "FAIL install-app-assembles-signed-bundle"
    fail=1
  fi
  rm -rf "$tmp"
fi
if [ "$fail" -eq 0 ]; then
  AGENTS_MON_BIN="$BIN" bash "$DIR/tests/navigation.sh" || fail=1
fi
if [ "$fail" -eq 0 ]; then
  AGENTS_MON_BIN="$BIN" bash "$DIR/tests/daemon-orphan.sh" || fail=1
fi
exit $fail

#!/usr/bin/env bash
# agenmux TPM entry point. Keep this pre-binary bootstrap small:
# tmux configuration owns launchers; the engine owns app integration.
CURRENT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

option() {
  local canonical="$1" legacy
  legacy="${canonical/@agenmux-/@agents-mon-}"
  if [ -n "$(tmux show-options -gq "$canonical")" ]; then
    tmux show-option -gqv "$canonical"
  elif [ -n "$(tmux show-options -gq "$legacy")" ]; then
    tmux show-option -gqv "$legacy"
  else
    printf '%s' "${2:-}"
  fi
}
DEFAULT_BIN="$CURRENT_DIR/target/release/agenmux"
DEBUG_BIN="$CURRENT_DIR/target/debug/agenmux"
BIN="$(option @agenmux-bin)"
[ -n "$BIN" ] || BIN="$DEFAULT_BIN"

# Set when activation recovers to the local debug build because @agenmux-bin
# pointed at a removed worktree. That binary is this checkout's own, and a
# developer build is not required to match the released tag.
RECOVERED=""

engine_current() {
  [ -x "$BIN" ] || return 1
  [ -n "$RECOVERED" ] && return 0
  want="$(bash "$CURRENT_DIR/scripts/version.sh" tag 2>/dev/null)" || return 1
  # A configured binary is verified too: a stale one must not activate silently.
  if [ "$BIN" != "$DEFAULT_BIN" ]; then
    [ "$("$BIN" --version 2>/dev/null)" = "agenmux ${want#v}" ]
    return $?
  fi
  state="$CURRENT_DIR/target/release/.agenmux-version"
  installed_tag="$(sed -n '1p' "$state" 2>/dev/null)"
  installed_rev="$(sed -n '2p' "$state" 2>/dev/null)"
  current_rev="$(git -C "$CURRENT_DIR" rev-parse HEAD 2>/dev/null || printf '-')"
  [ "$installed_tag" = "$want" ] && [ "$installed_rev" = "$current_rev" ] \
    && [ "$("$BIN" --version 2>/dev/null)" = "agenmux ${want#v}" ]
}

# Internal activation entrypoint used by the tmux bindings below. First use can
# beat the eager installer, so serialize with it before handing runtime control
# to Rust. This is bootstrap, not a second sidebar/toggle implementation.
if [ "${1:-}" = activate ]; then
  if [ "$BIN" != "$DEFAULT_BIN" ] && [ ! -x "$BIN" ]; then
    tmux set-option -gu @agenmux-bin 2>/dev/null || true
    tmux set-option -gu @agents-mon-bin 2>/dev/null || true
    if [ -x "$DEBUG_BIN" ]; then
      BIN="$DEBUG_BIN"
      RECOVERED=1
      tmux set-option -g @agenmux-bin "$BIN"
      AGENMUX_DIR="$CURRENT_DIR" "$BIN" setup || exit 1
    else
      BIN="$DEFAULT_BIN"
    fi
  fi
  mode="${2:-}"
  client="${3:-}"
  if ! engine_current; then
    locked=""
    unlock() {
      [ -n "$locked" ] || return
      locked=""
      tmux wait-for -U agenmux-install 2>/dev/null || true
    }
    trap unlock EXIT HUP INT TERM
    if tmux wait-for -L agenmux-install; then
      locked=1
      if ! engine_current && [ "$BIN" = "$DEFAULT_BIN" ]; then
        bash "$CURRENT_DIR/scripts/install-bin.sh" >/dev/null 2>&1 || true
      fi
      unlock
    fi
    if ! engine_current; then
      tmux display-message 'agenmux: native engine installation failed' 2>/dev/null || true
      exit 1
    fi
    # Let the freshly installed version set up app integration before retrying the
    # action that triggered installation.
    AGENMUX_INSTALL_REFRESH=1 bash "$CURRENT_DIR/agenmux.tmux" || exit $?
  fi
  exec env AGENMUX_DIR="$CURRENT_DIR" "$BIN" toggle "$mode" "$client"
fi

# Launchers are tmux preferences, independent of application TOML/validation.
# Native last-writer-wins: empty disables installation, never unbinds old keys.
# Expand trusted path/client identities with tmux's shell quoting at execution
# time, not by interpolating them into shell or tmux command-language text.
tmux set-option -g @agenmux-plugin-dir "$CURRENT_DIR" || exit $?
install_launcher() {
  local key="$1" action="$2"
  [ -n "$key" ] || return 0
  [ "$key" != ';' ] || key='\;'
  tmux bind-key -T prefix "$key" run-shell -b "$action" || {
    tmux display-message 'agenmux: launcher binding failed; check @agenmux-key and @agenmux-popup-key' 2>/dev/null || true
    return 1
  }
}
install_launcher "$(option @agenmux-key A)" \
  "bash #{q:@agenmux-plugin-dir}/agenmux.tmux activate '' #{q:client_name}" || exit $?
install_launcher "$(option @agenmux-popup-key e)" \
  "bash #{q:@agenmux-plugin-dir}/agenmux.tmux activate 'popup' #{q:client_name}" || exit $?

# Only a verified engine performs application setup and validation.
if engine_current; then
  AGENMUX_DIR="$CURRENT_DIR" "$BIN" setup || {
    rc=$?
    tmux display-message 'agenmux: setup failed; run agenmux config check --effective and agenmux setup for diagnostics' 2>/dev/null || true
    exit "$rc"
  }
else
  tmux display-message 'agenmux: native engine installation pending; source plugin again after installation if setup fails' 2>/dev/null || true
fi

# The source checkout has no binary, so eagerly install the default in the
# background. The activation entrypoint takes the same lock when first use
# beats it.
if [ "$BIN" = "$DEFAULT_BIN" ] \
   && [ "${AGENMUX_INSTALL_REFRESH:-}" != 1 ]; then
  (
    locked=""
    unlock() {
      [ -n "$locked" ] || return
      locked=""
      tmux wait-for -U agenmux-install 2>/dev/null || true
    }
    trap unlock EXIT HUP INT TERM
    tmux wait-for -L agenmux-install || exit 0
    locked=1
    bash "$CURRENT_DIR/scripts/install-bin.sh" >/dev/null 2>&1 || true
    # Re-enter even when an older binary already existed: source and engine
    # upgrades must install this version's setup contract together.
    if engine_current; then
      AGENMUX_INSTALL_REFRESH=1 bash "$CURRENT_DIR/agenmux.tmux"
    fi
  ) &
fi

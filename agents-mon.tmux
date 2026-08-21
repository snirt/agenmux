#!/usr/bin/env bash
# tmux-agents-mon TPM entry point. Keep this pre-binary bootstrap small: once
# the native engine exists, `agents-mon setup` owns all tmux integration.
CURRENT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEFAULT_BIN="$CURRENT_DIR/target/release/agents-mon"
BIN="$(tmux show-option -gqv @agents-mon-bin)"
[ -n "$BIN" ] || BIN="$DEFAULT_BIN"

engine_current() {
  [ -x "$BIN" ] || return 1
  [ "$BIN" != "$DEFAULT_BIN" ] && return 0
  want="$(bash "$CURRENT_DIR/scripts/version.sh" tag 2>/dev/null)" || return 1
  state="$CURRENT_DIR/target/release/.agents-mon-version"
  installed_tag="$(sed -n '1p' "$state" 2>/dev/null)"
  installed_rev="$(sed -n '2p' "$state" 2>/dev/null)"
  current_rev="$(git -C "$CURRENT_DIR" rev-parse HEAD 2>/dev/null || printf '-')"
  [ "$installed_tag" = "$want" ] && [ "$installed_rev" = "$current_rev" ] \
    && [ "$("$BIN" --version 2>/dev/null)" = "agents-mon ${want#v}" ]
}

# Internal activation entrypoint used by the tmux bindings below. First use can
# beat the eager installer, so serialize with it before handing runtime control
# to Rust. This is bootstrap, not a second sidebar/toggle implementation.
if [ "${1:-}" = activate ]; then
  mode="${2:-}"
  client="${3:-}"
  if ! engine_current; then
    locked=""
    unlock() {
      [ -n "$locked" ] || return
      locked=""
      tmux wait-for -U agents-mon-install 2>/dev/null || true
    }
    trap unlock EXIT HUP INT TERM
    if tmux wait-for -L agents-mon-install; then
      locked=1
      if ! engine_current && [ "$BIN" = "$DEFAULT_BIN" ]; then
        bash "$CURRENT_DIR/scripts/install-bin.sh" >/dev/null 2>&1 || true
      fi
      unlock
    fi
    if ! engine_current; then
      tmux display-message 'agents-mon: native engine installation failed' 2>/dev/null || true
      exit 1
    fi
    # Let the freshly installed version own bindings/hooks before retrying the
    # action that triggered installation.
    AGENTS_MON_INSTALL_REFRESH=1 bash "$CURRENT_DIR/agents-mon.tmux"
  fi
  exec env AGENTS_MON_DIR="$CURRENT_DIR" "$BIN" toggle "$mode" "$client"
fi

key="$(tmux show-option -gqv @agents-mon-key)"
tmux bind-key "${key:-A}" run-shell -b \
  "bash '$CURRENT_DIR/agents-mon.tmux' activate '' '#{client_name}'"

# optional dedicated popup key, e.g. set -g @agents-mon-popup-key 'e'
popup_key="$(tmux show-option -gqv @agents-mon-popup-key)"
[ -n "$popup_key" ] && tmux bind-key "$popup_key" run-shell -b \
  "bash '$CURRENT_DIR/agents-mon.tmux' activate popup '#{client_name}'"

# Live servers may retain deleted moving-sidebar hooks across an upgrade. This
# cleanup must work before Rust is installed.
tmux set-hook -gu 'after-select-window[42]' 2>/dev/null || true
tmux set-hook -gu 'client-session-changed[42]' 2>/dev/null || true
tmux set-hook -gu 'session-window-changed[42]' 2>/dev/null || true

# A source update can briefly leave the previous release's binary here; it may
# not know `setup` yet. The installer refresh below re-enters with the matching
# binary, so keep this compatibility probe quiet.
if engine_current; then
  AGENTS_MON_DIR="$CURRENT_DIR" "$BIN" setup >/dev/null 2>&1 || true
fi

# The source checkout has no binary, so eagerly install the default in the
# background. The activation entrypoint takes the same lock when first use
# beats it.
if [ "$BIN" = "$DEFAULT_BIN" ] \
   && [ "${AGENTS_MON_INSTALL_REFRESH:-}" != 1 ]; then
  (
    locked=""
    unlock() {
      [ -n "$locked" ] || return
      locked=""
      tmux wait-for -U agents-mon-install 2>/dev/null || true
    }
    trap unlock EXIT HUP INT TERM
    tmux wait-for -L agents-mon-install || exit 0
    locked=1
    bash "$CURRENT_DIR/scripts/install-bin.sh" >/dev/null 2>&1 || true
    # Re-enter even when an older binary already existed: source and engine
    # upgrades must install this version's setup contract together.
    if engine_current; then
      AGENTS_MON_INSTALL_REFRESH=1 bash "$CURRENT_DIR/agents-mon.tmux"
    fi
  ) &
fi

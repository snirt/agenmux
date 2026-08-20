#!/usr/bin/env bash
# tmux-agents-mon TPM entry point. Keep this pre-binary bootstrap small: once
# the native engine exists, `agents-mon setup` owns all tmux integration.
CURRENT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

key="$(tmux show-option -gqv @agents-mon-key)"
tmux bind-key "${key:-A}" run-shell -b \
  "bash '$CURRENT_DIR/scripts/toggle.sh' '' '#{client_name}'"

# optional dedicated popup key, e.g. set -g @agents-mon-popup-key 'e'
popup_key="$(tmux show-option -gqv @agents-mon-popup-key)"
[ -n "$popup_key" ] && tmux bind-key "$popup_key" run-shell -b \
  "bash '$CURRENT_DIR/scripts/toggle.sh' popup '#{client_name}'"

# Live servers may retain deleted moving-sidebar hooks across an upgrade. This
# cleanup must work before Rust is installed.
tmux set-hook -gu 'after-select-window[42]' 2>/dev/null || true
tmux set-hook -gu 'client-session-changed[42]' 2>/dev/null || true
tmux set-hook -gu 'session-window-changed[42]' 2>/dev/null || true

BIN="$(tmux show-option -gqv @agents-mon-bin)"
[ -n "$BIN" ] || BIN="$CURRENT_DIR/target/release/agents-mon"
# A source update can briefly leave the previous release's binary here; it may
# not know `setup` yet. The installer refresh below re-enters with the matching
# binary, so keep this compatibility probe quiet.
[ ! -x "$BIN" ] || AGENTS_MON_DIR="$CURRENT_DIR" "$BIN" setup >/dev/null 2>&1 || true

# The source checkout has no binary, so eagerly install the default in the
# background. toggle.sh takes the same tmux lock when first use beats it.
if [ "$BIN" = "$CURRENT_DIR/target/release/agents-mon" ] \
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
    if [ -x "$BIN" ]; then
      AGENTS_MON_INSTALL_REFRESH=1 bash "$CURRENT_DIR/agents-mon.tmux"
    fi
  ) &
fi

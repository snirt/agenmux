#!/usr/bin/env bash
# Install all plugin hooks — single source of truth, called from toggle.sh
# and agents-mon.tmux (config reloads clear hooks).
DIR="$(cd "$(dirname "$0")/.." && pwd)"

ver="$(tmux -V | sed 's/[^0-9.]//g')"
if awk -v v="$ver" 'BEGIN { exit !(v + 0 >= 3.2) }'; then
  # Native follow: the join executes inside the server during the switch, so
  # the new window first renders already WITH the sidebar — no flash/bump.
  # Server-side serialization also kills the old two-hooks race, no lock.
  # Guard: sidebar open, not already in this window, and never follow into pi.
  guard='#{&&:#{&&:#{!=:#{@agents-mon-sidebar},},#{!=:#{@agents-mon-sidebar-win},#{window_id}}},#{!=:#{session_name},pi}}'
  body="run -C 'set -g @agents-mon-prev-win #{@agents-mon-sidebar-win}'"
  body="$body ; run -C 'set -g @agents-mon-layout-#{window_id} \"#{window_layout}\"'"
  body="$body ; run -C 'join-pane -hbf -d -l #{?#{@agents-mon-width},#{@agents-mon-width},30} -s #{@agents-mon-sidebar} -t #{pane_id}'"
  body="$body ; run -C 'set -g @agents-mon-sidebar-win #{window_id}'"
  body="$body ; run-shell -b 'bash $DIR/scripts/restore.sh'"
  follow="if -F \"$guard\" { $body }"
else
  follow="run-shell 'bash $DIR/scripts/follow.sh'"
fi
tmux set-hook -g 'after-select-window[42]' "$follow"
tmux set-hook -g 'client-session-changed[42]' "$follow"
# new-window doesn't fire after-select-window; session-window-changed covers
# it (and skips new-window -d, which shouldn't steal the sidebar)
tmux set-hook -g 'session-window-changed[42]' "$follow"
# pane-exited misses kill-pane, and window-pane-changed fires mid-teardown
# (the dying pane still resolves, so orphan.sh takes the wrong branch);
# window-layout-changed fires after removal and is the reliable one. All
# three stay — orphan.sh is a cheap guard-and-exit when nothing died.
tmux set-hook -g 'pane-exited[42]' "run-shell 'bash $DIR/scripts/orphan.sh'"
tmux set-hook -g 'window-pane-changed[42]' "run-shell 'bash $DIR/scripts/orphan.sh'"
tmux set-hook -g 'window-layout-changed[42]' "run-shell 'bash $DIR/scripts/orphan.sh'"
# client resizes rescale panes proportionally — snap the sidebar back
tmux set-hook -g 'window-resized[42]' "run-shell 'bash $DIR/scripts/pin.sh'"

# Full-screen pane modes such as the default `prefix + w` chooser use `-Z` to
# zoom their target. When the selected target is the narrow sidebar that makes
# the plugin suddenly fill the window. pane-mode-changed fires just before the
# zoom is applied, so defer the format check to a background command; it keeps
# the mode open but restores the normal layout immediately. The user's original
# picker binding stays intact.
tmux set-hook -g 'pane-mode-changed[44]' \
  "run-shell -b 'tmux if-shell -t \"#{pane_id}\" -F \"#{&&:#{==:#{pane_title},agents-mon},#{window_zoomed_flag}}\" \"resize-pane -Z -t \\\"#{pane_id}\\\"\"'"

# Preserved-pane mode (Rust engine): windows created or first visited while on
# get their empty sidebar pane here. Guarded on @agents-mon-on, so these no-op in the
# bash-fallback mode (and the [42] follow hooks no-op in mirror mode — their
# guard is @agents-mon-sidebar, which mirror mode never sets).
# the explicit #{window_id} matters: on detached sessions (and older tmux)
# a bare display-message inside the hook script resolves ambiguously
mirror_add="if -F '#{!=:#{@agents-mon-on},}' { run-shell -b 'bash $DIR/scripts/mirror-add.sh #{window_id}' }"
tmux set-hook -g 'after-select-window[43]' "$mirror_add"
tmux set-hook -g 'session-window-changed[43]' "$mirror_add"
tmux set-hook -g 'client-session-changed[43]' "$mirror_add"
# border drags are detected and propagated by the daemon itself (single
# process = no racing resize storms); no width hook needed here.
# Servers that ran the short-lived sync-width.sh hook keep it until
# something unsets it — it now points at a deleted file and spams 127s
tmux set-hook -gu 'window-layout-changed[43]' 2>/dev/null

# A processless pane cannot read stdin, but a client key table is handled before
# pane input. Keep the real tmux selection border on the visual sidebar while
# routing its keys to the daemon.
sidebar_select="if -F '#{==:#{pane_title},agents-mon}' { switch-client -T agents-mon }"
tmux set-hook -g 'after-select-pane[44]' "$sidebar_select"

# Empty panes have no stdin. A dedicated client key table forwards the same
# logical keys to the daemon's non-blocking FIFO while the work pane keeps focus.
BIN="$(tmux show-option -gqv @agents-mon-bin)"
[ -n "$BIN" ] || BIN="$DIR/target/release/agents-mon"

# Start with the user's normal no-prefix bindings so selecting the sidebar
# behaves like selecting any regular pane (C-h/j/k/l navigation, custom
# bindings, etc.). list-keys emits sourceable tmux commands; plugin-specific
# bindings below then override only the keys the sidebar owns.
tmux unbind-key -a -T agents-mon 2>/dev/null
tmux list-keys -T root |
  sed 's/-T root /-T agents-mon /' |
  tmux source-file -

bind_nav() {
  key="$1" action="$2" next="$3"
  tmux bind-key -T agents-mon "$key" \
    "run-shell -b \"'$BIN' key '$action'\"; switch-client -T '$next'"
}
bind_nav j j agents-mon
bind_nav k k agents-mon
bind_nav Down down agents-mon
bind_nav Up up agents-mon
bind_nav '?' help agents-mon
bind_nav u versions agents-mon
# `/` enters text mode; `f` selects the next exact status; Esc clears filters.
# Search entry, status changes, and text delivery are synchronous: background
# run-shell jobs can reorder fast keystrokes and skip statuses/scramble query.
tmux bind-key -T agents-mon / \
  "run-shell \"'$BIN' key 'search'\"; switch-client -T agents-mon-search"
tmux bind-key -T agents-mon f \
  "run-shell \"'$BIN' key 'filter'\"; switch-client -T agents-mon"
bind_nav Space space agents-mon
bind_nav Any space agents-mon
bind_nav Enter enter root
bind_nav l l root
bind_nav q close root
tmux bind-key -T agents-mon Escape \
  "run-shell \"'$BIN' key 'all'\"; switch-client -T agents-mon"
bind_nav Q close root

# Search table sends printable ASCII as framed text packets. Framing makes
# normal-mode action keys (`j`, `q`, `f`, ...) literal query text while typing.
tmux unbind-key -a -T agents-mon-search 2>/dev/null
tmux list-keys -T root |
  sed 's/-T root /-T agents-mon-search /' |
  tmux source-file -
code=32
while [ "$code" -le 126 ]; do
  char="$(printf "\\$(printf '%03o' "$code")")"
  key="$char"
  [ "$char" = ' ' ] && key=Space
  [ "$char" = ';' ] && key='\;'
  hex="$(printf '%02X' "$code")"
  tmux bind-key -T agents-mon-search "$key" \
    "run-shell \"'$BIN' key 'text-$hex'\"; switch-client -T agents-mon-search"
  code=$((code + 1))
done
bind_search() {
  key="$1" action="$2" next="$3"
  tmux bind-key -T agents-mon-search "$key" \
    "run-shell \"'$BIN' key '$action'\"; switch-client -T '$next'"
}
bind_search Up up agents-mon-search
bind_search Down down agents-mon-search
bind_search C-p up agents-mon-search
bind_search C-n down agents-mon-search
bind_search BSpace backspace agents-mon-search
bind_search C-u clear-search agents-mon-search
bind_search Escape escape agents-mon
bind_search C-c escape agents-mon
# Enter accepts query and hands j/k back to filtered navigation. A second Enter
# in the normal table jumps to selected result.
bind_search Enter enter agents-mon
tmux bind-key -T agents-mon-search Any 'switch-client -T agents-mon-search'

# Wheel events reach both plugin tables. Over sidebar they move selection;
# elsewhere native wheel behavior remains unchanged.
bind_wheel() {
  table="$1" key="$2" dir="$3" native="$4"
  tmux bind-key -T "$table" "$key" \
    if-shell -F '#{==:#{pane_title},agents-mon}' \
    "run-shell -b \"bash '$DIR/scripts/scroll.sh' '#{pane_id}' $dir\" ; switch-client -T $table" \
    "$native"
}
for table in agents-mon agents-mon-search; do
  bind_wheel "$table" WheelUpPane up \
    'if -Ft= "#{||:#{pane_in_mode},#{mouse_any_flag}}" "send-keys -M" "copy-mode -e; send-keys -M"'
  bind_wheel "$table" WheelDownPane down 'send-keys -M'
done
tmux set-option -g @agents-mon-nav-version 12

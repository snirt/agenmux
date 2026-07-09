#!/usr/bin/env bash
# Restore the layout of the window the sidebar just left (freed width gets
# dumped onto one neighbor otherwise). $1 = window id; without it, use
# @agents-mon-prev-win as set by the native follow hook.
win="${1:-$(tmux show-option -gqv @agents-mon-prev-win)}"
[ -n "$win" ] || exit 0
tmux set-option -gu @agents-mon-prev-win 2>/dev/null
old_layout="$(tmux show-option -gqv "@agents-mon-layout-${win}")"
[ -n "$old_layout" ] || exit 0
# the layout string embeds absolute sizes — restoring it into a window that
# changed size since the save leaves dead (dotted) window area. Only restore
# on an exact size match; otherwise let tmux keep its own layout.
size="${old_layout#*,}"; size="${size%%,*}"
[ "$size" = "$(tmux display-message -p -t "$win" '#{window_width}x#{window_height}' 2>/dev/null)" ] \
  && tmux select-layout -t "$win" "$old_layout" 2>/dev/null
tmux set-option -gu "@agents-mon-layout-${win}"
exit 0

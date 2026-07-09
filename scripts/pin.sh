#!/usr/bin/env bash
# Hook handler (window-resized): client/terminal resizes scale panes
# proportionally, growing the sidebar past its set width — and follow.sh
# would then remember the scaled width as a "manual resize". Snap it back.
sb="$(tmux show-option -gqv @agents-mon-sidebar)"
[ -n "$sb" ] || exit 0
w="$(tmux show-option -gqv @agents-mon-width)"
tmux resize-pane -t "$sb" -x "${w:-30}" 2>/dev/null
exit 0

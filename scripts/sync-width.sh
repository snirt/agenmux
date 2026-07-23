#!/usr/bin/env bash
# Hook handler (window-layout-changed, mirror mode): keep mirror widths and
# the rendered content width in sync.
# - window size changed since last look => proportional rescale artifact:
#   snap that mirror back to the set width (same job as pin.sh)
# - window size unchanged but a mirror's width differs => the user dragged
#   the mirror border: adopt it as the new global width everywhere (the
#   daemon sizes the frame to the smallest mirror, so content follows)
[ "$(tmux show-option -gqv @agents-mon-on)" = 1 ] || exit 0
W="$(tmux show-option -gqv @agents-mon-width)"; W="${W:-30}"

adopt=""
while IFS=$'\t' read -r pane win width wsize; do
  [ -n "$pane" ] || continue
  stored="$(tmux show-option -gqv "@agents-mon-winsize-${win}")"
  if [ "$wsize" != "$stored" ]; then
    tmux set-option -g "@agents-mon-winsize-${win}" "$wsize"
    [ "$width" != "$W" ] && tmux resize-pane -t "$pane" -x "$W" 2>/dev/null
  elif [ "$width" != "$W" ]; then
    adopt="$width"
  fi
done <<EOF
$(tmux list-panes -a -f '#{==:#{pane_title},agents-mon}' \
    -F '#{pane_id}	#{window_id}	#{pane_width}	#{window_width}x#{window_height}')
EOF

if [ -n "$adopt" ]; then
  tmux set-option -g @agents-mon-width "$adopt"
  tmux list-panes -a -f '#{==:#{pane_title},agents-mon}' -F '#{pane_id}' |
    while read -r p; do tmux resize-pane -t "$p" -x "$adopt" 2>/dev/null; done
fi
exit 0

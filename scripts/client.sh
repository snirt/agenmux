#!/usr/bin/env bash
# Print the user's client: the most recently active non-control client.
# The 'focused' flag is useless here — terminals that never report focus
# loss leave it set on every client. $1 overrides the output format.
FMT="${1:-#{client_name}}"
tmux list-clients -f '#{?#{m:*control-mode*,#{client_flags}},0,1}' \
  -F "#{client_activity} $FMT" | sort -rn | head -n 1 | cut -d' ' -f2-

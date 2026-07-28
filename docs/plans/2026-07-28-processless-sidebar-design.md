# Processless preserved sidebar

## Decision

Keep one reserved tmux pane in every eligible window, but create it as an
empty input pane (`split-window -I`) instead of running `agents-mon mirror`
inside it. Empty panes remain alive and participate in the layout while
reporting `pane_pid=0`, so switching windows still causes no layout bump.

The existing Rust daemon becomes the only persistent `agents-mon` process. A
deep `PaneWriters` module hides tmux's input-pane lifecycle behind two
operations:

- reconcile the set of currently visible sidebar pane IDs;
- emit one completed frame to every reconciled writer.

Internally, each visible pane has one blocking
`tmux display-message -I -t <pane>` child whose stdin is held by the daemon.
Switching windows changes the reconciled set: hidden-pane writers close, new
visible-pane writers open, and the current frame is immediately replayed.
Multiple attached sessions can each keep their visible sidebar live. With no
real client attached, one active pane remains warm so the first attach and
detached integration paths have a current frame; other detached sessions cost
no writer.

Agent discovery remains a two-second scan. Tmux notifications wake the daemon
immediately for focus, layout, session, and window changes, but pane output
cannot reliably express semantic agent state. This keeps layout/render
delivery event-driven without introducing `%output` floods or fragile terminal
parsing.

## Interaction

An empty pane cannot consume keyboard input. The configured sidebar toggle
therefore selects the sidebar and enters a dedicated tmux key table. The table
starts with the user's root bindings so normal tmux pane navigation still
works, then overrides `j`, `k`, arrows, `Enter`, `l`, `?`, `u`, `q`, `Escape`,
and `Q` for sidebar actions. Movement keeps the table active. `q`/`Escape`
return focus to the previous work pane and restore the root table; `Q` closes
the sidebar. The `❯` cursor is green only while navigation is active.

Mouse clicks read the daemon's shared visible-row map, which matches the one
currently rendered frame. Teardown closes writers, kills empty panes, restores
saved layouts, removes key/liveness state, and leaves the user's active work
pane selected. `q`/`Escape` leave navigation while keeping the preserved
sidebar open; `Q` explicitly closes it.

## Verification

The tmux integration test is the public-interface tracer:

1. toggling creates one titled sidebar per window with `pane_pid=0`;
2. exactly one visible sidebar receives a rendered frame;
3. switching windows preserves every layout and moves live rendering;
4. concurrently requested additions still create one sidebar;
5. dragging width remains synchronized;
6. `q`/`Escape` leave navigation while `Q` closes all sidebars and restores
   layouts;
7. no persistent `agents-mon mirror` process is required.

The existing popup and bash fallback paths remain unchanged.

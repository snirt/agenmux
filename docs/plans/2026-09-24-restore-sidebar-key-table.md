# Restore the sidebar key table on session return

## Observation

The live daemon and key FIFO exist, but a client can display a selected sidebar while its key table is `root`. The pane-selection hook does not run when a client returns to a session whose sidebar was already selected.

## Plan

1. Reproduce the session-return transition with an attached tmux client in Docker and assert both the selected pane and its key table.
2. Restore the Agenmux key table when a client enters a window or session with an already selected sidebar. Keep the existing pane-selection hook.
3. Run the focused regression and full project suite in Docker, then activate the updated binary through `make dev-use`.

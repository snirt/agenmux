# Control client dead-socket fix

## Goal

When a control client connects through a stale `$TMUX` socket, report the missing server without starting a new one. Preserve connections to live servers.

## Steps

1. Pass tmux's `-N` flag in the shared `Tmux::connect_with_output` path used by CLI scans and runtime monitoring.
2. Strengthen the existing dead-socket CLI regression to check that `scan` and `list` return an error and leave no socket behind.
3. Run the focused CLI test, the project Rust tests, and an isolated live-server scan to confirm successful connections still work.

The reported long-running `setup --if-needed` hang was not reproduced in the issue and is outside this fix.

## Verification

- The focused dead-socket CLI regression passed for both `scan` and `list`.
- The full Rust test suite passed with local tmux socket access.
- `scan` succeeded against a private live tmux server.

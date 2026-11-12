# AX-255 slice 1: route shared code through the multiplexer seam

Parent: #255. This PR covers only the first sub-issue; the other six
(input split, module layout, repo clutter, plan naming, tests layout,
container docs) follow as separate PRs.

## Goal

No production code outside `src/mux/` names `mux::tmux` or `mux::rex`.
Shared code calls backend-neutral operations on `Multiplexer` or on the
`mux` module; backend mechanics stay inside the adapters. Behavior is
unchanged on tmux and Rex.

## Changes

1. **Runtime dir.** New `src/mux/runtime.rs` owns `runtime_dir`,
   `prepare_runtime_dir` and `prepare_dir`. `tmux::runtime_dir` and
   `rex::runtime_dir` stay as each backend's resolver; the tmux-side
   `prepare_runtime_dir` goes.
2. **tmux integration lives in the tmux adapter.** `setup.rs`, `panes.rs`
   and `toggle.rs` only ever drive tmux (key tables, hooks, sidebar panes,
   the toggle binding). They move to `src/mux/tmux/`. The tmux key, click
   and wheel senders move out of `input.rs` into `src/mux/tmux/keys.rs`;
   the FIFO writers they share with the sidebar stay in `input.rs` (the
   rest of the input split is sub-issue 2).
3. **One entry for backend commands.** `mux::run_command(args, plugin_dir)`
   dispatches the internal commands tmux bindings and Rex actions call
   (`setup`, `toggle`, `key`, `click`, `wheel`, `pane-*`, `teardown`,
   `rex-*`). `main.rs` falls through to it.
4. **`as_tmux` is gone.** Its callers use new trait operations with no-op
   defaults: `key_sink`, `settle`, `key_mode`, `install_keys`, `dock`,
   `inherit_header`, `notice`.
5. **Connection-less operations** on `mux`: `reap_dead_owner`,
   `reinstall_keys` (config reload), `note`, `restart_view`, `close_view`
   / `ClosedView::reopen` (update), `notification_server` /
   `open_notified` (notification clicks).
6. **Gate.** `tests/mux-boundary.sh` rejects any `mux::tmux` / `mux::rex`
   (including `mux::{self, tmux}`) outside `src/mux/` with no allowlist.
   `tests/mux-boundary-self-test.sh` proves it fails on a planted import.

## Verification

- `cargo test --locked`, `tests/run.sh`.
- Self-test: planted `use crate::mux::tmux;` outside `src/mux/` fails.
- Live tmux pane: toggle, popup, key and click jumps, agent `working` →
  `idle`. Rex: same where available.

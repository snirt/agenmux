# AX-220: Restore saved layouts on tmux 3.8 JSON window_layout

Issue: GitHub #220, "Layout restore silently skipped on tmux 3.8 (window_layout is now JSON)".
Status: implemented and verified locally.

## Findings

- Reproduced locally on tmux 3.8 (scratch socket): `#{window_layout}` from a CLI client is
  `{"V":2,"L":{"t":"h","w":120,"h":40,...}}`. `select-layout '<json>'` restores it correctly.
- `src/panes.rs:310` saves the layout via `tmux::command` (CLI subprocess), so on 3.8 the
  stored `@agenmux-layout-<win>` value is JSON.
- `layout_size` (`src/panes.rs:565`) takes the token after the first comma. For JSON it yields
  `"L":{"t":"h"`, never equal to `#{window_width}x#{window_height}`, so `restore_layout` skips
  `select-layout` silently.
- Only the size guard is broken. `snapshot.rs` reads layouts over control mode (old format) and is
  unaffected. Previously saved old-format values must keep working.
- Existing `teardown_discards_a_layout_after_window_size_changes` (`tests/plugin.rs:287`) only
  covers the negative path, which passes for the wrong reason on 3.8.

## Decision

Teach `layout_size` both formats (issue option 2). Reading over the control pipe would move a CLI
toggle path onto the daemon's pipe for one value; not worth it.

- Old format: unchanged (`csum,WxH,...`).
- JSON (starts with `{`): the root node's `"w"` and `"h"` are the first occurrences in the string
  (`"L":{"t":..,"w":..,"h":..`). Extract them with a small string scan; no `serde_json`
  dependency.
- Return an owned `Option<String>` (`"WxH"`) so both formats compare against
  `#{window_width}x#{window_height}` unchanged at the call site.

## Steps

1. `src/panes.rs`: update `layout_size` as above; adjust the `restore_layout` comparison for the
   owned return.
2. Unit test `layout_size_is_the_absolute_window_size`: add a real sanitized 3.8 JSON layout
   (root `120x40` with children of other sizes, proving the root is chosen) and a malformed JSON
   case returning `None`.
3. Integration test in `tests/plugin.rs` beside the size-change test: save the current layout,
   add a pane marked `@agenmux 1` (teardown only restores windows holding a sidebar), change the
   layout, run `agenmux teardown`, assert the original layout is restored. Fails on the old code.
   Runs whichever format the installed tmux emits: 3.8 locally, the old format on older CI tmux.
4. Run `cargo test --locked` and `tests/run.sh`.

## Live verification

On a scratch socket (`TMUX=<scratch>,0,0` for every agenmux call): split a window, open the
sidebar (`toggle split`), close it (`key close`; a repeated toggle only refocuses), and confirm
`#{window_layout}` matches the pre-toggle value. Done on tmux 3.8: restored exactly.

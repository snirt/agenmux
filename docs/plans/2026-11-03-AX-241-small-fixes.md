# AX-241: popup size clamp and test flakes

Scope of #241 after review: #162 and the partial #226 items. #204 (distinct
state glyphs) is deferred and #207 (NO_COLOR, v/V marker) is dropped; the PR
mentions both without closing them.

## #162: clamp a fixed popup size against the named client

- `display-message -p -c <client>` without `-t` expands `#{client_*}` for
  whichever client owns the command's default session, not the named one.
- `toggle.rs` reads the named client's size from `list-clients` once per
  launch iteration and uses it for the fixed width, fixed height, and the
  auto height cap.
- Regression test: two attached clients of different sizes on different
  sessions, the larger attached last; a fixed size larger than the small
  client must still open a popup on it.

## #226 (partial)

- Rename a staged `agenmux-scan-cache` into place instead of rewriting it, so
  readers (`#{agenmux}`, tests) never see a truncated file.
- `navigation-key-table`: wheel steps reach the daemon through control-mode
  buffer notifications, `gg` through the key FIFO, so `gg` could overtake the
  burst's tail. Wait until the daemon trace shows every wheel step first.
- `recently_closed_forget_confirms_in_split_and_popup_input_modes`: the daemon
  rewrites the snapshot on its own, so assert the record is still there rather
  than byte equality.
- `all_panes_reload_preserves_daemon_and_selection`: fixed by the atomic cache.
- `input::tests::chunked_escape_and_paste_wait_for_in_flight_bytes`: send each
  byte once the reader has drained the previous one, instead of fixed sleeps
  that a slow runner can stretch past the parser's timeouts.
- `returning_to_selected_sidebar_restores_its_key_table`: read the client's
  pane and key table from `list-clients`, the same wrong-client pattern as
  #162.

Out of scope: tmux 3.8 CI job, the container-only
`entrypoint-removes-legacy-follow-hooks` failure.

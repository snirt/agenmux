# AX-208: Bound tmux replies and pane writes; stop rewriting the rows file

Issue: GitHub #208, "Blocking tmux calls and pane writes, plus the rows file rewritten every 250 ms".
Status: implemented and verified in the tmux 3.7b container.

## Findings

- `Tmux::run` (`src/tmux.rs`) writes a command and blocks in `read_block` until a matching
  `%end`/`%error`. `read_line_retry` uses `BufReader::read_until`, which blocks with no deadline.
- `PaneWriter::open` (`src/pane_writers.rs`) opens the FIFO with `O_NONBLOCK` and then clears it,
  so `emit` blocks whenever a pane reader stops draining (stalled PTY, stopped process).
- `Sidebar::emit` (`src/sidebar/render.rs`) stages and renames the rows file on every render, even
  when the rows are identical; while anything animates that is every 250 ms.
- The event loop treats `TmuxError::Io` from a scan as fatal (daemon exits and tears down) and
  `TmuxError::Error` as a soft failure. Most other callers ignore errors (`unwrap_or_default`).

## Decisions

- Reply deadline: 2 s per reply. A timeout is a soft `TmuxError::Error`, never a pipe failure, so
  the daemon and its panes survive a transient stall.
- Realignment: on timeout, send a uniquely numbered `display-message -p am-sync-N` marker. Until
  that marker line is seen (by `run`, `sync`, or `drain_notifications`), `run` fails fast without
  writing, so a wedged server costs one stall rather than one per command, and a late reply is
  never paired with a later command. `sync` uses the same numbered markers.
- Pane writes stay nonblocking. On `EAGAIN`, poll for `POLLOUT` within a shared ~100 ms budget per
  frame so a live reader still receives large frames; past the budget, drop the writer. The
  periodic reconcile reopens it and replays the full frame (frames start with `ESC[H` and repaint
  every row, so a torn frame is overwritten).
- Rows file: remember the last written rows and rewrite only when they change.
- Ownership check: `superseded()` read `@agenmux-generation` and `@agenmux-control-client`
  with two commands. A hook-triggered empty block arriving after the sync barrier shifted both
  replies by one, so the generation value was read as a foreign control client and the daemon
  quit. The race predates this change, but the new timing made it frequent:
  `tmux_management_creates_and_deletes_stable_targets` failed ~15% of container runs (0/8 on
  master). Both options now come
  back in one `am-owner|<generation>|<client>` reply, and anything without the tag is not a claim.

## Verification

- Unit: rows file untouched when rows are unchanged, rewritten when they change.
- Unit: a FIFO whose reader never drains returns within the budget and drops the writer; the
  existing large-frame test still passes with a live reader.
- Unit: scripted control client that replies late: `run` times out, fails fast while the marker is
  outstanding, and returns the correct body after realignment.
- Unit: an untagged or empty reply never counts as an ownership claim.
- `cargo test --locked` and `tests/run.sh` in the container; the plugin test above passed 20/20
  after the ownership fix.

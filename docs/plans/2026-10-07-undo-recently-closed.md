# Undo list for recently closed sessions and windows (#179)

## Status

Implemented on top of #178 (merged as #180): its snapshot file, layout
capture, `replay`, `AGENT_RESUME` and `tmux_management.resume_agents`.

## Decisions

- The log is per server: it lives in the server's snapshot file, moves to
  `.prev` with it on a restart and is not restorable there.
- `u` is a fixed key, like `z`/`Z`, `R`, `x`: a configured chord on `u` wins.
  No `[keys.normal] undo`. `versions` defaults to `U`, so a configured
  `versions` key keeps working unchanged.

## Recording closes (`src/snapshot.rs`)

- `Session` and `Window` gain `#[serde(skip)] id` (`$n`/`@n`), filled by
  `layout()`.
- `Store::set_layout` diffs the previous capture against the new one by id:
  a missing session id → one `session` entry (its windows are not logged
  separately); a missing window id in a live session → one `window` entry. A
  rename keeps its id, so it is not a close. The first capture after `open`
  has no previous one, so it logs nothing.
- An empty scan keeps the last layout (#178) and logs nothing, so a dying
  server or desynced read never floods the log.
- `Snapshot.closed: Vec<Closed>`, newest first, capped at 20:
  `time` (unix secs), `kind` (`session`/`window`), `id`, and a `Session` holding
  the captured session or the one window.
- Close notifications (`%window-close`, `%sessions-changed`, ...) already force
  a full scan, and full scans capture layout, so the diff runs right after a
  close. Also capture on `%layout-change` scans, so a split made just before a
  close is recorded.
- Popup and split sidebars can run together, so the log is read-modify-write
  on the file: append dedupes by `(kind, id)`; forget/restore remove from the
  file; `save` writes the file's current `closed` back unchanged. `write` keeps
  the file while `closed` is non-empty.

## Undo overlay (`src/sidebar/overlay.rs`)

- `Overlay::Undo { sel }`, rendered with the help/versions chrome:

  ```text
  Recently closed
  ❯ 10:42  session  work      3 windows, 5 panes, 2 agents
    10:31  window   play:2    2 panes
  ↵ restore · d forget · esc close
  ```

- Newest entry preselected, so `u` `Enter` undoes the last close.
- An entry is blocked (dimmed, `Enter` ignored) when its session name is live
  (session entry) or its `session:index` is live (window entry).
- `d` forgets; `Enter` restores, removes the entry and closes the overlay;
  an empty list shows `nothing closed yet`.
- Restore needs `tmux_management.enabled`; otherwise the hint omits `↵ restore`.

## Restore

- `replay()` with a one-session `Snapshot` built from the entry and the live
  `(session, index)` set. A window entry whose session is gone recreates the
  session with that window.
- Agents resume only with `tmux_management.resume_agents = true` (#178).

## Keys

- `input.rs`: protocol byte `u` → `Key::Undo` in the fixed-key fallback;
  `send_key` gains `undo`. `keys.rs`: `Versions` default `U`.
- `setup.rs`: fixed `u` → `undo` binding, replaced by a configured chord.
- Help overlay lists `u  recently closed` while no action claims `u`.
- Docs: `docs/usage.md` key table and a "Recently closed" section,
  `docs/configuration.md` versions default, `examples/config.toml`,
  `src/app_config/mod.rs` sample, `RELEASE_NOTES.md` calling out `u` → `U`.

## Tests

- Unit: close diff (session close, window close, rename is not a close, first
  capture logs nothing, empty scan logs nothing), cap at 20, dedupe across two
  stores, forget, `write` keeps a closed-only file, blocked detection, restore
  of a session and of a window into an existing session (fake runner), key
  defaults (`u` undo, `U` versions, configured `versions = ['u']` wins).
- Live, on a scratch socket: close a window and a session, rename one, `u`
  lists newest first, `Enter` restores layout and paths, a reused name dims.

## Known ceilings

- Closes while no sidebar runs are not logged.
- Layout and paths are as of the last capture (2s periodic, or the last
  structural/layout notification); a `cd` just before a close may be missed.

## Pane entries (added after review)

- Deleting one pane of a window that stays open is logged too (`%n` id). The
  entry holds the whole window as it was, so ids now persist in the snapshot
  file (meaningless on another server, ignored by `R`).
- Restore relaxes "never touch a live window" for panes only: `split-window -d`
  after the nearest saved neighbour still open (`-b` before the next one when
  it was first; the window itself when none is left), then `select-layout`
  with the saved layout. tmux rejects that layout unless the pane count
  matches, which leaves its own split.
- A pane entry is blocked once its window id is gone; the window entry covers it.

## Gap review

- A new sidebar pane can be scanned once before it is marked; marking drops it
  from the scan. Pane closes are therefore confirmed against `list-panes -a`
  (queried only when the scan suggests one). Window and session closes trust
  the scan: a window left holding only a sidebar pane is closing.
- Recording a window or session close drops that window's pane entries, which
  could never be restored again.
- `Enter` on a blocked entry keeps the list open.

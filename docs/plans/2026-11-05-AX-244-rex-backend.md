# Rex multiplexer backend (AX-244)

## Goal

Rex becomes the second `Multiplexer` behind the #243 seam. Rex sessions,
windows and terminal blocks stand in for tmux sessions, windows and panes. The
sidebar is a terminal block at the side of a window, rendered by the same
shared daemon tmux uses. tmux behaves exactly as before.

Reference: `poc/rex-multiplexer` (`src/mux/rex.rs`, `rex/agenmux.lua`,
`docs/rex.md`, `tests/rex.rs`). Its code is ported onto master's trait, not
merged: the POC predates the seam and carries an earlier trait. Rex runs on
macOS only; it has no Linux client yet.

## Layout

| Path | Holds |
| --- | --- |
| `src/mux/mod.rs` | backend selection (`AGENMUX_MUX`, `--mux`, `TMUX`/`REX_SESSION`, the running server) and connectionless dispatch |
| `src/mux/rex.rs` | `impl Multiplexer for Rex`, Rex CLI/API client, placement, toggle, popup, update close/reopen |
| `src/mux/rex/*.lua` | scan, placement and popup scripts the backend runs through `rex do` |
| `rex/agenmux.lua` | what `init.lua` loads: actions and keys |

## Shared code

Policy stays shared; Rex adds primitives only.

1. **Input without key tables.** Rex has no key bindings to route keys, so
   each sidebar block's reader forwards its terminal input (keys, SGR mouse)
   to the daemon's input FIFO, which decodes it tty-style. tmux keeps its key
   FIFO; the reader forwards only when the backend does not route keys.
2. **Width.** Rex places blocks by ratio, so a sidebar lands a few cells off
   the configured width. `SidebarPane::settled` tells the drag probe a width
   the backend could not set exactly from one the user dragged.
3. **Frames.** Readers keep whole frames and clip them to their own size,
   since ratio-placed blocks differ slightly in width.
4. **Feedback.** Rex has no message line or paste buffer: `message` returns
   false and the sidebar flashes the text; `copy` returns the OSC 52 escape,
   which the daemon sends with its next frame.
5. **Restore.** Rex renumbers windows after a close: `Live` matches windows
   by id where the backend says indexes shift.
6. **Notifications.** A click target names the backend; non-tmux backends go
   through `jump`.
7. **Hygiene.** `config reload` skips tmux bindings, `autoupdate` asks the
   backend instead of `$TMUX`, `mux::Error` names the backend, `update`
   closes and reopens Rex sidebars.

Deferred from the POC: the pane size-report socket (latency only), the queued
pane writer, the resize stress test.

## Installation

One install serves every multiplexer: `install.sh` clones agenmux to
`${XDG_DATA_HOME:-~/.local/share}/agenmux`, not tmux's plugin dir. An install
already in `~/.tmux/plugins/agenmux` keeps updating in place. tmux loads it
through a `run-shell` line (never TPM's `@plugin`, which would clone a second,
tmux-only copy); Rex through a `dofile(...)` line in `init.lua` with full paths,
since Rex actions see a bare `PATH`.

On a terminal a checklist shows tmux and Rex as integrated now. Checking one
adds its lines after asking (keys: tmux `A`/`a`, Rex `alt+s`/`alt+p`, Mac dead
keys refused) and reloads it; unchecking one removes its lines after asking,
unbinds its keys and closes its sidebars (`agenmux teardown` works for Rex
too). `AGENMUX_MUX=tmux|rex|both`, or no terminal, sets up the chosen ones and
removes nothing. Rex alone needs no tmux.

## Layout fixes found live

- A new window's blocks report Rex's default 80x24 until the app sizes them
  (no `size.desired`); placement uses the last settled sidebar's share or cell
  size instead, and unsized blocks never drive the divider feedback.
- One frame serves every sidebar, so it folds to the narrowest. Rex resizes
  only shown sidebars; for self-sized backends the watched one sets the width.
- A double click on a pane row jumps: tmux's click helper does that itself,
  Rex forwards every double click to the sidebar.

## Agent state

Rex keeps what programs report of themselves (OSC 7501,
`program_status`) and drops a record when its program exits. Verified live:
Claude Code, Codex, OpenCode and Pi report working, blocked (Claude, Codex, OpenCode)
and done/idle. Oh My Pi and Hermes report nothing. The scan fetches it per
block. A reported idle or blocked is the state and skips the capture. A
reported working still captures, and a blocked screen wins: Pi reports
working while its question waits. The screen rules read the rest, such as
Claude at the folder-trust prompt, which reports nothing.

## Testing

- Rex backend unit tests in `src/mux/rex.rs` (id parsing, listing decode,
  layout rebuild, references, event folding) and `tests/rex-layout.lua`
  (placement and divider feedback). Shared suites are not duplicated.
- `tests/install-script.sh` drives the checklist through `expect`.
- `tests/mux-boundary.sh` also rejects `rex` spawns and `mux::rex` outside
  `src/mux/` and the integration modules.
- Live, on a scratch Rex server: sidebar, popup, width drag, restore,
  notifications.

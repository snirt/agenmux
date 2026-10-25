# One agenmux daemon, any multiplexer

## Problem

Every Rex sidebar block runs a full `agenmux sidebar`. With N sidebars there
are N scanners, N `rex events` streams and N copies of the sidebar state, so
they have to coordinate:

- an elected leader places new windows' sidebars and sends notifications
- each sidebar tracks its own width (`Width`, `rex-sidebar-ratio`) and races
  the others and the app: a resize spreading to the others, the app evening
  out splits, and a sidebar mistaking its own save for another's
- cursor, folds and scroll differ per window

tmux avoids this with one `agenmux daemon` that scans and renders, and thin
`agenmux sidebar-pane` readers in each sidebar pane. That daemon, though, is
written against tmux: `daemon.rs`, `panes.rs`, `toggle.rs` and `input.rs`
run tmux commands and read `@agenmux-*` options directly. And the Rex
backend holds policy of its own (`Width`, `spread`, `reap`, `elect`) that
duplicates what the tmux daemon does.

## Goal

agenmux is agnostic to the multiplexer. One daemon, one `sidebar-pane`, and
one set of sidebar policies serve tmux and Rex. A backend answers questions
and performs primitive actions. It decides nothing. The tmux path behaves as
it does today.

## Principle

The core owns every decision: when a sidebar opens or closes, its width,
which panes get frames, what a drag means, who owns the sidebars, when the
daemon exits. A backend owns only how to ask its server, and the facts that
differ between servers, such as how keys reach the sidebar.

A test for every seam method: could a third multiplexer (zellij, WezTerm)
implement it without knowing agenmux's policy? If the method's name or
arguments mention a tmux or Rex concept (options, key tables, ratios,
generations stored in options), it is in the wrong place.

## Seam

Primitives only. Ids are opaque strings; sizes are cells.

```rust
/// Facts about the server.
fn identity(&mut self) -> Option<String>;       // tells this server from the one before a restart
fn sidebar_panes(&mut self) -> Vec<SidebarPane>; // pane, window, session, cols, rows,
                                                 // window cols/rows, window pane count, window active
fn visible_windows(&mut self) -> HashSet<String>; // windows a real client shows
fn self_pane(&self) -> Option<String>;            // the pane this process runs in

/// Primitive actions.
fn open_sidebar(&mut self, window: &str, side: Side, cols: usize, command: &[&str], focus: bool)
    -> Result<String, TmuxError>;
fn close_panes(&mut self, panes: &[String]);
fn resize_panes(&mut self, cols: &[(String, usize)]); // batched: one fork or one script

/// Capabilities.
fn routes_keys(&self) -> bool; // tmux key tables deliver keys to the daemon;
                               // otherwise the sidebar pane forwards its input
```

`SidebarPane` is today's `M` in `daemon.rs`.

Each backend keeps how it does each one. The tmux backend uses
`list-panes`, `list-clients`, `split-window`, `resize-pane` and
`kill-pane`. The Rex backend uses the scan listing, the app clients' active
windows, `rex_place.lua`, `block.close`, and `session.resize_pane` at
`cols / window cols`, which `fit()` already does.

## Core

These live outside `src/mux/`, and neither backend repeats them:

- **Daemon** (`sidebar/daemon.rs`): scan, render and event loop, as today.
  `mirror_tick` keeps its logic (fold to the narrowest pane, duplicate
  cleanup, drag adoption, put-back after a neighbour closes, visible-only
  writers, exit after the last pane), calling the seam instead of tmux.
- **Placement**: one sidebar per visible window, others lazily, none in a
  window the user toggled off, close one in a window with no real pane left.
  This is `pane_add` and the toggle today, plus Rex's `spread` and `reap`,
  merged into one.
- **Width**: one width in columns, `display.sidebar_width`, saved by drag
  adoption. Rex's share and `rex-sidebar-ratio` go away, along with
  `Width`, `track_ratio` and `RESHAPE`. The tmux daemon's pane-count check
  already handles a split or close reshaping the layout.
- **Ownership and runtime state**: generation, owner and width live in
  agenmux files under the runtime dir: a lock file, plus a state file
  readable without a server. The runtime dir derives from `identity()`.
  tmux keeps publishing its `@agenmux-*` options for its key bindings and
  hooks, as a backend side effect, until those read the files.
- **`sidebar-pane`** (`pane_writers::run_pane`): identity from
  `self_pane()`. When the backend does not route keys, it forwards its raw
  terminal input, mouse reports included, to the keys FIFO in a new packet
  type: raw input from client X. It stays a dumb pipe. Key resolution
  cannot happen in the pane: which keymap applies depends on daemon state
  (search, overlay editing, a pending `dd`/`yy` sequence).
- **Raw input in the daemon**: today the daemon reads the FIFO with fixed
  `protocol_keys(mode)`, because tmux key tables already resolved the
  user's bindings and sequences. A raw-input packet is decoded the way tty
  mode decodes stdin instead: `normal_keys`/`search_keys`,
  `key_sequence` continuations and `settings_keys`. Mouse reports become
  `Key::Press` and wheel keys, as in tty mode. So the tty decoding path
  serves both, and nothing about key resolution is per multiplexer.
- **Notifications**: the daemon sends them, so `notifies()`, `quiet()` and
  `elect` go away.

The popup stays a tty `agenmux sidebar` on both.

## What stays backend-specific

- tmux key tables and their install (`setup.rs`): a tmux way of delivering
  keys, behind `routes_keys()`.
- tmux control mode and hook-safe forking: how the tmux backend talks to
  its server.
- `rex/agenmux.lua` and `rex_*.lua`: how Rex starts actions and runs
  batched calls.

## Order

Each step lands green on its own, with tmux behaviour unchanged:

1. Seam: add the primitives. The tmux impl moves `daemon.rs`'s commands
   behind them verbatim, and `mirror_tick` calls them.
2. Ownership and width state in runtime-dir files. tmux still mirrors them
   to its options.
3. Placement: `pane_add` and the toggle call the seam. Rex's `spread` and
   `reap` fold into the same code.
4. Raw-input packets: the daemon decodes them through the tty key path,
   including mouse `Key::Press` in daemon mode. `sidebar-pane`:
   `self_pane()`, raw forwarding behind `routes_keys()`.
5. Rex: implement the seam and start the daemon from `rex-toggle` and
   `rex-sidebars`. Remove `elect`, `Width` and the ratio file.
6. Version switch through the generation file. Docs: `docs/rex.md` (width
   now `display.sidebar_width`, focus, mouse), `RELEASE_NOTES.md`.

Before step 1, confirm with the user whether any file should be split into
modules (AGENTS.md). The plan only moves code between existing modules.

## Verification

- `cargo test --locked` and `tests/run.sh` at every step. The tmux
  integration tests are the guard that tmux is unchanged.
- Unit: a fake backend implementing the seam drives `mirror_tick`'s drag,
  put-back and duplicate cases without any server. The Rex `sidebar_panes`
  reads a sanitized scan listing; `sidebar-pane` turns key and mouse bytes
  into FIFO packets.
- Live, in a scratch Rex session the app shows, and in a scratch tmux
  server, each with the deployed binary confirmed first:
  - one `agenmux daemon`, and one `sidebar-pane` per window
  - keys, search, paste, click, double-click and wheel in each window
  - a border drag moves every sidebar; split and then ctrl-d keeps the width
  - one notification per event
  - closing the last sidebar ends the daemon; the toggle starts it again

## Open questions

- With one frame for all sidebars, the cursor follows one focus. Use the
  client the user last acted in, as tmux does, for Rex too?
- Frame height: `watched_height` uses the watched window's height. Is that
  right when Rex windows differ in height?
- Should tmux's key bindings and hooks move to the runtime-dir files in this
  change, so `@agenmux-*` options stop being state? Or leave that for later?

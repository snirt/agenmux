# Multiplexer seam (AX-231)

## Problem

agenmux is wired to tmux: about 230 call sites across 18 files fork `tmux`,
call `crate::tmux::command*`, or send tmux command strings over the control
pipe. A second multiplexer would have to touch every one of them.

## Goal

Shared code (scan, sidebar, daemon, overlays, snapshot, settings) reaches the
server only through a `Multiplexer` trait. tmux is its only implementation and
behaves exactly as today. No other backend lands here.

Rex is the expected second backend, but it is unreleased. The
`poc/rex-multiplexer` branch is the reference: each seam method below was
checked against how that branch does the same thing on Rex, so a later Rex
backend fits without reshaping the trait. No Rex code is ported.

## Principles

- **A method enters the trait only if a second backend would really
  implement it.** Where the Rex branch left a method a no-op (key sinks, key
  bindings, key tables, header colors, `sync`, `run`), it stays inside the
  tmux backend or behind `as_tmux()`.
- **Every method is required.** No default bodies except `as_tmux`, so a new
  backend answers each one and a missing feature fails to compile instead of
  silently doing nothing.
- **Primitives only.** Backends answer questions and perform actions. Policy
  (what to scan, when to rescan, which panes get sidebars, widths, ownership,
  undo, waiting for a client to arrive) stays in shared code.
- **No backend concepts in the API.** No format strings, options, key tables
  or ratios. Ids are opaque strings; nothing outside the tmux backend assumes
  `$`/`@`/`%` prefixes.
- **Data in, data out.** No callbacks into the core, so a scripted adapter
  stays possible later.
- **YAGNI.** Only what shared code calls today. Backend selection
  (`AGENMUX_MUX`, auto-detect) arrives with the second backend.

## Layout

| Path | Holds |
| --- | --- |
| `src/mux/mod.rs` | `Multiplexer` trait, shared types, `connect()`, `runtime_dir()` |
| `src/mux/tmux.rs` | `impl Multiplexer for Tmux`, tmux snapshot replay, the tmux escape hatch |
| `src/mux/tmux/control.rs` | Today's `src/tmux.rs` (control-mode client, notification parsing, command helpers, runtime dir), moved |
| `src/mux/fake.rs` | `#[cfg(test)]` in-memory backend for shared-code tests |

Renames: `TmuxError` → `mux::Error`, `PendingChanges` → `mux::Changes`.
`poll_by`, a plain `poll(2)` helper, moves out of the tmux client to
`input.rs`.

## Seam

```rust
pub enum Kind { Session, Window, Pane }
/// A record and the records above it. Every backend may need the whole path:
/// Rex scopes each call by session.
pub struct Ids<'a> { pub session: &'a str, pub window: &'a str, pub pane: &'a str }
pub enum New {
    Session { name: String, cwd: String },
    Window { session: String, name: String, cwd: String },
    Launch { session: String, cwd: String, argv: Vec<String> },
}

pub trait Multiplexer {
    // Scan
    fn panes(&mut self, skip: Option<&str>) -> Result<Vec<ParsedPane>>;
    fn capture(&mut self, pane: &str) -> Result<String>;
    fn focus(&mut self) -> Option<ClientFocus>;

    // Changes
    fn events_fd(&self) -> Option<RawFd>;
    fn events_buffered(&self) -> bool;
    fn drain_events(&mut self) -> Result<bool>;      // true: rescan now
    fn changes(&self) -> &Changes;                   // pending, not consumed
    fn take_changes(&mut self) -> Changes;
    fn defer_output(&mut self, panes: HashSet<String>);
    fn covered_session(&self) -> Option<&str>;       // None: events cover every session
    fn follow_session(&mut self, session: &str) -> bool; // make events cover it

    // Records
    fn is_live(&mut self, kind: Option<Kind>, ids: Ids) -> bool; // None: all three
    fn name(&mut self, kind: Kind, ids: Ids) -> Result<String>;
    fn rename(&mut self, kind: Kind, ids: Ids, name: &str) -> Result<()>;
    fn delete(&mut self, kind: Kind, ids: Ids) -> Result<()>; // never a last pane, window or session
    fn pane_cwd(&mut self, ids: Ids) -> Result<String>;  // after confirming ids are current
    fn session_cwd(&mut self, session: &str) -> Result<String>;
    fn create(&mut self, new: &New) -> Result<String>; // the new pane

    // Clients
    fn jump(&mut self, pane: &str) -> Result<()>;    // on the client the user last used
    fn show(&mut self, client: &str, pane: &str, stay: bool) -> Result<()>;
    fn focus_sidebar(&mut self, client: &str, expected_window: Option<&str>);
    fn message(&mut self, client: &str, text: &str) -> bool; // false: the sidebar shows it
    fn copy(&mut self, client: Option<&str>, text: &str) -> Result<Option<String>>; // Some: escape to write
    fn reference(&self, kind: Kind, id: &str) -> String; // what `yy` copies

    // Restore
    fn identity(&mut self) -> Option<Identity>;      // tells restarts apart; names the snapshot file
    fn windows(&mut self) -> Result<Vec<WindowMeta>>;
    fn live(&mut self) -> Live;
    fn restore(&mut self, snap: &Snapshot, live: &Live, resume: &Resume);
    fn restore_pane(&mut self, entry: &Closed, live: &Live, resume: &Resume);

    // Settings
    fn options(&mut self) -> Result<BTreeMap<String, String>>; // `@agenmux-<setting>` overrides; empty if none
    fn reload_token(&mut self) -> Option<String>;
    fn publish_reload(&mut self, token: &str) -> Result<()>;

    // Sidebar panes
    fn sidebar_panes(&mut self) -> Vec<SidebarPane>;
    fn visible_windows(&mut self) -> HashSet<String>;
    fn resize_panes(&mut self, cols: &[(String, usize)]) -> bool;
    fn close_panes(&mut self, panes: &[String]);
    fn remember_width(&mut self, cols: usize) -> bool;
    fn connection(&mut self) -> Option<String>;      // this connection's name, for ownership
    fn owner(&mut self) -> Option<Owner>;
    fn claim(&mut self, owner: &Owner, runtime: &Path) -> Result<()>;
    fn release(&mut self, generation: Option<&str>);

    // tmux-only features; every other backend returns None
    fn as_tmux(&mut self) -> Option<&mut Tmux> { None }
}
```

About 45 methods in 7 groups. Each gets a one-line contract in its doc
comment.

### Fit with the Rex POC

Checked method by method against `poc/rex-multiplexer:src/mux/rex.rs`. The
first draft of this plan needed these corrections:

| Draft | Fits Rex as | Why |
| --- | --- | --- |
| `Target::Window(id)` | `(Kind, Ids)` | Rex calls need the session id (`session.set_window_label`, `block.close`) |
| `cwd(target)` | `pane_cwd(ids)`, `session_cwd(session)` | Rex sessions have no directory; the first pane's stands in |
| `client_window(client)` + shared wait | `focus_sidebar(client, window)` | Rex clients don't report their window; Rex waits for the sidebar block to appear instead |
| no jump method | `jump(pane)` | master tests `starts_with('%')` and forks `switch-client`; Rex focuses the block |
| `Live` derived from `panes()` | `live()` | Rex counts sidebar blocks and appends windows, so ids and indexes differ from the scan |

Every other method maps onto a Rex implementation in the POC. Two answer
trivially on Rex but stay, because the answer is real rather than a missing
mechanism: `events_buffered` (false: Rex drains whole lines) and
`covered_session`/`follow_session` (events cover all sessions).

Left out on purpose, and added with the Rex backend: `server()` (a daemon
that outlives its server; tmux's dies with its control connection) and the
POC's Rex-only `embedded`, `notifies`, `quiet` and
`reconcile_sidebar_layout` (several scanners, Lua-owned layout; the shared
daemon makes them moot).

`options()` keeps tmux's `@agenmux-<setting>` key convention: the resolver and
its source labels (`tmux @agenmux-width`) depend on it, and the POC's Rex
backend used the same keys.

Decisions inside it:

- **Changes keep the peek/take split.** The event loop schedules on pending
  changes but consumes them at scan time, and output seen during a scan
  belongs to the next one. Collapsing them would change scan timing.
- **`windows()` returns `WindowMeta`, not `WINDOWS_FMT` text.** The format
  string and its parser move into the tmux backend; `snapshot::layout` takes
  the struct. Its `layout` field is opaque: only the same backend's restore
  reads it.
- **`restore`/`restore_pane` take a precomputed `Resume`** (agent name →
  resume command) instead of a callback. Their tmux command sequences
  (layout strings, placeholder panes) move into `src/mux/tmux.rs` with their
  unit tests; the snapshot model and `closed_between`/`record` stay shared.

## tmux-only, behind the escape hatch

These are tmux mechanisms the Rex branch had no use for. They stay tmux code,
reached explicitly:

- From shared code, through `mux.as_tmux()`: the daemon's key FIFO sink
  (keys tmux delivers itself), reinstalling key bindings when their version
  is stale, switching key tables while the user types in an overlay, and
  inheriting header colors from tmux styles.
- Whole modules that run only from tmux bindings and hooks, through
  `crate::mux::tmux::{command, ...}`: `setup.rs`, `toggle.rs`, `panes.rs`, the
  `send-key`/`click`/`wheel` entry points in `input.rs`, the version-switch
  messages in `release.rs`, and `notification-open` in `notifications.rs`.
  Moving them is churn without a second backend. The Rex ticket decides what
  of their policy (placement in particular) lifts into shared code.

`tests/mux-boundary.sh` (in `tests/run.sh`) keeps the boundary: outside
`src/mux/`, nothing spawns `tmux`, and only the modules above may name
`mux::tmux`.

## Steps

Each lands green (`cargo test --locked`, `tests/run.sh`) with no behaviour
change:

1. Move `src/tmux.rs` to `src/mux/tmux/control.rs`; rename the error and
   change types; fix imports.
2. Trait with scan, focus and change methods; `scan.rs`, `main.rs` and the
   sidebar hold `Box<dyn Multiplexer>`.
3. Records and clients (sidebar management, overlays, jump, `yy`).
4. Restore: identity, `WindowMeta`, replay into the tmux backend.
5. Settings: options and reload token.
6. Sidebar panes (daemon), with its tmux-only steps on `as_tmux()`.
7. Integration modules on `mux::tmux`; `runtime_dir` via `mux`; boundary
   test.
8. `FakeMux` and shared-path tests; `docs/development.md` section on adding
   a backend.

## Testing

Tested once, not per backend:

- Existing tmux suites (`tests/plugin.rs`, `navigation.sh`, `daemon-*.sh`,
  unit tests) are the proof that tmux is unchanged. They are not rewritten.
- `FakeMux` serves canned panes, captures and options and records actions. A
  few tests drive shared code through it: a scan detects agents and skips
  the sidebar's own pane, management actions reach the backend, settings read
  overrides, `Store::open` uses the backend identity. They show shared code
  needs no tmux; a future backend reuses them as they are.
- Per backend only thin translation tests (command built, reply parsed). The
  existing tmux format-parsing tests move with their code.
- Live: a scratch tmux socket (`TMUX=<scratch>,0,0`), a real agent going
  working → idle, and jump, rename, delete, launch, undo and restore in the
  sidebar and daemon.

## Out of scope

- Any non-tmux backend, backend selection, Rex docs.
- Lifting tmux-hook placement into the daemon.
- Faster local release builds (fat LTO dominates; separate issue).

## As built

`src/mux/mod.rs` is the source of truth for the trait. Where it differs from
the sketch above:

- `create` takes `New` by value with borrowed fields; `show` returns the
  pane's window, so the shared code lands the client on that window's sidebar.
- `sidebar_panes` returns `Result`: an unanswered query is not "no sidebars"
  (#238). `close_sidebars` joins the group for teardown, and `claim` takes
  only the client: the activation's generation is set before the daemon
  starts.
- Connectionless functions sit next to the trait for commands that hold no
  connection: `runtime_dir`, `prepare_runtime_dir`, `own_pane`, `options`,
  `publish_reload` and `owner` (the daemon checks its generation before it
  connects).
- The daemon still discards stale tmux replies before measuring, exactly
  where it did, through `as_tmux().sync()`; `owner()` barriers inside the
  backend, as the old owner query did.
- Closed undo entries tell sessions, windows and panes apart by structure (a
  window entry holds its window, a pane entry its pane), not by tmux id
  prefix.
- Focus: the backend reports `focus::Client`s (activity, pane, on its
  sidebar, focused), and `ClientFocus::from_clients` decides who acts and who
  looks. tmux's `list-clients` row format stays in the tmux backend.
- Behind `as_tmux()`: the daemon's key FIFO sink, key tables while typing,
  header colors from tmux styles, docking the sidebars to the other side, and
  the settings overlay reinstalling key bindings.
- Still tmux-specific in shared code: `config reload` reinstalls tmux key
  bindings through `setup.rs`; it holds no connection to ask. The Rex ticket
  decides the binding primitive (Rex binds keys in `init.lua`).
- The FakeMux tests are a scan, live settings, and management. `Store::open`
  is left out: the snapshot path reads `XDG_STATE_HOME`, which parallel tests
  cannot change safely.

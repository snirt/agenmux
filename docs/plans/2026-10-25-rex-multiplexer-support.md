# Full Rex support

## Problem

The `poc/rex-multiplexer` branch adds a `Multiplexer` seam (`src/mux.rs`) with
a Rex backend that implements only `panes`, `capture` and `focus`. Everything
else reaches tmux directly (`command_status`, `crate::tmux::command`,
`mux.run`, `as_tmux`) and fails soft under Rex, so these features silently do
nothing:

- tmux management (`cc`/`cs`/`r`/`dd`), quick launchers, `yy`
- mouse (clicks arrive through tmux bindings; tty mode never enables
  reporting)
- restore after restart (`R`/`x`) and undo (`u`): `snapshot::Store::open`
  needs a `Tmux`
- live settings and `config reload`: `Settings::refresh` reads tmux options
- popup mode, notifications (`notification-open`), the status line
- daemon mode: one scanner, mirrored sidebar panes, width sync. Each Rex
  window runs its own tty sidebar and scanner, placed by `rex-sidebars` or
  an `init.lua` alt+s action
- in-sidebar messages (`client_message` uses `display-message`)

Scanning is also expensive: `Rex::panes` forks `rex` three times per terminal
block per scan, and change detection is polling only.

## Goal

Every feature in `docs/usage.md` works under Rex, or is documented as not
applicable, with the tmux path unchanged.

## Design

### 1. Semantic seam

Replace tmux command strings at call sites with intent-level methods on
`Multiplexer`. The tmux impl moves the existing commands behind them verbatim.

```rust
fn create_window(&mut self, session: &str, name: &str, cwd: &str) -> Result<String>;
fn create_session(&mut self, name: &str, cwd: &str) -> Result<String>;
fn rename(&mut self, target: &Target, name: &str) -> Result<()>;
fn delete(&mut self, target: &Target) -> Result<()>;
fn launch(&mut self, session: &str, cwd: &str, argv: &[String]) -> Result<String>;
fn show(&mut self, client: Option<&str>, pane: &str) -> Result<()>; // jump/show-created
fn message(&mut self, client: Option<&str>, text: &str);
fn copy(&mut self, client: Option<&str>, text: &str) -> Result<()>;
fn server_identity(&mut self) -> Option<(String, PathBuf)>; // snapshot key + path
fn windows(&mut self) -> Result<Vec<WindowMeta>>;           // replaces WINDOWS_FMT parsing
fn options(&mut self) -> Options;                           // tmux @agenmux-*; Rex: empty
fn reference(&self, target: &Target) -> String;             // `yy` text
```

`Target` is `{Session|Window|Pane}(id)`. Rex IDs (`session:…`, `window:…`,
`block:…`) pass through untouched; nothing may assume `$`/`@`/`%` prefixes
(audit `item_reference`, `valid_pane`, `target_is_live`, the rows file).

Rex mapping:

| Intent | Rex call |
| --- | --- |
| create window | `session.new_window` (layout: terminal block, `cwd`) |
| create session | `session.create` |
| rename session / window / pane | `session.set_label` / `session.set_window_label` / `session.set_block_label` |
| delete | `session.destroy` / `session.close_window` / `block.close` (never a sidebar block) |
| launch | `session.new_window` running `argv` in `cwd` |
| show | existing `focus` + `session.select` client action, aimed at a `-C` client |
| message | in-sidebar flash line (Rex has no `display-message`) |
| copy | OSC 52 from the sidebar tty; no paste buffer |
| server identity | `server.status` id + start time; path under `$XDG_STATE_HOME/agenmux/rex/` |

`mux.rs` will hold two backends plus the trait. **Ask before splitting** it
into `mux/{mod,tmux,rex}.rs`. Splitting is likely warranted once the Rex
backend owns management, events and snapshots.

### 2. Cheap scans and change events

- `Rex::panes` makes one `rex do -e <lua>` call that walks `session.view` for
  every session and calls `process`/`title`/`size` in-process, returning one
  JSON document. That replaces 3×N forks with one per scan.
- `capture` keeps one fork per block. Detection only captures agents.
- `notify_fd`: a long-lived `rex events --all --json` child. Its stdout is the
  fd. `drain_notifications` maps session, window and block events to
  `PendingChanges`, so layout changes rescan at once instead of waiting for
  the poll. Restart the child if it exits; fall back to polling meanwhile.
- First, check whether block output is visible as an event. If not, output
  changes stay on the scan schedule (as with tmux's deferred output today).

### 3. Input in tty mode

- Enable SGR mouse reporting (`?1000h ?1006h`) in tty mode; restore on exit.
  Decode presses into the existing `Key::Select`/`Key::Open` (0x05/0x0f)
  protocol so the click model from #201 (single selects, double opens) is
  shared. Double-click timing moves into the sidebar for tty mode. Wheel
  scrolls.
- `q`/`Q` in a Rex sidebar block exits. Confirm Rex closes the block when its
  command ends; otherwise call `block.close` on `REX_BLOCK`.
- `focus_sidebar` maps to `rex focus` on the sidebar block of the window.
- Cursor follows focus: `client_focus` reports the focused block of the
  sidebar's own session's active window (Rex clients do not say which
  session they show). `session_view_changed` events rescan at once, so
  choosing a pane moves the agenmux cursor to it.
- Mutation input mode (tmux key tables) is moot in tty mode: stdin already
  carries every key.

### 4. Settings and reload without tmux options

- Rex has no `@agenmux-*` options. `Settings::refresh` takes the mux and
  reads `options()`; under Rex the file is the only source.
- `config reload` writes the reload token to a state file
  (`$XDG_STATE_HOME/agenmux/reload-token`) when not in tmux. Refresh watches
  it by mtime.
- Settings overlay width change: `session.resize_pane` on the sidebar split.

### 5. Snapshot, restore, undo

- `Store::open(&mut dyn Multiplexer)` via `server_identity()`; `layout()`
  is built from `windows()` instead of the tmux format string.
- Replay (`restore_closed`, `replay`, `replay_pane`) uses `create_session`,
  `create_window` and `launch`. Panes in a window replay as `session.new_split`
  after the first block. Split geometry is best effort; record what Rex's
  layout JSON exposes.
- A Rex server restart is detected by a new server id, like tmux's
  pid|start_time.

### 6. Placement, toggle, popup

- Move the alt+s Lua toggle into Rust: `agenmux toggle` under Rex adds or
  removes the sidebar split in the focused window. The plugin ships a
  `rex/agenmux.lua` that binds the key and calls
  `rex.exec.start({agenmux, "toggle"}, {detach = true})`. The user adds one
  `dofile` line to `init.lua`; agenmux never edits it.
- Width: keep the split ratio in `$XDG_STATE_HOME/agenmux/rex-sidebar-ratio`.
  Each running sidebar saves its ratio when the user changes it (a key or
  mouse resize), once placement has settled; opening one uses the saved
  ratio. The app halves a new split in a window it shows, after the server
  applied the ratio, so `rex_place.lua` creates the split and corrects the
  border from inside one script, before the app draws the half width. This replaces the column-based ratio in
  `add_sidebar`, which reads stale block sizes right after layout changes.
- Auto-placement (one sidebar per window) stays opt-in: alt+s is the Rex
  default. `rex-sidebars` becomes event-driven on the §2 event stream instead
  of a 2s poll.
- Closing matches tmux: a running sidebar closes its window once it is the
  only block left, and destroys a session it saw holding windows once it has
  none (Rex keeps empty sessions, which have no row to show). The session
  goes to the undo log like any close. `dd` keeps refusing a session's last
  window.
- Popup mode: `session.new_layer` floating layer with the sidebar block,
  closed on `q`/`Esc` or after a jump.

### 7. Notifications

`notification-open` gains a Rex form (`rex <server> <block>`). It runs `show`
via the Rex backend instead of `tmux -S`.

### 8. Shared scanner (measure first)

Tty sidebars each scan the server. With N windows that is N scanners and N×
capture load. If §2 leaves this measurable, reuse the daemon: one
`agenmux daemon` per Rex server scans and renders. Sidebar blocks run a thin
frame reader (the `pane_writers` role). Skip if per-sidebar cost is
negligible after §2.

### 9. Not applicable

- Status line: Rex has no status line. `agenmux status` still prints counts
  for scripts. Document it.
- tmux paste buffer for `yy`: OSC 52 only.

## Order

Each step lands green on its own, tmux behaviour unchanged:

1. Seam + `Target` + ID-prefix audit (§1, tmux impl only; no behaviour change)
2. Batched scan + event fd (§2) and live detection verification
3. Tty mouse, jump, `q`, focus (§3)
4. Management, launchers, `yy`, messages on Rex (§1 Rex impl)
5. Settings/reload (§4)
6. Rust toggle, ratio memory, event-driven auto-placement, popup (§6)
7. Snapshot/restore/undo (§5)
8. Notifications (§7)
9. Measure; shared scanner only if needed (§8)
10. Docs: `docs/usage.md` Rex section, README, RELEASE_NOTES

## Verification

- Unit: a fake `rex` on `PATH` (shell script that answers canned, sanitized
  JSON for `ls`, `session.view`, block methods and the batch script, and logs
  argv). It covers the Rex `Multiplexer` methods, ID handling and the
  `events` decoder. tmux tests stay as they are; `cargo test --locked` and
  `tests/run.sh` both pass at every step.
- Detection (AGENTS.md): real agent in a fresh Rex block on a scratch server
  (`REX_SERVER`), capturing title and screen through working → idle.
  Fixtures sanitized; two working variants plus an idle negative.
- Live per step: drive the sidebar in a scratch Rex session with `rex
  send-key` and `rex capture`. Check each `docs/usage.md` key under Rex,
  plus mouse via the app.
- Before live checks, confirm which `agenmux` binary the Rex sidebar block
  runs (the `command` in its layout).

## Open questions

- Does Rex emit block output or title changes as events?
- Does a terminal block close when its command exits, or linger?
- Is there a supported Linux Rex build for CI, or does Rex stay local-only
  verification?
- Should auto-placement (one sidebar per window) be the Rex default, or
  alt+s only?

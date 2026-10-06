# Lua extensions

## Goal

Make new views and integrations (git status, branch and commit pickers, PR
lists, per-pane badges) cheap to add. Today every view is a hand-written
`Overlay` variant plus renderer, key, and setup changes, and quick launchers are
the only extension point. Keep the Rust core responsible for every mechanism and
let small Lua files supply policy: what data to fetch, how to label it, and what
a key does.

## Non-goals

- No Lua in detection, scanning, rendering, tmux transport, pane lifecycle,
  setup, or update/rollback. Those stay Rust and keep working when every plugin
  is broken.
- No third-party plugin ecosystem yet. Plugins ship in this repository; users may
  add personal scripts, but the API is unstable until phase 4.
- No raw drawing from Lua. Plugins describe data; Rust lays it out.

## Architecture

Rust owns mechanisms, Lua owns policy:

| Rust (core) | Lua (plugins) |
| --- | --- |
| tmux control pipe, scan, detection, render loop | which command to run for a pane |
| process spawning, timeouts, output caps, dedupe | how to parse the output |
| badge layout, clipping, colors | badge text and semantic highlight |
| generic list overlay (scroll, select, close) | list items and what selecting one does |
| key tables, sequence dispatch, help | key sequence, label, handler |
| executing requests (open window, send keys) | which request to make |

Plugins never touch tmux or the terminal directly. API calls either record
state (badges, handlers, keys) or queue requests that the sidebar validates and
executes on its own loop.

### Runtime

- `mlua` with vendored Lua 5.4. Only `string`, `table`, `math`, and `utf8` are
  opened; no `io`, `os`, `package`, `debug`, or `require` from disk.
- Every call into Lua runs under a wall-clock budget enforced by an instruction
  hook. A failing or slow handler logs `agenmux ext: <plugin>: <error>` to the
  daemon log and never stops the sidebar.
- Load order: bundled `runtime/plugin/*.lua` (sorted), then
  `$XDG_CONFIG_HOME/agenmux/init.lua`, then
  `$XDG_CONFIG_HOME/agenmux/plugin/*.lua`.
- Hot reload: the sidebar stats its sources on periodic scans and rebuilds the
  VM when one changes. New key sequences additionally need `agenmux setup`
  (toggle reruns it automatically because the key fingerprint changes).

### API v0

```lua
agenmux.on(event, fn)                       -- PaneAdded, PaneRemoved,
                                            -- SelectionChanged, AgentStateChanged
agenmux.keymap.set(seq, fn, { desc = "" })  -- seq: 1-2 printable characters
agenmux.system(argv, opts, fn)              -- async; opts: cwd, timeout_ms,
                                            -- key (dedupe), ttl_ms (cache)
agenmux.ui.badge(pane_id, ns, text, hl)     -- text nil clears; hl: muted, accent,
                                            -- ok, warn, error
agenmux.ui.list{ title, items, on_select }  -- generic picker overlay
agenmux.api.open_window{ pane, cmd }        -- new window at the pane's cwd
agenmux.api.send_keys(pane_id, text)
agenmux.notify(message)
agenmux.log(message)
```

Pane values passed to handlers: `id`, `session`, `session_id`, `window`,
`window_id`, `cwd`, `command`, `title`, and for agent panes `agent` and `state`.

### Efficiency rules

The sidebar runs one process with zero forks per tick. Extensions must not
regress that by accident, so the core enforces:

- No per-tick hook. Plugins react to events; scans fire events only for diffs.
- `system` is always asynchronous, at most 4 jobs run at once (the rest queue),
  each has a timeout (default 2 s, max 10 s) and a 64 KiB output cap.
- Jobs with the same `key` share one process; results with `ttl_ms` are cached,
  so ten panes in one repository run `git status` once.
- Badges render on the next frame; they never force a redraw on their own.
- Only while jobs are in flight does the loop wake every 50 ms to collect them.

### Keys

Plugin keys join `input::sequence_bindings` as `SequenceDispatch::Extension`.
Built-in and launcher sequences win: a plugin sequence that equals or
prefix-conflicts with one is skipped and reported. Because `setup` derives tmux
key tables and the setup fingerprint from the same bindings, plugin keys appear
in help, continuation hints, popup input, and split key tables without extra
wiring. `setup` loads plugins in declare-only mode (no jobs, no events).

### Distribution

Bundled plugins live in `runtime/` beside `agents/` and ship with the checkout
the installer and TPM already clone. Before leaving experimental status, embed
`runtime/` into the binary so the engine and its Lua cannot drift, with
`AGENMUX_RUNTIME=<dir>` for development.

### Enablement

The prototype is opt-in through `AGENMUX_EXTENSIONS=1` in tmux's global
environment, so default behavior is unchanged. Phase 2 replaces this with an
`[extensions] enabled` config key and per-plugin switches.

### Trying the prototype

```sh
tmux set-environment -g AGENMUX_EXTENSIONS 1
agenmux extensions          # sources, bound keys, load errors
```

Reopen the sidebar (toggle reruns setup when plugin keys change). Panes inside
a git repository show a branch badge (`*` dirty, `↑n` ahead). `gc` lists
commits and opens `git show` in a new window; `gb` lists branches and switches
the selected pane's repository. Personal scripts go in
`$XDG_CONFIG_HOME/agenmux/init.lua` or `$XDG_CONFIG_HOME/agenmux/plugin/`.

## Phases

1. **Host and git prototype** (this branch): Lua host with budgets, loading,
   hot reload, events, keymaps, async `system` with dedupe/cache/queue, badges,
   generic list overlay, `open_window`/`send_keys`/`notify`, `agenmux
   extensions` diagnostics, and `runtime/plugin/git.lua` as the reference
   plugin.
2. **Config and health**: `[extensions]` config, per-plugin enable, settings
   rows, `agenmux health` budget/error report, timers (`agenmux.defer`), badges
   in agent-only view.
3. **Building blocks from built-ins**: highlight groups from the theme, text
   input prompt (`ui.input`), move Help and Versions onto the list overlay,
   reimplement quick launchers as a bundled plugin.
4. **Stabilize**: embed `runtime/`, versioned API with `api-info`, docs in
   `docs/extensions.md`, then consider user-shared plugins.

## Risks

- C toolchain in every release target (vendored Lua). Verify cross builds before
  phase 2.
- Binary size: expect a few hundred KiB on top of the size-tuned release build.
- API surface becomes a contract once users depend on it; keep it marked
  unstable and only add functions a bundled plugin needs.
- Plugins that send keys into agent panes can type into prompts; bundled plugins
  must not do so implicitly.

## Verification

1. Unit tests for the host: loading and error isolation, budget enforcement,
   keymap validation, events, badge state, request queue, job dedupe/cache/
   timeout, and sequence conflict filtering.
2. `cargo test --locked` and `tests/run.sh` stay green with extensions disabled.
3. Live check in a private tmux server with `AGENMUX_EXTENSIONS=1`: git badges
   appear on panes inside repositories, `gc` opens the commit list, selecting a
   commit opens `git show` in a new window, and editing `git.lua` hot-reloads.
4. Review the diff and scan changed files for private identifiers or secret-like
   values.

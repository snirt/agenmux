# Rex

agenmux also runs inside [Rex](https://www.superlogical.com/rex/). Rex sessions,
windows and terminal blocks take the place of tmux sessions, windows and panes.
The sidebar is a terminal block at the side of a window.

## Setup

Add one line to `~/.config/rex/init.lua`, with the paths to your agenmux
checkout or install:

```lua
dofile("/path/to/agenmux/rex/agenmux.lua"){ bin = "/path/to/agenmux" }
```

Then run `rex config reload`. Options:

| Option | Default | Meaning |
| --- | --- | --- |
| `bin` | `"agenmux"` | The agenmux binary. Rex actions see a bare `PATH`, so give a full path. |
| `key` | `"alt+s"` | Turn the sidebars on or off; with `display.mode = "popup"`, show the popup. |
| `popup_key` | `"alt+p"` | Open or close the sidebar as a floating popup; `false` binds nothing. Avoid Mac dead keys such as alt+e, alt+i, alt+n and alt+u: macOS keeps them from Rex. |

As in tmux, the key turns agenmux on in the current window, and every
window you move to gets a sidebar too. Pressed again, it closes them all.

agenmux never edits `init.lua`. To keep a sidebar in every window, old ones
included, run `agenmux rex-sidebars` once per Rex server start.

agenmux finds the `rex` CLI through `$AGENMUX_REX`, `$REX_BIN_DIR`, `PATH`,
then the app's own copy.

## What works

Everything in [usage](usage.md) works the same, except:

- **One daemon**, as in tmux: it scans, renders and sends notifications, and
  each sidebar block only shows its frames and passes on your keys. It starts
  with the first sidebar and ends with the last.
- **Width**: `display.sidebar_width`, in columns. Dragging a sidebar's
  border resizes every sidebar and keeps that width, as tmux keeps
  `@agenmux-width`; a split, a close or a window resize puts it back.
  `display.sidebar_position` applies too. Width requests settle against each
  terminal's reported columns, independently of other blocks' fonts and sizes.
  Hidden blocks settle when the app next displays them; unresponsive clients
  park instead of causing repeated layout changes. Rex's minimum pane sizes
  bound the realized width without overwriting the saved preference. Frames
  are projected into each pane's current PTY viewport and repainted on resize,
  so a narrow or short pane cannot wrap rows or scroll the tree.
  Rex's Lua integration owns placement, divider feedback, drag propagation, and
  empty-window cleanup. Rust transports intent and renders measured geometry;
  adopting existing sidebars does not reset their split ratios.
- **Resize reporting**: each sidebar sends actual PTY dimensions to the daemon
  over a private Unix-domain socket at startup and on `SIGWINCH`. Drag bursts
  coalesce; the daemon redraws cached state without waiting for a geometry poll.
  Frames still use per-pane FIFOs, with nonblocking delivery so a slow pane cannot
  stall the others. Readers reconnect after daemon handover and retain local
  clipping while sizes change. Native PTY resizing and in-flight backend work can
  still add latency; the render cadence is not a 60 FPS guarantee.
- **Popup**: `display.popup_width` and a fixed `popup_height` size it in
  cells; `popup_height = "auto"` takes 80% of the window.
- **Focus**: the cursor follows the session the app shows. Rex does not
  report it, so agenmux reads the app's own record of it from its macOS
  preferences when it changes, or uses newer live focus changes while that
  persisted record lags. A popup follows the pane it opened over. Jumps go to
  the app window whose key opened the sidebar.
- **Mouse**: the sidebar block turns on mouse reporting itself. Click and
  wheel work as in tmux. Double-click goes to any row: a session header to
  its current window, a window header to its focused pane. Fold with the
  keys.
- **Messages** (copy confirmations, errors) show in the sidebar's hint line;
  Rex has no message line.
- **`yy`** copies `rex session …`, `rex window …` or `rex block …` through
  OSC 52. There is no paste buffer.
- **Configuration** comes from `config.toml` alone; Rex has no
  `@agenmux-*` options. `agenmux config reload` reaches running sidebars
  through `$XDG_STATE_HOME/agenmux/reload-token`.
- **Rendering-daemon reload**: `agenmux rex-reload` (or the Rex action
  `agenmux_reload`) replaces the renderer without closing sidebar blocks or
  changing existing split ratios. It does not restart the Rex server. Existing
  pane readers reconnect; replacing their executable still requires reopening
  those panes.
- **Closing** works as in tmux. A window whose last pane closes takes its
  sidebar with it, and a session whose last window closes is closed too
  (Rex would keep it, empty). `u` lists it, so it can come back. `dd` still
  refuses a session's last window; delete the session row instead.
- **Restore and undo**: a window comes back at its old place in the tab
  order, with its splits and their ratios. A pane comes back beside its
  nearest open neighbour.
- **Version switching** (`U`) closes every Rex sidebar and popup and reopens
  the sidebars on the new release.
- **Status line**: Rex has none. `agenmux status` still prints the counts.
- **Refresh**: Rex reports layout, focus, title and process changes as events,
  but not terminal output. A screen that changes without its title waits for
  the periodic scan.

## Choosing the server

agenmux talks to the multiplexer it runs in: Rex in a Rex block, tmux in a
tmux pane, the inner one when nested. Stale variables do not mislead it: with
both `TMUX` and `REX_SESSION` set, it checks which one's pane or block this
process actually runs in. Outside both, it uses whichever server is running;
with both running, pass `--mux rex` or `--mux tmux`, or set `AGENMUX_MUX`.

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
| `key` | `"alt+s"` | Toggle the sidebar in the current window; with `display.mode = "popup"`, show the popup. |
| `popup_key` | `"alt+p"` | Open or close the sidebar as a floating popup; `false` binds nothing. Avoid Mac dead keys such as alt+e, alt+i, alt+n and alt+u: macOS keeps them from Rex. |

While a sidebar is open, every new session and window gets one too, as in
tmux. A window whose sidebar you toggled off stays without.

agenmux never edits `init.lua`. To keep a sidebar in every window, old ones
included, run `agenmux rex-sidebars` once per Rex server start.

agenmux finds the `rex` CLI through `$AGENMUX_REX`, `$REX_BIN_DIR`, `PATH`,
then the app's own copy.

## What works

Everything in [usage](usage.md) works the same, except:

- **Width**: the sidebar opens at the width you last resized one to, with
  keys or the mouse, however it was closed. `display.sidebar_width` does not
  apply. `display.sidebar_position` does.
- **Popup**: `display.popup_width` and a fixed `popup_height` size it in
  cells; `popup_height = "auto"` takes 80% of the window.
- **Focus**: the cursor follows the pane focused in the sidebar's own session,
  or in a popup, the pane it opened over. Jumps go to the app window whose
  key opened the sidebar.
- **Notifications** come from one sidebar, whichever others are open. A
  sidebar in a window you are not looking at waits for Rex's events instead
  of polling.
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

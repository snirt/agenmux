# Rex

agenmux also runs inside [Rex](https://www.superlogical.com/rex/). Rex sessions,
windows and terminal blocks take the place of tmux sessions, windows and panes.
The sidebar is a terminal block at the left of a window.

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
| `key` | `"alt+s"` | Toggle the sidebar in the current window. |
| `popup_key` | `"alt+e"` | Show the sidebar as a floating popup; `false` binds nothing. |

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
  apply.
- **Focus**: the cursor follows the pane focused in the sidebar's own session.
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
- **Restore and undo**: windows and panes come back side by side. Rex takes no
  saved layout, and a restored window is appended rather than put back at its
  old index.
- **Status line**: Rex has none. `agenmux status` still prints the counts.
- **Refresh**: Rex reports layout, focus, title and process changes as events,
  but not terminal output. A screen that changes without its title waits for
  the periodic scan.

## Choosing the server

agenmux talks to Rex when it runs in a Rex block outside tmux (`REX_SESSION`
set, `TMUX` unset). `AGENMUX_MUX=rex` or `AGENMUX_MUX=tmux` forces either one.

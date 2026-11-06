# Rex

agenmux also runs inside [Rex](https://www.superlogical.com/rex/). Support is in
beta, and on macOS only: Rex has no Linux client yet. Rex sessions,
windows and terminal blocks take the place of tmux sessions, windows and panes.
The sidebar is a terminal block at the side of a window.

## Setup

You need Rex on macOS. The [installer](installation.md#installer) does this for
you. By hand, add one line to `~/.config/rex/init.lua` (or `$REX_CONFIG`), with
the full paths to your agenmux checkout or install:

```lua
dofile("/path/to/agenmux/rex/agenmux.lua"){ bin = "/path/to/agenmux/target/release/agenmux" }
```

The installer writes the same line with its keys spelled out, for example
`{ bin = "…", key = "alt+s", popup_key = "alt+p" }`. Then run
`rex config reload`. Options:

| Option | Default | Meaning |
| --- | --- | --- |
| `bin` | `"agenmux"` | The agenmux binary. Rex actions see a bare `PATH`, so give a full path. |
| `key` | `"alt+s"` | Turn the sidebars on or off; with `display.mode = "popup"`, show the popup. |
| `popup_key` | `"alt+p"` | Open or close the sidebar as a floating popup; `false` binds nothing. Avoid Mac dead keys such as alt+e, alt+i, alt+n and alt+u: macOS keeps them from Rex. |

As in tmux, the key turns agenmux on in the current window, and every window
you move to gets a sidebar too. Pressed again, it closes them all. To have
sidebars from the start, run `agenmux rex-sidebars` once per Rex server start;
it returns when the server stops.

agenmux itself never edits `init.lua`; the installer adds or removes its line
when you ask. It finds the `rex` CLI through `$AGENMUX_REX`,
`$REX_BIN_DIR`, `PATH`, then the app's own copy.

## What works

Everything in [usage](usage.md) works the same, except:

- **One daemon**, as in tmux: it scans, renders and sends notifications, and
  each sidebar block only shows its frames and passes on your keys and mouse.
  It starts with the first sidebar and ends with the last.
- **Agent state**: an agent that reports its own state to Rex shows that
  state; Claude Code, Codex, OpenCode and Pi report. A question or permission
  prompt on screen still shows as blocked, since Pi reports working while one
  waits. Oh My Pi and Hermes report nothing and are read from the screen and
  title, as in tmux.
- **Width**: `display.sidebar_width`, in columns. Rex sizes splits by ratio, so
  the daemon moves each divider until the block reports that many columns.
  Dragging a sidebar's border resizes every sidebar and keeps that width, as
  tmux keeps `@agenmux-width`. `display.sidebar_position` applies too.
- **Popup**: `display.popup_width` and a fixed `popup_height` size it in
  cells; `popup_height = "auto"` takes 80% of the window.
- **Focus**: the cursor follows the session the app shows. Rex does not report
  it, so agenmux reads the app's own record of it from its macOS preferences,
  and focus changes Rex reports win while that record lags. A popup follows the
  block it opened over.
- **Mouse**: click selects a row, double-click opens it, the wheel scrolls.
- **Messages** (copy confirmations, errors) show in the sidebar's hint line;
  Rex has no message line.
- **`yy`** copies `rex session …`, `rex window …` or `rex block …` through
  OSC 52. There is no paste buffer.
- **Configuration** comes from `config.toml` alone; Rex has no `@agenmux-*`
  options. `agenmux config reload` reaches running sidebars through
  `$XDG_STATE_HOME/agenmux/reload-token`. Rex keys live in `init.lua`.
- **Daemon reload**: `agenmux rex-reload` (or the Rex action `agenmux_reload`)
  replaces the daemon without closing sidebar blocks or moving dividers.
- **Closing** works as in tmux. A window whose last pane closes takes its
  sidebar with it, and a session whose last window closes is closed too (Rex
  would keep it, empty). `u` lists it, so it can come back. `dd` still refuses
  a session's last window; delete the session row instead.
- **Restore and undo**: a window comes back at its old place in the tab order,
  with its splits and their ratios. A pane comes back beside its nearest open
  neighbour.
- **Version switching** (`U`) closes every Rex sidebar and popup and reopens
  the sidebars on the new release.
- **Notifications** work as in tmux. Clicking one brings Rex forward and
  focuses the agent's block. Turn them off with `notifications = false` under
  `[behavior]` in `config.toml`; there is no `@agenmux-notifications`.
- **Quick launchers**: `working_directory = "tmux"` means the directory of
  the session's first block, or your home directory.
- **Status line**: Rex has none. `agenmux status` prints plain counts.
- **Refresh**: Rex reports layout, focus, title and process changes as events,
  but not terminal output. A screen that changes without its title waits for
  the periodic scan.

## Choosing the server

agenmux talks to the multiplexer it runs in: Rex in a Rex block, tmux in a
tmux pane, the inner one when nested. Stale variables do not mislead it: with
both `TMUX` and `REX_SESSION` set, it checks which one's pane or block this
process actually runs in. Outside both, it uses whichever server is running;
with both running, pass `--mux rex` or `--mux tmux`, or set `AGENMUX_MUX`.
`REX_SERVER` picks the Rex server, as for the `rex` CLI; sidebar blocks carry
it, and each server gets its own runtime directory.

## Removing

Run the installer again and uncheck Rex: it removes the `init.lua` line after
asking, reloads Rex and closes the open sidebars. By hand, close the sidebars
with the toggle key, delete the line, and run `rex config reload`.

## Troubleshooting

The daemon log is the same as in tmux, `$XDG_STATE_HOME/agenmux/daemon.log`;
see [Troubleshooting](troubleshooting.md). Rex has no `@agenmux-debug`: to
trace, run `AGENMUX_DEBUG=/tmp/agenmux-trace.log agenmux rex-reload` from a
Rex block, which replaces the daemon with one that writes the trace. Run
`agenmux rex-reload` again without the variable to stop.

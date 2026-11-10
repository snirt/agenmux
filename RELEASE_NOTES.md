# agenmux v0.9.0

## What's changed

v0.9.0 adds [Rex](https://www.superlogical.com/rex/) as a second multiplexer, in beta and on macOS only. The installer now installs agenmux once for every multiplexer.

### Rex (beta)

- agenmux runs inside Rex: Rex sessions, windows and terminal blocks stand in for tmux sessions, windows and panes. The sidebar, popup, search, session management, notifications, restore and undo work as in tmux, driven by the same daemon ([#244](https://github.com/snirt/agenmux/issues/244)).
- `alt+s` toggles the sidebars and `alt+p` the popup; both are set in the `init.lua` line that loads agenmux. `agenmux rex-sidebars` keeps a sidebar in every window, and `agenmux rex-reload` replaces the daemon without touching the layout.
- Commands use the multiplexer they run in. Outside both, with both servers running, pass `--mux tmux` or `--mux rex`, or set `AGENMUX_MUX`.
- Beta limitations: Rex has no Linux client or status line. Screen-only changes wait for periodic scans; active-window tracking follows Rex's terminal-size reports. Clipboard copying uses OSC 52 rather than a tmux paste buffer.
- See [docs/rex.md](https://github.com/snirt/agenmux/blob/master/docs/rex.md) for setup and what differs from tmux.

### Installer

- New installs go to `~/.local/share/agenmux` (under `$XDG_DATA_HOME` when set), which tmux and Rex both load. Existing installs in `~/.tmux/plugins/agenmux` keep updating in place.
- On a terminal, a checklist shows tmux and Rex as integrated now. Checking one adds its config lines after asking; unchecking one removes them, unbinds its keys and closes its sidebars. `AGENMUX_MUX=tmux`, `rex` or `both` skips the checklist.
- tmux is loaded through a `run-shell` line, also with TPM, so TPM does not clone a second, tmux-only copy. Rex alone needs no tmux.

### Fixes

- New-window prompts use window colors and icons; new-session prompts keep session styling in both views and glyph modes ([#252](https://github.com/snirt/agenmux/pull/252)).

### Assets

- Linux x86_64
- Linux aarch64
- macOS x86_64
- macOS aarch64
- SHA-256 checksums

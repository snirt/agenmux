# agenmux v0.9.0

## What's changed

v0.9.0 adds [Rex](https://www.superlogical.com/rex/) as a second multiplexer, in beta and on macOS only. The installer now installs agenmux once for every multiplexer. This release also applies auto-updates to an open sidebar, keeps sidebars alive through a stalled tmux server, and fixes clicks in an unframed sidebar.

### Rex (beta)

- agenmux runs inside Rex: Rex sessions, windows and terminal blocks stand in for tmux sessions, windows and panes. The sidebar, popup, search, session management, notifications, restore and undo work as in tmux, driven by the same daemon ([#244](https://github.com/snirt/agenmux/issues/244)).
- `alt+s` toggles the sidebars and `alt+p` the popup; both are set in the `init.lua` line that loads agenmux. `agenmux rex-sidebars` keeps a sidebar in every window, and `agenmux rex-reload` replaces the daemon without touching the layout.
- Commands use the multiplexer they run in. Outside both, with both servers running, pass `--mux tmux` or `--mux rex`, or set `AGENMUX_MUX`.
- See [docs/rex.md](https://github.com/snirt/agenmux/blob/master/docs/rex.md) for setup and what differs from tmux.

### Installer

- New installs go to `~/.local/share/agenmux` (under `$XDG_DATA_HOME` when set), which tmux and Rex both load. Existing installs in `~/.tmux/plugins/agenmux` keep updating in place.
- On a terminal, a checklist shows tmux and Rex as integrated now. Checking one adds its config lines after asking; unchecking one removes them, unbinds its keys and closes its sidebars. `AGENMUX_MUX=tmux`, `rex` or `both` skips the checklist.
- tmux is loaded through a `run-shell` line, also with TPM, so TPM does not clone a second, tmux-only copy. Rex alone needs no tmux.

### Sidebar

- A tmux server that stops answering for a few seconds (laptop sleep, heavy load) no longer makes the daemon exit and leave frozen sidebars ([#238](https://github.com/snirt/agenmux/pull/238)).
- With `show_frame = false`, a click now selects the row under the cursor instead of the row below it ([#242](https://github.com/snirt/agenmux/pull/242)).
- A fixed popup width or height is clamped to the client that opened it, not to a larger client attached elsewhere, so the popup opens instead of failing with "height too large" ([#242](https://github.com/snirt/agenmux/pull/242), [#162](https://github.com/snirt/agenmux/issues/162)).
- The scan cache is written atomically, so `#{agenmux}` and popup sizing never read it half-written ([#242](https://github.com/snirt/agenmux/pull/242)).

### Auto-update

- An open tmux sidebar view no longer waits for a fresh start: once a prepared release is ready, it closes and reopens itself on it. An open popup, or a view in Rex, still switches on its next launch ([#246](https://github.com/snirt/agenmux/pull/246), [#245](https://github.com/snirt/agenmux/issues/245)).

### Assets

- Linux x86_64
- Linux aarch64
- macOS x86_64
- macOS aarch64
- SHA-256 checksums

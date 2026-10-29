# agenmux v0.8.6

## What's changed

v0.8.6 cleans up after a daemon that died without tearing down, and stops a stacked sidebar from resizing every other sidebar to the full window width.

### Daemon

- When the daemon dies without running its own teardown, `agenmux status` and `pane-add` now detect the missing control client and clean up: frozen sidebars are killed, layouts restored and ownership options cleared ([#235](https://github.com/snirt/agenmux/pull/235), [#234](https://github.com/snirt/agenmux/issues/234)).
- Control-mode clients, such as the attach `status` falls back to when no daemon is running, no longer trigger `pane-add` and open a sidebar no one sees ([#235](https://github.com/snirt/agenmux/pull/235)).

### Sidebar

- A layout change that stacks the sidebar above or below the other panes (`select-layout even-vertical`, `move-pane -v`) is no longer read as a border drag. Before, it set the sidebar width to the full window width, and the real panes in every window shrank to one column ([#229](https://github.com/snirt/agenmux/pull/229), [#228](https://github.com/snirt/agenmux/issues/228)).

### Assets

- Linux x86_64
- Linux aarch64
- macOS x86_64
- macOS aarch64
- SHA-256 checksums

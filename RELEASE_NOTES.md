# agenmux v0.8.2

## What's changed

v0.8.2 lets the split sidebar dock on the right edge of the window.

### Right-hand sidebar

- `display.sidebar_position = "right"` (or `set -g @agenmux-sidebar-position right`) opens the split sidebar on the right edge; `left` stays the default ([#199](https://github.com/snirt/agenmux/pull/199), [#198](https://github.com/snirt/agenmux/issues/198)).
- Changing the position from the settings overlay, `agenmux config reload` or the tmux option moves open sidebars to the new edge at the same width, without switching windows ([#199](https://github.com/snirt/agenmux/pull/199)).
- Beside a right-hand sidebar, `h` jumps toward the panes and `l` folds a branch, unless `keys.normal.jump` is configured ([#199](https://github.com/snirt/agenmux/pull/199)).
- Restoring a previous tmux server's layout and cleaning up restored ghost panes follow the sidebar's side ([#199](https://github.com/snirt/agenmux/pull/199)).

### Assets

- Linux x86_64
- Linux aarch64
- macOS x86_64
- macOS aarch64
- SHA-256 checksums

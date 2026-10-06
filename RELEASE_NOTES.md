# agenmux v0.7.2

## What's changed

v0.7.2 stops agenmux from starting a stray tmux server when `$TMUX` points at
a dead socket, keeps two tmux servers from sharing runtime files, and shows a
play button on the README video.

### Fixes

- `agenmux list` and `scan` with `$TMUX` pointing at a dead socket now exit 1 instead of starting a new tmux server ([#164](https://github.com/snirt/agenmux/pull/164), [#135](https://github.com/snirt/agenmux/issues/135)).
- Runtime files live in a private directory per tmux server, so two servers sharing `TMPDIR` no longer take each other's sidebar keys; `AGENMUX_RUNTIME_DIR` still overrides it ([#164](https://github.com/snirt/agenmux/pull/164), [#161](https://github.com/snirt/agenmux/issues/161)).
- Returning to a window or session whose sidebar is selected restores the sidebar's key table ([#164](https://github.com/snirt/agenmux/pull/164)).
- The sidebar waits up to 15 seconds instead of 5 for its first scan, so large tmux servers start reliably ([#164](https://github.com/snirt/agenmux/pull/164)).

### Documentation

- The README video thumbnail shows a play button ([#172](https://github.com/snirt/agenmux/pull/172)).

### Assets

- Linux x86_64
- Linux aarch64
- macOS x86_64
- macOS aarch64
- SHA-256 checksums

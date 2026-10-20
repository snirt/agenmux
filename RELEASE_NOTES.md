# agenmux v0.8.5

## What's changed

v0.8.5 keeps the sidebar responsive when tmux stalls or a pane stops reading, stops config and regex errors from drawing over the live popup, and gives every overlay the same header.

### Responsiveness

- A tmux reply now waits at most 2 seconds. A stalled server costs one short pause instead of freezing the sidebar, and a late reply can no longer be matched to the wrong command ([#225](https://github.com/snirt/agenmux/pull/225), [#208](https://github.com/snirt/agenmux/issues/208)).
- Writes to sidebar panes no longer block. A pane that stops reading is dropped and redrawn in full on the next refresh ([#225](https://github.com/snirt/agenmux/pull/225)).
- The rows file is written only when its contents change, instead of on every animation frame ([#225](https://github.com/snirt/agenmux/pull/225)).
- Fixed a race that could make the daemon quit as if another one had replaced it ([#225](https://github.com/snirt/agenmux/pull/225)).

### Errors

- Config refresh errors and invalid agent regexes no longer print over the live popup. While the sidebar is open they go to the daemon log and the `AGENMUX_DEBUG` trace; the tmux notice for an invalid live config is unchanged ([#223](https://github.com/snirt/agenmux/pull/223), [#209](https://github.com/snirt/agenmux/issues/209)).

### Overlays

- Help, versions, recently closed and settings share one top bar, so the settings header now uses the same color as the others ([#224](https://github.com/snirt/agenmux/pull/224), [#211](https://github.com/snirt/agenmux/issues/211)).

### Assets

- Linux x86_64
- Linux aarch64
- macOS x86_64
- macOS aarch64
- SHA-256 checksums

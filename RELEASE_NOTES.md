# agenmux v0.4.0

## What's changed

### Application configuration

- Added optional XDG configuration for display, behavior, themes, and key bindings ([#61](https://github.com/snirt/agenmux/pull/61)).
- Added dark, light, and terminal themes with per-color overrides across split and popup views.
- Added strict validation, effective-value reporting, and live `config reload` without reopening sidebars.

### Navigation

- Added `gg` / `G` shortcuts to select the first or last visible agent ([#60](https://github.com/snirt/agenmux/pull/60)).
- Improved mouse navigation with click-to-select, second-click-to-open, independent wheel scrolling, and a scrollbar ([#59](https://github.com/snirt/agenmux/pull/59)).

### Fixes

- Preserved normal tmux mouse behavior when clicking outside agent rows and over sidebar overlays ([#50](https://github.com/snirt/agenmux/pull/50)).
- Recovered cleanly when a previously selected development binary no longer exists ([#52](https://github.com/snirt/agenmux/pull/52)).
- Updated Pi detection for current working indicators and interactive question prompts ([#53](https://github.com/snirt/agenmux/pull/53), [#56](https://github.com/snirt/agenmux/pull/56)).
- Prevented idle macOS notifications from causing sustained broker and `usernotificationd` CPU usage ([#62](https://github.com/snirt/agenmux/pull/62)).

### Maintenance

- Improved navigation test synchronization and isolated test tmux servers ([#57](https://github.com/snirt/agenmux/pull/57)).

### Assets

- Linux x86_64
- Linux aarch64
- macOS x86_64
- macOS aarch64
- SHA-256 checksums

**Full changelog:** <https://github.com/snirt/agenmux/compare/v0.3.1...v0.4.0>

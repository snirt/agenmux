# agenmux v0.6.2

## What's changed

v0.6.2 includes all changes from v0.6.1.

### Interface

- Added agent icons, Neovim/lazygit pane icons, and per-key agent configuration overrides ([#130](https://github.com/snirt/agenmux/pull/130)).
- Improved Settings controls and text editing, including cursor movement and paste ([#123](https://github.com/snirt/agenmux/pull/123), [#129](https://github.com/snirt/agenmux/pull/129)).
- Fixed sidebar mouse-row selection and made Escape close the version picker ([#120](https://github.com/snirt/agenmux/pull/120), [#122](https://github.com/snirt/agenmux/pull/122)).

### Reliability

- Prevented macOS sidebar deadlocks on large frames and kept sidebar key bindings after repeatable pane-selection keys ([#137](https://github.com/snirt/agenmux/pull/137), [#131](https://github.com/snirt/agenmux/pull/131)).
- Kept foreground applications from inheriting a nested agent identity ([#128](https://github.com/snirt/agenmux/pull/128)).
- Preserved symlinked tmux configuration during installation and hardened installer checks ([#136](https://github.com/snirt/agenmux/pull/136)).

### Release preparation

- Added patch/minor bump commands with early release checks and automatic local test suites ([#139](https://github.com/snirt/agenmux/pull/139)).
- Made timing-sensitive CI tests wait for observed state ([#124](https://github.com/snirt/agenmux/pull/124)).

### Assets

- Linux x86_64
- Linux aarch64
- macOS x86_64
- macOS aarch64
- SHA-256 checksums

**Full changelog:** <https://github.com/snirt/agenmux/compare/v0.6.1...v0.6.2>

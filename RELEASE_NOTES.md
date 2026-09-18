# agenmux v0.6.0

## What's changed

v0.6.0 includes all changes from v0.5.1.

### Tmux management

- Added configurable quick launchers: with tmux management enabled, `e` opens nvim and `og` opens lazygit in a new window at the selected pane's working directory, `o` shows the optional launcher sequences, and both built-ins and custom launchers (command, args, and working directory) are configurable under `[quick_launchers.*]` ([#107](https://github.com/snirt/agenmux/pull/107)).

### Sidebar

- Replaced the `f` state cycle (`all → blocked → working → idle → done`) with a **User attention** toggle that shows done, working, and blocked agents while hiding idle ones ([#106](https://github.com/snirt/agenmux/pull/106)).
- Fixed stale split-sidebar geometry after a tmux pane resize by refreshing layout geometry before every scan ([#105](https://github.com/snirt/agenmux/pull/105)).

### Installation

- Added `make dev-docker` for running a checkout with a mounted tmux config in Docker, with `AGENMUX_SKIP_UPDATE` and `AGENMUX_FORCE_WIZARD` installer flags for driving the wizard non-interactively ([#102](https://github.com/snirt/agenmux/pull/102)).
- Fixed Docker dev startup parity and copied Docker configs into the disposable dev container instead of relying on a bind mount ([#108](https://github.com/snirt/agenmux/pull/108), [#111](https://github.com/snirt/agenmux/pull/111)).

### Assets

- Linux x86_64
- Linux aarch64
- macOS x86_64
- macOS aarch64
- SHA-256 checksums

**Full changelog:** <https://github.com/snirt/agenmux/compare/v0.5.1...v0.6.0>

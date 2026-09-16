# agenmux v0.5.1

## What's changed

### Tmux management

- Added opt-in tmux management from the sidebar: `cc` creates a window, `cs` creates a session, `dd` deletes the selected record, and `r` renames it in place ([#82](https://github.com/snirt/agenmux/pull/82)).
- Session and multi-pane window rows become selectable with management on, mutation prompts render inline on the cursor row, and every operation revalidates tmux IDs before acting.

### Sidebar

- Added an optional all-panes mode that renders the complete tmux session, window, and pane hierarchy alongside agents, toggled live with `.` ([#70](https://github.com/snirt/agenmux/pull/70), [#82](https://github.com/snirt/agenmux/pull/82)).
- Added an in-app Settings view (`s`) with search, dropdowns, mouse support, and persistence to the configuration file ([#84](https://github.com/snirt/agenmux/pull/84)).
- Inherited header colors from tmux and reused the shared top bar across sidebar views ([#84](https://github.com/snirt/agenmux/pull/84)).
- Made the cursor follow window switches immediately and kept held `j`/`k` responsive under scans ([#87](https://github.com/snirt/agenmux/pull/87)).
- Hardened close/reopen ownership with lifecycle generations, loaded only the focused sidebar at startup, and created hidden-window sidebars lazily ([#99](https://github.com/snirt/agenmux/pull/99)).
- Kept held navigation and wheel bursts responsive with ordered foreground delivery and bounded coalescing ([#99](https://github.com/snirt/agenmux/pull/99)).
- Restored the sidebar width after a neighbouring pane is killed ([#88](https://github.com/snirt/agenmux/pull/88)).
- Kept the sidebar scrollbar continuous on full-width rows ([#66](https://github.com/snirt/agenmux/pull/66)).

### Installation

- Added a one-line installer that clones or updates the plugin, writes the tmux.conf line after confirmation, and reloads tmux ([#92](https://github.com/snirt/agenmux/pull/92)):

  ```sh
  curl -fsSL https://snirt.github.io/agenmux/install.sh | sh
  ```

- Added Docker and Podman test, installer, and interactive harnesses with sanitized optional tmux configuration and Nerd Font guidance ([#99](https://github.com/snirt/agenmux/pull/99)).
- Reduced the release binary size with a size-optimized profile ([#67](https://github.com/snirt/agenmux/pull/67)).

### Detection

- Read the Claude Code 2.1 activity line as working, covering the `✳` glyph and hook progress in the detail, so panes no longer flip to idle mid-task ([#93](https://github.com/snirt/agenmux/pull/93)).

### Performance and diagnostics

- Triggered bounded scans from pane output instead of polling, reducing redundant pane captures ([#77](https://github.com/snirt/agenmux/pull/77)).
- Yielded cooperatively between pane captures when input is waiting and added one-window/many-window latency reporting ([#99](https://github.com/snirt/agenmux/pull/99)).
- Cut the sidebar click helper from seven tmux forks to four ([#87](https://github.com/snirt/agenmux/pull/87)).
- Kept daemon stderr in `$XDG_STATE_HOME/agenmux/daemon.log` with one rotated generation, and extended the opt-in `@agenmux-debug` trace with timestamps, scan numbers, and state changes ([#86](https://github.com/snirt/agenmux/pull/86)).

### Fixes

- Loaded the focused view during sidebar startup ([#71](https://github.com/snirt/agenmux/pull/71)).
- Kept the daemon alive when a pane capture file cannot be read ([#79](https://github.com/snirt/agenmux/pull/79)).
- Fixed a pane-add lock race on macOS and stopped the daemon cleanly on teardown ([#89](https://github.com/snirt/agenmux/pull/89)).

### Maintenance

- Refactored the sidebar into focused input, filtering, rendering, overlay, and daemon components ([#68](https://github.com/snirt/agenmux/pull/68)).
- Cached cargo output in CI, cancelled superseded PR runs, and stabilised the flaky plugin tests ([#79](https://github.com/snirt/agenmux/pull/79), [#89](https://github.com/snirt/agenmux/pull/89)).
- Replaced the Nix sanity job with a pinned OCI build and real-tmux test harness ([#99](https://github.com/snirt/agenmux/pull/99)).
- Baked the latest release into the landing page at deploy time ([#78](https://github.com/snirt/agenmux/pull/78)).

### Assets

- Linux x86_64
- Linux aarch64
- macOS x86_64
- macOS aarch64
- SHA-256 checksums

**Full changelog:** <https://github.com/snirt/agenmux/compare/v0.4.0...v0.5.1>

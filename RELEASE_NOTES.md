# agenmux v0.8.3

## What's changed

v0.8.3 separates selecting a sidebar row from jumping to its pane: a click selects, a double click opens.

### Mouse

- A single click selects a record and keeps focus in the sidebar, even when the record is already selected; repeated clicks no longer switch panes ([#202](https://github.com/snirt/agenmux/pull/202), [#201](https://github.com/snirt/agenmux/issues/201)).
- A double click on a record jumps to its pane, and a double click on a session or window header folds or unfolds it. Quick clicks on two different records only select ([#202](https://github.com/snirt/agenmux/pull/202)).
- Settings, versions (`U`) and recently closed (`u`) follow the same rule: a click selects, a double click does what Enter does. Versions and recently closed now respond to the mouse, and clicking the recently closed search line refines the search ([#202](https://github.com/snirt/agenmux/pull/202)).
- A double click in the sidebar no longer drops it into tmux copy mode ([#202](https://github.com/snirt/agenmux/pull/202)).

### Assets

- Linux x86_64
- Linux aarch64
- macOS x86_64
- macOS aarch64
- SHA-256 checksums

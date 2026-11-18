# agenmux v0.9.1

## What's changed

- `agenmux list` and `scan` show agent panes by default. The `--session` and `--command` filters now narrow those results. If a script used either filter to include non-agent panes, add `--all` to keep those rows ([#262](https://github.com/snirt/agenmux/pull/262)).
- Fixed the `cs` shortcut in Rex so it opens the new-session prompt instead of Settings. Typed keys now continue a pending shortcut sequence before triggering individual key bindings ([#260](https://github.com/snirt/agenmux/pull/260)).
- Sped up `agenmux list` on servers with many Pi panes. Pi subject lookups scan a bounded set of recent session files, and `list` reuses cached subjects when the daemon is running ([#258](https://github.com/snirt/agenmux/pull/258)).
- Routed shared runtime code through the common multiplexer interface and kept tmux-specific commands inside the tmux backend ([#259](https://github.com/snirt/agenmux/pull/259)).

### Assets

- Linux x86_64
- Linux aarch64
- macOS x86_64
- macOS aarch64
- SHA-256 checksums

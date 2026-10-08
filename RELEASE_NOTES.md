# agenmux v0.8.0

## What's changed

v0.8.0 adds undo for closed sessions, windows and panes, multi-selection,
copying tmux references for agent prompts, restoring the sidebar layout after a
tmux restart, and automatic updates.

### Undo recently closed

- `u` opens a **Recently closed** list of sessions, windows and panes, grouped by day. `Enter` restores the selected entry, so `u` `Enter` undoes the last close; `d` forgets one ([#183](https://github.com/snirt/agenmux/pull/183), [#191](https://github.com/snirt/agenmux/pull/191)).
- Sessions and windows come back with their saved layout and a shell in each saved directory; a closed pane splits back into its window next to its old neighbour ([#183](https://github.com/snirt/agenmux/pull/183)).
- `tmux_management.undo_history` sets how many closes are kept (default 20, `0` turns logging off) ([#183](https://github.com/snirt/agenmux/pull/183)).

### Multi-selection

- `v` toggles the focused session, window or pane; `V` starts a range that `j`/`k`, `gg`/`G` or a click extends. `Esc` clears the selection ([#184](https://github.com/snirt/agenmux/pull/184)).
- `yy` copies every selected reference and `dd` deletes every selected record behind one confirmation ([#184](https://github.com/snirt/agenmux/pull/184)).

### Copy tmux references

- `yy` copies the selected row's tmux reference (`tmux session $3`, `tmux window @7`, `tmux pane %12`) to the tmux paste buffer and, over OSC 52, to the terminal clipboard, ready to paste into an agent prompt ([#177](https://github.com/snirt/agenmux/pull/177)).

### Restore after a tmux restart

- Collapsed sessions and windows are remembered per tmux server and survive closing the sidebar and restarting tmux ([#180](https://github.com/snirt/agenmux/pull/180)).
- After a tmux restart the footer offers the previous layout: `R` recreates missing sessions and windows with their layout and directories, `x` dismisses the offer ([#180](https://github.com/snirt/agenmux/pull/180)).
- With `tmux_management.resume_agents = true`, restored agent panes resume their last conversation (`claude --continue`, `codex resume --last`, …); custom agents opt in with `AGENT_RESUME` ([#180](https://github.com/snirt/agenmux/pull/180)).

### Automatic updates

- Auto-update is on by default. At most once a day agenmux downloads and verifies the newest stable release in the background and switches to it on the next fresh start; a release that fails to start is rolled back and not retried ([#185](https://github.com/snirt/agenmux/pull/185)).
- The version picker (`U`) has an `auto-update on|off` row; `behavior.auto_update` sets it in config ([#185](https://github.com/snirt/agenmux/pull/185), [#189](https://github.com/snirt/agenmux/pull/189)).
- Development checkouts, branch checkouts, modified installs and custom engines are never updated; the version picker shows why ([#185](https://github.com/snirt/agenmux/pull/185)).
- `install.sh` installs the latest stable tag; `AGENMUX_REF=<branch|tag>` keeps a development ref ([#185](https://github.com/snirt/agenmux/pull/185)).

### Fixes

- Sidebar messages no longer drop `%` and `#`, so error messages and copied pane IDs show in full ([#177](https://github.com/snirt/agenmux/pull/177)).
- `agenmux key` reports why a key could not be delivered instead of exiting silently ([#187](https://github.com/snirt/agenmux/pull/187)).

### Documentation

- The README plays the intro video inline, right after the description ([#175](https://github.com/snirt/agenmux/pull/175), [#190](https://github.com/snirt/agenmux/pull/190)).

### Upgrade notes

- The version picker moved from `u` to `U`; `u` now opens the recently closed list. A configured `[keys.normal] versions` chord still takes precedence.
- `v`, `V`, `R` and `x` are now fixed keys in the sidebar; a configured chord or launcher prefix on them takes precedence.
- Auto-update is on by default. To turn it off, toggle it in the version picker or add:

  ```toml
  [behavior]
  auto_update = false
  ```

### Assets

- Linux x86_64
- Linux aarch64
- macOS x86_64
- macOS aarch64
- SHA-256 checksums

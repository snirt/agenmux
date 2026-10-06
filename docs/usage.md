# Usage

Press `prefix + A` to open the sidebar, or to move into it when it is already
open. It lists every tmux session, window, and pane, with agent state inline.

## Keys

| Input | Action |
| --- | --- |
| `j` / `k`, `↑` / `↓` | Move selection |
| `gg` / `G` | Move to the first / last visible agent |
| `Enter` / `l` | Jump to the selected agent or pane; on a collapsed header, expand it |
| `Space` | Collapse or expand the selected header |
| `h` / `←`, `→` | Collapse / expand (`h` on a pane steps to its header) |
| `z` / `Z` | Collapse / expand everything |
| `/` | Search |
| `f` | Toggle **User attention**: show done, working, and blocked; hide idle |
| `.` | Toggle between all panes and agents only |
| `Esc` | Leave search and clear filters |
| `cc` / `cs` | Create a window / session ([tmux management](#tmux-management)) |
| `r` | Rename the selected record |
| `dd` | Delete the selected record (asks first) |
| `oe` / `og` | Open nvim / lazygit in a new window ([quick launchers](#quick-launchers)) |
| `s` | Settings |
| `u` | Version picker |
| `?` | Help |
| `q` / `Q` | Close the sidebar |

Movement, jump, search, filter, reset, help, versions, settings, and close can be
rebound; see [Configuration › Keys](configuration.md#keys).

## Views

- **All panes** (default): the full tmux tree.
- **Agents only**: just the detected agents. Press `.` to switch live, or set
  `display.show_all_panes = false` to make it the default.

The view changes only what the sidebar shows. `agenmux scan`, the status line,
and notifications always count agents only.

For the agent-only, read-only sidebar from earlier releases:

```toml
[display]
show_all_panes = false
[tmux_management]
enabled = false
```

## The tree

| Row | Looks like |
| --- | --- |
| Session | Header, collapsible |
| Split window | Accent-colored `▼ name` header with its panes nested below |
| Single-pane window | One muted row with the window name (an agent's own row if it runs one) |
| Pane in a split window | `▢ command`, or its pane title when set |
| Agent | Status glyph, agent icon and name, then the pane command |
| Agent subject | Next line, prefixed with `↳` |

Panes running `nvim` or `lazygit` show the Neovim or git icon.

### Collapsing

- `▼` is open, `▶` is collapsed.
- A collapsed `▶` takes the color of the most urgent agent hidden under it
  (blocked, then done, then working) and blinks.
- Collapse state lasts until the sidebar daemon exits.
- Search and User attention show every match without changing what is
  collapsed.

### Agent icons

| Agent | Icon |
| --- | --- |
| Claude Code | `nf-cod-claude` (Nerd Fonts 3.5+) |
| Codex | `nf-cod-openai` (Nerd Fonts 3.5+) |
| Pi, Oh My Pi | `Pı` |
| OpenCode | `OC` |
| Hermes | `⚕` |

`display.agent_label` picks `icon-text` (default), `icon`, or `text`. The
installer sets it from its font check. Change an icon with `AGENT_ICON`; see
[Adding / overriding agents](custom-agents.md).

## Search and filters

- `/` starts a search. Type, then `Enter` to keep the query and navigate with
  `j`/`k`; `Enter` again jumps.
- While typing: `↑`/`↓` or `Ctrl-N`/`Ctrl-P` move, `Ctrl-U` clears.
- Search matches session, window, and pane details. A session or window match
  keeps everything under it; a pane match keeps its session and window.
- `f` (User attention) hides idle agents. It can't be combined with a text
  search.
- Empty results say `no panes`, `no agents`, or `no matches`.

## Mouse

Requires `set -g mouse on`, and works in the sidebar only (tmux does not send
mouse events into a popup).

- Click a row to select it; click it again to jump.
- Clicking elsewhere in the sidebar focuses it.
- Click a selected header to collapse or expand it.
- The wheel scrolls the list without changing selection.

## Status line

Add `#{agenmux}` to `status-right` or `status-left`:

```tmux
set -g status-right '#{agenmux} | %H:%M'
```

It shows counts such as `⣿1 ⣾2 ⣿1` in red/yellow/green (blocked/working/idle),
and stays empty when no agents are running.

## Popup mode

- `prefix + e` opens the same view in a floating popup (the installer suggests
  `prefix + a`).
- To make `prefix + A` open the popup too, set `@agenmux-display popup` or
  `display.mode = "popup"`.
- Close it with `q` or `Esc`. The popup holds the client, so there is no outside
  toggle.
- After a jump, the popup reopens over the selected agent.

## tmux management

On by default. Set `tmux_management.enabled = false` for a read-only sidebar.

| Keys | Action |
| --- | --- |
| `cc` | New window in the selected session; type a name or leave it blank |
| `cs` | New session; type a name or leave it blank |
| `r` | Rename the selected session, window, or pane in place |
| `dd` | Delete the selected record and everything under it |

- New windows start in the selected pane's directory. Focus stays in the
  sidebar.
- `Esc` cancels create and rename; an empty rename also cancels.
- Delete asks inline, showing the name and tmux ID. Only a lowercase `y`
  confirms; anything else cancels.
- Deleting a window or pane never removes its session implicitly; delete the
  session row for that.
- If the target disappeared meanwhile, the action reports an error and
  refreshes. It never acts on a different target.
- Pane splitting is not part of tmux management.

```toml
[tmux_management]
enabled = true
confirm_delete = true      # false deletes without asking

[keys]
sequence_timeout_ms = 1000 # time allowed between keys of gg/cc/cs/dd
```

## Quick launchers

With tmux management on, `oe` opens nvim and `og` opens lazygit in a new window
at the selected pane's directory. Press `o` to list the launchers.

Change, disable, or add launchers in `config.toml`:

```toml
[quick_launchers.lazygit]
enabled = false                # remove a built-in

[quick_launchers.terminal]
sequence = "ot"
label = "terminal"
command = "fish"
args = ["--login"]             # passed as separate arguments
working_directory = "selected" # or "tmux" for the session default
```

A launcher sequence can't clash with another launcher, a built-in sequence,
`G`, or a configured key.

## CLI

Commands for shell use:

```text
agenmux --version
agenmux scan | list | status        # scan is an alias for list
agenmux list <session>              # every pane in one session
agenmux list [session] --type <name> # one agent (claude) or command (zsh)
agenmux config [--help | check [--effective] | reload]
agenmux detect <conf> <screen-file> [title]
agenmux update [latest | vX.Y.Z]
agenmux releases refresh
```

The tmux integration also calls internal commands (`sidebar`, `daemon`, `key`,
`click`, `wheel`, `setup`, `toggle`, `pane-*`, `teardown`,
`notification-open`); don't call them yourself.

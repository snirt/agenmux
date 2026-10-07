# Configuration

agenmux reads two layers:

- **tmux options** (`@agenmux-*` in `tmux.conf`): launcher keys, plus live
  overrides for a few settings.
- **`config.toml`**: display, behavior, tmux management, theme, and keys.

A tmux option wins over the file when both set the same thing; see
[Precedence](#precedence).

## tmux options

| Option | Default | Purpose |
| --- | --- | --- |
| `@agenmux-key` | `A` | Main launcher (`prefix + key`); empty disables it |
| `@agenmux-popup-key` | `e` | Popup launcher; empty disables it |
| `@agenmux-display` | `split` | What the main key opens: `split` (sidebar) or `popup` |
| `@agenmux-width` | `30` sidebar, `40` popup | Width in cells |
| `@agenmux-height` | auto | Fixed popup height; auto prefers at least 15 rows |
| `@agenmux-hide-windows` | unset | Glob of windows to hide from the `prefix + w` picker; `''` restores the default picker |
| `@agenmux-notifications` | `on` | Desktop notifications; `off` disables |
| `@agenmux-debug` | unset | Trace file path for the next sidebar start; see [Troubleshooting](troubleshooting.md) |

### Launcher keys

- Launcher keys live only in tmux options; `config.toml` has no `keys.prefix`.
- Set custom keys **before** the TPM line, e.g. `set -g @agenmux-key 'E'`, then
  reload tmux.
- Normal tmux rules apply: the last binding of a key wins.
- Changing or clearing an option doesn't remove the old binding. Run
  `unbind-key A` (or your old key) and reload.
- An invalid `config.toml` never stops the launchers from installing.

To bind the keys yourself, clear both options before TPM and call the plugin
entrypoint (it verifies the engine first; don't bind the binary directly):

```tmux
set -g @agenmux-key ''
set -g @agenmux-popup-key ''
# plugin declarations, then:
run '~/.tmux/plugins/tpm/tpm'
bind-key A run-shell -b "bash #{q:@agenmux-plugin-dir}/agenmux.tmux activate '' #{q:client_name}"
bind-key e run-shell -b "bash #{q:@agenmux-plugin-dir}/agenmux.tmux activate 'popup' #{q:client_name}"
```

## config.toml

Location: `$XDG_CONFIG_HOME/agenmux/config.toml`, or
`~/.config/agenmux/config.toml` when `XDG_CONFIG_HOME` is unset. The file is
optional; every key has a default.

```toml
version = 1
[display]
mode = "split"            # split | popup
show_all_panes = true     # false: agents only
show_frame = true         # frame around the whole sidebar pane
sidebar_width = 30
popup_width = 40
popup_height = "auto"
agent_label = "icon-text" # icon-text | icon | text
[behavior]
notifications = true
auto_update = true        # false: no background update preparation
# hide_windows = "agents*" # unset: leave the window picker alone
[tmux_management]
enabled = true            # false: read-only sidebar
confirm_delete = true
resume_agents = false     # true: restore relaunches agents via AGENT_RESUME
undo_history = 20         # closed branches u can restore; 0: off
```

- `agenmux config --help` lists every key, its values, and its default.
- [`examples/config.toml`](../examples/config.toml) is a complete annotated
  example.
- Quick launchers are covered in [Usage](usage.md#quick-launchers).
- `behavior.auto_update` is explained in
  [Installation](installation.md#automatic-updates); a manual version switch
  sets it to `false`.
- The file is data only: no includes, variables, or shell commands. (Agent
  `.conf` files are different; they can run `SUBJECT_CMD`.)

### Settings view

Press `s` in the sidebar or popup to edit settings in place.

- `↑`/`↓` move, `Enter` edits, `C-u` clears the field, `Esc` cancels or goes back.
- Each row shows the file value, the effective value, and where it comes from.
- Values are validated before the file is saved; a bad value changes nothing.
- **Revert to defaults** clears saved display, behavior, theme, and key
  settings. tmux option overrides stay in effect.
- Launcher keys and agent `.conf` files are not edited here.

### Reloading

After editing the file by hand:

```sh
agenmux config reload
```

- Validates first; an invalid file is refused and nothing changes.
- Open sidebars and popups apply new settings, theme, and keys within a moment.
- There is no file watcher. Without a reload, a running sidebar keeps the
  settings it started with.

To validate without applying:

```sh
agenmux config check              # file only, no tmux needed
agenmux config check --effective  # also tmux options, with the source of each value
```

## Theme

```toml
[theme]
base = "light"         # dark (default) | light | terminal
[theme.colors]
working_bg = "#fff0cc" # override one role; the base fills the rest
pane_bg = 236
```

| Base | Use for |
| --- | --- |
| `dark` | Dark terminals (the original look) |
| `light` | Light terminals: dark text, pale selection fills |
| `terminal` | Your terminal's own default background |

- Colors are `"default"`, `0`–`255`, or `"#RRGGBB"`. ANSI names are not accepted.
- Roles: `header_fg`, `header_bg`, `pane_bg`, `selected_bg`, `text_fg`, `muted_fg`,
  `accent_fg`, `error_fg`, and `_fg` / `_bg` / `_bg_unfocused` for each of
  `blocked`, `working`, `idle`, `done`.
- `pane_bg` fills the selected ordinary-pane row; `muted_fg` colors
  ordinary-pane cursors. `selected_bg` fills rows picked with `v` / `V`.
- Colors change only color: glyphs, spinner, and blink stay the same.
- The theme applies to the sidebar, popup, help, and version picker. Global
  tmux colors are never changed.

## Keys

```toml
[keys.normal]
down = ["n", "PageDown"] # replaces the default ["j", "Down"]
up = ["p", "PageUp"]
close = []               # unbind (Ctrl-C/Ctrl-D still close the popup)
[keys.search]
cancel = ["Escape", "C-g"]
```

| Mode | Actions |
| --- | --- |
| `keys.normal` | `down`, `up`, `jump`, `search`, `filter`, `reset`, `help`, `versions`, `settings`, `close` |
| `keys.search` | `up`, `down`, `accept`, `cancel`, `backspace`, `clear` |

- A value replaces that action's whole default list (up to 16 chords); `[]`
  unbinds it and hides it from hints and help.
- A chord can serve only one action per mode.
- `gg` and `G` are fixed; binding either key to an action replaces that jump.
- Search mode can't use printable keys, since typing owns them.
- Changes apply on `agenmux config reload`.

Allowed chords: one printable ASCII character, `Space`, `Up`/`Down`/`Left`/`Right`,
`Home`/`End`, `PageUp`/`PageDown`, `Enter`, `Escape`, `Tab`, `BSpace`, or a
`C-x` control chord.

Reserved chords:

- `C-c`, `C-d`: always exit (`C-c` may still be the search cancel).
- `C-@`, `C-a`, `C-b`, `C-l`: used internally by the sidebar.
- `C-i`, `C-m`, `C-[`, `C-?`: the same as `Tab`, `Enter`, `Escape`, `BSpace`.

## Precedence

For each setting, the first layer that sets it wins:

1. Explicit mode passed on the command line (`agenmux toggle popup`)
2. `@agenmux-*` tmux option
3. Legacy `@agents-mon-*` tmux option
4. `config.toml`
5. Built-in default

- Every layer is validated, even one that loses.
- An **empty** `@agenmux-*` option resets that setting to its default and hides
  the legacy and file values. Empty launcher options disable the launcher.
- Width and height are `1`–`10000` cells, clamped to the space available.
- `@agenmux-notifications` accepts `on`/`off`, `true`/`false`, `yes`/`no`,
  `1`/`0` in any case. `@agenmux-display` also accepts `float` for `popup`.
- Dragging the sidebar border sets `@agenmux-width` live; unset it to go back to
  the file value.
- A bad live tmux override keeps the last valid setting and logs a warning.
- With an invalid file, closing, teardown, and key and mouse input still work.

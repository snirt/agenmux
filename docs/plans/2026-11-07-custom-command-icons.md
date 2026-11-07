# Custom Command Icons

## Goal

Let users choose the icon shown for non-agent panes and quick launcher items,
replacing the hardcoded `nvim`/`lazygit` glyphs with one configurable list.

## Decisions

- `config.toml` gains `[[command_icons]]` entries with `match` (regex) and
  `icon` (string; `""` means no icon). Unknown fields are rejected, so a later
  `color` field stays additive.
- Patterns match the whole command name (`^(?:pat)$`): `match = "vim"` does not
  match `nvim`. Patterns are compiled once per distinct pattern and cached; an
  invalid or oversized regex makes the file invalid, so `config check` and
  `config reload` refuse it.
- Rules match tmux `pane_current_command`, so panes started from a shell and
  panes opened by quick launchers behave the same.
- Precedence: user rules in file order, then built-ins (`nvim` U+F36F,
  `lazygit` U+E702), then the default window/pane glyph. First match wins.
- Agents are untouched: an agent row always shows its `AGENT_ICON`.
- Quick launcher items (launcher list, `o` continuations, help, footer
  hints) show an icon from the basename of `command`. A basename in a loaded
  conf's `AGENT_BINS` uses that agent's `AGENT_ICON` (hidden when
  `display.agent_label = "text"` or plain glyphs cannot show it); otherwise
  the command icon rules apply.
- `display.glyphs = "plain"` hides built-in command icons only; user icons are
  drawn as written.
- Limits: 64 rules, `match` 1..=256 bytes, `icon` at most 16 bytes, no control
  characters.
- `config check --effective` lists the rules in precedence order with their
  source. The settings view does not list them, and "Revert to defaults"
  keeps them.
- Not handled: wrapper processes (`python3` running a TUI) and per-rule color.

## Verification

- Unit tests in `app_config::command_icons` for parsing, validation, matching,
  precedence, plain mode, and the effective report.
- Renderer tests for user rules on pane rows, agent precedence, and launcher
  item icons.
- `cargo test` and `tests/run.sh`.

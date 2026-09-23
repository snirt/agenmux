# Agent and Neovim Icons (#121, #127)

## Goal

Make mixed agent sessions faster to scan with per-agent icons, and mark
Neovim panes in the all-panes inventory.

## Decisions

- `AGENT_ICON` in `agents/*.conf`: optional glyph or short string, prefixed
  before the bold agent name in both sidebar views. The text name always stays.
  Icon inherits the name style; state stays on the status dot.
- Width is counted per character like other sidebar text; double-width
  characters may misalign and are documented as such.
- Built-in defaults: Claude `nf-cod-claude` (U+EC82), Codex `nf-cod-openai`
  (U+EC81), both Nerd Fonts 3.5+; Pi and Oh My Pi `Pı` (P + dotless i
  U+0131); OpenCode `OC`, its own title prefix; Hermes plain `⚕`
  (U+2695).
- User `agents/<name>.conf` overrides only the keys it assigns. An empty value
  clears the key to its default, so `AGENT_ICON=""` disables a built-in icon.
- Icons are looked up from loaded confs at render time; scan TSV, search,
  and notifications stay unchanged.
- Ordinary panes whose command is `nvim` show `nf-linux-neovim` (U+F36F), and
  `lazygit` panes `nf-dev-git` (U+E702), in place of the window/pane glyph,
  with the same styling. Not configurable.

## Verification

- `conf::tests::override_sets_only_mentioned_keys`: merge, clear, and new agent.
- `semantic_renderer_frames`: icon rows in both views at wide and narrow
  widths, name-only rows without icons, Neovim glyph vs default window glyph.
- `cargo test` and `tests/run.sh`.

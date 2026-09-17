# Issue #103: configurable quick launchers

## Goal

Let the selected pane launch a configured program in a new tmux window at that
pane's working directory. Keep launchers behind `tmux_management.enabled` and
leave the plugin-opening tmux options (`@agenmux-key` and
`@agenmux-popup-key`) independent.

## Design

- Add ID-keyed `[quick_launchers.<id>]` application config entries. Resolve
  `nvim` (`e`) and `lazygit` (`og`) as defaults; let an entry override a
  default, disable it with `enabled = false`, or add a custom launcher.
- Accept a display label, one- or two-character alphanumeric sequence,
  executable, argv list, `selected` or `tmux` working-directory mode, and an
  enabled flag. Pass the executable and argv items separately to `new-window`
  so tmux starts the program directly without a shell command.
- Include active launcher bindings in the existing sequence dispatcher and
  private key tables. Validate active launcher conflicts against other
  launchers, built-in sequences, and the normal keymap before reload can
  install anything. Preserve existing `u`, `G`, and `gg` actions.
- At dispatch, capture the highlighted pane ID, then query its pane/window/
  session IDs and current path immediately before `new-window`. A failed or
  mismatched query reports an error without creating a window. Select the
  created pane for the invoking client, return that client to tmux's root key
  table, and request a sidebar refresh. Close a standalone popup after a
  successful launch.
- Show only enabled, management-gated launchers in key hints, continuation
  hints, and help. Document the schema and the independence from plugin opener
  keys in the README and example config.
- Pin hints to the last content row and lead with the help and settings shortcuts.
- Frame the whole Agenmux pane with a foreground-only theme accent; use tmux's
  active pane-border foreground while Agenmux has focus. Make the frame
  toggleable from the Display settings as `display.show_frame`.

## Implementation and verification

1. Extend the typed config, validation, effective rows, settings editing,
   help, and revert behavior; verify invalid values and conflicts.
2. Extend sequence matching and tmux binding generation; verify defaults,
   conflict reporting, management gating, and live binding reload.
3. Add selected-pane launch execution and focus/refresh behavior; cover cwd,
   shell argument boundaries, stale targets, and agent/ordinary rows with
   private tmux integration tests.
4. Run formatting, focused config/input/setup tests, the plugin integration
   suite, and the repository test script. Review the final diff and scan changed
   files for private identifiers or secret-like values.

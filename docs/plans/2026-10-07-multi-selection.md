# Multi-selection with `v` / `V` (#182)

## Decisions

- `v` and `V` are fixed keys, like `z`/`Z`, `u`, `R`, `x`: a configured chord
  or quick-launcher prefix on the same key wins. No `[keys.normal]` actions.
- Selection is held by tmux id (`$n` session, `@n` window, `%n` pane), so it
  survives rescans, search, filters, folding and the `.` view toggle. Hidden
  items stay selected but are not drawn; closed items drop out because the
  selection is always read back through the current pane list.
- `v` toggles the cursor row. In range mode it keeps the range and ends it.
- `V` anchors a range at the cursor row. The range is anchor..cursor in visible
  order on top of the earlier selection, so any cursor move (`j`/`k`, arrows,
  `gg`/`G`, click) grows or shrinks it. `V` again keeps the range and ends it.
- `Esc` ends range mode and clears the selection; with nothing selected it
  clears filters as before.
- Commands end range mode and clear the selection, like a vim operator:
  - `yy` copies every selected reference in tree order, one per line, or the
    cursor row with nothing selected.
  - `dd` deletes every selected record behind one confirmation (or none with
    `confirm_delete` off). A record whose session or window is also selected
    is skipped: deleting the parent removes it. Cancelling keeps the selection.
  - `r`, `cc`, `cs` and quick launchers still act on the cursor row; they clear
    the selection.
- Selected rows get a background tint from the new `selected_bg` theme role.
  The cursor row keeps its own colors. `terminal` uses palette color 8, since
  its other fills are the default background and would hide the selection.
- The footer shows `N selected · yy copy · dd delete · esc clear`, prefixed
  with `-- RANGE --` while a range is open.

## Wiring

- `src/input.rs`: `Key::Mark`/`Key::MarkRange`, decoded from `v`/`V` in both the
  popup and the daemon FIFO; `agenmux key mark|mark-range`.
- `src/setup.rs`: `v`/`V` in the fixed plugin-table bindings.
- `src/sidebar/filter.rs`: selection state over `visible`.
- `src/sidebar.rs`: dispatch, bulk `yy`/`dd`; `Overlay::Confirm` holds a list.
- `src/sidebar/render.rs`: tint and footer; help overlay and `docs/usage.md`.

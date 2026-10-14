# Single click selects, double click opens (#201)

## Problem

`input::click` jumps to the pane when the clicked agent row is already
selected, so repeated single clicks switch panes. Overlays repeat the same
"click the selected row to activate" rule (settings), or have no mouse rows at
all (versions, recently closed).

tmux 3.7 reports the second press within its 300ms click timeout as
`SecondClick1Pane`, not `MouseDown1Pane`, without checking that both presses
hit the same place. `DoubleClick1Pane` follows 300ms later and by default puts
the sidebar into copy mode.

## Design

One mouse model everywhere:

- Single click: select the row. Never activate.
- Double click (second press on the same record within tmux's click timeout):
  select and activate. Agent row: focus its pane for the clicking client.
  Branch header: toggle the branch. Overlay row: what Enter does there.

Pieces:

- Bindings: `MouseDown1Pane` runs `click`; `SecondClick1Pane` and
  `TripleClick1Pane` run `click … double`; `DoubleClick1Pane` is a no-op on
  sidebar panes. Native behaviour stays on other panes; all four are backed up
  and restored.
- `click` records the last press (client, sidebar pane, row target, index,
  time). A `double` press counts only when it matches that record, so presses
  on different rows never activate.
- The rows map marks branch headers `2` when selected and `3` otherwise.
- Protocol: `0x05` + index stays "select"; new `0x0f` + index is "open"
  (`Key::Open`). The main list selects and toggles a header; overlays share one
  helper that maps select/open to their selection and `Jump`.
- Versions and recently closed publish click rows; recently closed exposes its
  search line like settings does.

## Verification

- Unit: press classification, `0x0f` decoding, the shared overlay mouse helper,
  settings, the rows map flag for headers.
- `tests/navigation.sh`: a single click on the selected row keeps the sidebar
  focus; a double click focuses the pane.
- Live: real tmux client on a scratch socket, SGR mouse presses.

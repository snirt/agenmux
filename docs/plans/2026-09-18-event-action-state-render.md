# Sidebar: event → action → state → render

Restructure the sidebar engine so each layer has one job: input sources
produce events, events resolve to actions, actions mutate state or run tmux
effects, and rendering reads state only. Staged so every step ships on its
own with behavior unchanged; this document records the first step and the
rest of the route.

## Where the code already is

The split is further along than the single `event_loop` suggests:

- **Input decoding is already separate.** `input.rs` turns bytes from the
  popup tty or the daemon FIFO into a logical `Key` through the user keymap
  (`app_config::Action`, the *binding* enum) or the fixed FIFO protocol.
  `Key` is the sidebar's key event.
- **tmux is already an event source.** `Tmux::drain_notifications` folds
  `%output` / focus notifications into `PendingChanges`; the loop turns them
  into scan requests through `ScanSchedule`.
- **Rendering already reads state.** `render()` only reads the `Sidebar`
  fields and diffs against `last_frame`, with one exception noted below.

What was missing is the action layer: `dispatch_key` mapped keys straight
onto methods, so `j`, `Down`, a wheel tick, a click packet and a FIFO key
name each took their own path to the same state change, and the search-mode
copy of that mapping lived in `filter.rs`.

## Stage 1 (this change): the `Action` seam

- `src/sidebar/action.rs` holds `Action`, the list view's intentions
  (`MoveSelection`, `SelectIndex`, `Jump`, `FocusSearch`, `SearchInput`,
  `ToggleAllPanes`, `Quit`, `Close`, …), one resolver per input mode
  (`normal_action`, `search_action`) and `Sidebar::apply`, the only place an
  action becomes state or effects.
- `dispatch_key` keeps its protocol duties (client ownership, multi-key
  sequences, mode selection) and ends in `key → action → apply`. Coalesced
  navigation runs from held keys and wheel bursts apply the same actions.
- `filter.rs` owns query state through `push_query`, `pop_query` and
  `clear_query`; the search key table moved to `search_action`.
- Tests feed keys to the resolvers and actions to a real `Sidebar` (isolated
  tmux socket, no terminal, no render) and assert selection, viewport,
  query, attention filter and overlay state.

Naming: `app_config::Action` is the keymap *binding* the user configures,
`sidebar::action::Action` is the resulting intention. Renaming the keymap
enum to `KeyAction` or `Binding` is a mechanical follow-up once the second
name has settled.

## Later stages

Each one is independent and behavior preserving.

1. **Sequences and mutations as actions.** `SequenceResult::Match` currently
   calls `begin_mutation` directly. Add `Action::Mutate(SequenceAction)` so
   `gg`, `cc`, `dd`, `r` reach `apply` like every other key, and move the
   `Delete`/`Rename` scope resolution into a pure function tested without
   tmux.
2. **Overlay keys as actions.** Help, Versions and the Create/Rename/Confirm
   prompts map cleanly (`OverlayNext`, `OverlayPrev`, `OverlayAccept`,
   `OverlayCancel`, `PromptInput`). Settings is the largest and should go
   last; its key table in `settings_key` is already close to a reducer over
   the `Settings` struct.
3. **Effects out of state code.** `jump`, `execute_mutation`, `focus_sidebar`
   and the key-table switches fork `tmux` from inside handlers. Return an
   `Effect` (or push onto a queue the loop drains after `apply`) so the state
   step is testable without a server and effects run in one place. The
   `DispatchResult` enum is the first such effect (`Break`, `QuietExit`).
4. **An `Event` enum for the loop.** Only external stimuli belong there:
   `Key(Key)`, `Tmux(PendingChanges)`, `Scan { periodic }`, `Tick`,
   `Resize`, `Terminate`. `event_loop` becomes poll → event → handle, and
   the `ScanSchedule` and animation timers become event producers. Internal
   bookkeeping (frame diffing, cache writes) stays ordinary functions.
5. **Render without side effects.** `render_overlay` mutates
   `Overlay::Versions.sel` and reads the tag list from disk while drawing.
   Move both into the `OpenVersions` / overlay navigation actions so
   rendering is a pure read of `&Sidebar`.

Not planned: a message bus or reducer framework. The engine is one thread
with one owner of state; plain enums and methods are enough.

## Verified

- `cargo fmt`, `cargo clippy --all-targets` (one pre-existing warning in
  `focus_sidebar`, untouched), `cargo test --bin agenmux`: 145 passed
  (142 before, plus the three action tests).
- `cargo test --test cli --test pane_lock --test parity --test release`
  pass as before.

## Skipped

- `tests/plugin.rs` needs `expect` to attach a client and fails in an
  environment without it, before and after this change.

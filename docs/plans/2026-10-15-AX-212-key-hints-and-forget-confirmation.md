# AX-212: accurate key hints and safe history forgetting

Status: implemented and verified in isolated tmux servers; not deployed to the user's active installation.
Issue: https://github.com/snirt/agenmux/issues/212

## Goal

Overlay hints describe keys that actually work after rebinding. Forgetting a recently closed entry asks before permanently removing recovery history.

## Findings

- `src/sidebar/overlay.rs`: recently-closed hints hard-code `/ search`, `/ refine`, `Esc clear`, and `d forget`. The forget handler immediately removes the filtered selection from memory and the snapshot store.
- `render_settings` hard-codes confirmation, read-only error, navigation, and search hints. It is a free function, so it cannot directly call `self.hint`; build hints before borrowing the overlay mutably and pass rendered strings, or use an existing shared rendering helper. Avoid duplicating key-label logic.
- `src/sidebar/render.rs`: `hint`, `back_hint`, `nav_label`, and `join` already provide keymap-derived labels and omit unbound actions.
- `src/app_config/keys.rs`: no Forget action exists. `d` is a fixed management sequence prefix, not a rebindable keymap action. Do not invent `Action::Forget` merely to replace a literal.
- Settings text/select editing uses `settings_keys()` and the dedicated edit table. Its fixed editor keys must not be relabeled using unrelated normal-mode bindings.
- Existing destructive confirmation uses literal `y`; any other key cancels. Tree deletion optionally bypasses confirmation through `tmux_management.confirm_delete`.

## Proposed changes

### 1. Make command-mode hints accurate

In `src/sidebar/overlay.rs`, derive recently-closed search/refine from normal `Action::Search`, clear from `Action::Reset`, and restore from `Action::Jump`. Reuse `back_hint` for back/cancel where its accepted actions match the handler.

Derive settings revert-confirm from normal `Action::Jump` and cancellation/read-only back from the actions accepted by `settings_key`. Apply the same rule to settings list navigation and search/refine hints while touching this renderer. Preserve fixed editor hints unless input behavior changes too.

Keep the fixed `d` forget hint accurate and show it only where forgetting is enabled and reachable. No keymap schema expansion or new user option.

### 2. Add confirmation inside the recently-closed overlay

Keep pending confirmation state with the Undo overlay, rather than converting history entries to unrelated live-tmux mutation targets or nesting a second overlay. Store the pending entry identity, retaining selection and search state.

- First `d` on a valid filtered selection opens a clearly destructive prompt identifying the entry. No memory or disk mutation yet.
- Match the existing explicit `y` confirmation contract; other keys cancel. Consume confirmation input before search, movement, restore, or another forget action.
- On confirmation, resolve by identity, remove exactly that entry, call the existing store `forget`, and clamp selection against the remaining filtered rows.
- On cancellation, preserve history, query, selection, and list visibility.
- Keep prompt input reachable in popup and daemon/split modes. Inspect the text-input classification in `src/sidebar.rs` and key-table routing before choosing the minimal transport adjustment. Restore the correct table after confirm/cancel.
- Proposed safety policy: always confirm history forgetting, even when live-tmux delete confirmation is disabled. This protects permanent loss of recovery data without adding configuration. Revisit only if product intent requires sharing the existing opt-out.

### 3. Add focused regression tests

Extend existing overlay/render tests rather than introducing a new test harness.

- Rebind search, jump, reset, close, and navigation to non-default keys; verify relevant command-mode hints use those labels, not stale defaults.
- Unbind optional actions; verify no orphan hint labels or separators.
- Check settings confirmation and read-only error hints, plus wide/narrow/tiny clipping.
- Confirm fixed editor hints still correspond to actual editor input.
- Forget request does not change memory or persistent history. Cancellation by Escape and another key leaves both unchanged.
- `y` removes only the chosen entry, including when filtering changes visible indices. Verify selection clamping for last/only entry and no-op behavior on an empty list.
- While confirmation is pending, movement/search/restore/mouse input cannot delete or restore a different entry.
- Cover tmux management disabled and the proposed always-confirm policy with `confirm_delete = false`.
- Exercise public overlay dispatch/render paths; use temporary snapshot storage and existing fake tmux support where available.

## Verification

1. Run focused new Rust tests, then `cargo test --locked` and `bash tests/run.sh`, all invoked under explicit Bash.
2. Live-check a disposable tmux session against the built checkout binary: inspect configured binary/plugin paths rather than assuming the active installation uses this worktree.
3. Test popup and split/daemon modes with custom bindings: settings revert cancellation, read-only back, recently-closed search/refine/clear, forget cancellation, and confirmed forgetting of disposable history only.
4. Verify focus/key-table recovery after each confirmation outcome and returning to the main view. No real user history should be used for destructive tests.
5. Inspect exact diff and `git status`; review new files for credentials/private identifiers. Do not commit captures or local session output.

## Scope and expected files

- `src/sidebar/overlay.rs`: primary rendering, pending state, dispatch, tests.
- `src/sidebar/render.rs`: existing render test setup/Undo constructors, if affected.
- `src/sidebar.rs`: exhaustive matches/Undo constructors and input classification only if necessary.
- `src/input.rs` / `src/setup.rs`: inspect confirmation transport; change only if existing raw-text handling cannot be reused.

No module split, new dependency, broad keymap redesign, release bump, or agent-state detector changes.

## Verification results

- `cargo fmt --all --check`: passed.
- `cargo test --locked`: passed (296 passed, one existing ignored test). An earlier run timed out in the unrelated quick-launcher integration test; its focused rerun and subsequent full rerun passed.
- `bash tests/run.sh`: passed, including final rerun.
- New live integration test passes against a disposable snapshot in isolated tmux servers: standalone popup terminal-input path and split daemon path; custom hints, Escape cancellation, disk mutation only after `y`, and split key-table recovery. Actual popup chrome and the user's active daemon were not changed or tested.
- Pending confirmation always asks even with `confirm_delete = false`, as proposed.

## Review follow-up

- The prompt `forget <kind> <label>? y/N` uses the error color on the blank row under the title, so a long list or short pane cannot push it off screen. The label clips before `y/N` does; the footer is empty while it is pending. `y/N` replaces "any other key cancels", which the split edit table does not honor for keys it leaves unbound.
- Clear hints name both the close and reset keys, matching `Search::key`.
- One `forget_available` gate covers the hint, the `d` request, and the `y` confirmation, so a reload that disables management disarms a pending forget.
- The renderer test child gets a temporary `XDG_STATE_HOME`; the split integration test checks the key table after Escape as well as after `y`.
- Re-verified: `cargo fmt --all --check`, `cargo test --locked` (296 passed, one existing ignored), `bash tests/run.sh`. The remaining clippy warnings predate this branch.
- LSP checks reported no errors; one file check timed out. Compiling and full tests provide the completed Rust validation.

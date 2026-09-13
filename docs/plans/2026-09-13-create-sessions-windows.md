# Tmux Management Implementation Plan

**Goal:** Add gated sidebar create/delete/rename commands with shared mappings, exact-client transport, safe stable-ID mutation, and live-configurable sequence behavior.

**Architecture:** One built-in mapping drives dispatch, prefix hints, setup bindings, and help. `[tmux_management]` gates every mutation and controls delete confirmation. Sequence packets carry invoking client identity through framed FIFO transport. Create/delete/rename operations use tmux-native commands with stable IDs and existing control-mode refresh paths.

**Tech Stack:** Rust, tmux, TOML/Serde, existing unit/plugin/navigation tests.

## Requirements

- `[tmux_management] enabled = false` by default; `confirm_delete = true` by default.
- `[keys] sequence_timeout_ms = 1000` by default and must be positive.
- All fields are typed, validated, shown in effective config/example/docs, and live-reloaded.
- Disabled mutations are absent from prefix hints/help, cannot execute, and disabling clears pending `c`/`d`/`r`.
- Shared built-ins: `gg` first, `cc` create window, `cs` create session, `dp` delete pane, `dw` delete window, `ds` delete session, and `r` rename.
- First `c`/`d`/`g` shows valid continuations from the same mapping used by dispatch and help; `r` opens a pane/window/session scope chooser.
- Escape, invalid continuation, or timeout clears pending state/hint; timeout expires without another key.
- Create prompts for optional name. Enter accepts; blank lets tmux choose; Escape cancels.
- Create inherits selected pane cwd, revalidates stable IDs, uses `new-window`/`new-session`, switches invoking client to created first pane, and refreshes surviving sidebars.
- Mutation overlays accept framed daemon input only from the invoking client; legacy unowned packets remain compatible for popup/direct input.
- Delete revalidates stable pane/window/session IDs and uses `kill-pane`/`kill-window`/`kill-session`; `dp`/`dw` refuse to implicitly destroy a session, while `ds` moves attached clients first; stale targets error and refresh without fallback.
- With confirmation enabled, overlay displays exact resource type and identity; only explicit `y` confirms. Enter, `n`, Escape, or any other key cancels.
- Rename preloads the current pane/window/session name for inline typing and Backspace edits, revalidates the stable ID, and uses `select-pane -T`/`rename-window`/`rename-session`; control-only or empty names cancel and native tmux errors remain nonfatal.
- With confirmation disabled, completed delete sequence executes immediately.
- Sidebar self-removal is safe; surviving views refresh and preserve nearest valid selection.
- Pane creation/splitting remains excluded.
- Do not split existing modules.

---

### Slice 1: Complete typed live configuration

**Files:** `src/app_config.rs`, `examples/config.toml`, `tests/cli.rs`

- [x] Add failing tests for defaults, custom values, zero timeout, wrong types, effective rows, help, and shipped example.
- [x] Add `TmuxManagementConfig { enabled, confirm_delete }`, resolved `AppConfig` fields, validation, source tracking, help, and effective rows.
- [x] Preserve compatible `sequence_timeout_ms` work already present.
- [x] Update example config with exact default sections.
- [x] Run `cargo test app_config::tests` and `cargo test --test cli`.

### Slice 2: Shared mappings, framed client transport, and idle expiry

**Files:** `src/input.rs`, `src/setup.rs`, `src/main.rs`, `src/sidebar.rs`, `src/sidebar/render.rs`, `src/sidebar/overlay.rs`

- [x] Add one failing test for `c` continuations and `cc` dispatch; implement shared sequence table.
- [x] Add successive tests for `cs`, `d` continuations, `dp`/`dw`/`ds`, and existing `gg`.
- [x] Derive active setup bindings, prefix hints, help entries, and dispatch from same table; filter mutations when management is disabled or prefix is overridden by configured normal key.
- [x] Add framed sequence packet test carrying exact client; retain legacy packet decode and optional no-client CLI form.
- [x] Include pending deadline in event-loop poll wake; test default/custom expiry without follow-up input plus Escape/invalid dismissal.
- [x] On live reload, apply timeout/gate and clear pending mutation prefixes when management becomes disabled.
- [x] Run focused input/setup/sidebar tests.

### Slice 3: Create prompt and tmux creation

**Files:** `src/sidebar.rs`, `src/sidebar/overlay.rs`, `tests/plugin.rs`, `tests/navigation.sh`

- [x] Add failing public-flow test for `cc` opening optional-name prompt from selected pane context.
- [x] Reuse search-mode text routing for prompt input; blank Enter omits `-n`, text Enter supplies it, Escape creates nothing.
- [x] Snapshot selected cwd/session stable ID, then revalidate before mutation.
- [x] Execute argument-safe `new-window -d -P -F ... -t <session-id>: -c <cwd>` and `new-session -d -P -F ... -c <cwd>`, with optional names.
- [x] Parse returned stable IDs, switch exact invoking client to returned first pane/root table, and use existing popup jump handoff when popup owns client.
- [x] Add named/unnamed/cancel/stale/cwd/exact-client/switch/focus/refresh tests through public runtime detector/input path.
- [x] Run focused plugin and navigation tests.

### Slice 4: Confirmed and immediate deletion

**Files:** `src/sidebar.rs`, `src/sidebar/overlay.rs`, `tests/plugin.rs`, `tests/navigation.sh`

- [x] Add failing `dp` confirmation test showing exact pane ID/type and safe default cancellation.
- [x] Add confirmation tests: only `y` executes; Enter, `n`, Escape, and other keys cancel.
- [x] Add `confirm_delete = false` immediate-execution test.
- [x] Revalidate captured `%pane`, `@window`, or `$session` immediately before `kill-pane`, `kill-window`, or `kill-session`; no name/current-target fallback.
- [x] Report stale/nonfatal errors to invoking client and request immediate refresh.
- [x] Add real tmux tests for each delete scope, implicit session-destruction guards, wrong-target prevention, sidebar self-removal, surviving-view refresh, and nearest valid selection.
- [x] Run focused plugin and navigation tests.

### Slice 5: Documentation and full verification

**Files:** `README.md`, `tests/navigation.sh`

- [x] Document gate defaults, confirmation behavior, timeout, `cc`/`cs`/`dp`/`dw`/`ds`, and pane-splitting exclusion.
- [x] Verify disabled commands absent from hints/help and cannot execute; verify live enable/disable and pending-prefix clearing.
- [x] Run `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test`, `/bin/bash tests/run.sh`, and `/bin/bash tests/navigation.sh`.
- [x] Inspect active `@agenmux-bin` and its plugin root/configs; because it belongs to another worktree, leave it untouched and verify this checkout with a fresh isolated real-tmux daemon.
- [x] Inspect `git status`, exact diff, privacy identifiers, secret-like values, and confirm no raw captures/logs/session data are tracked.

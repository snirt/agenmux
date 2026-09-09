# Implementation Plan

**Ticket:** [#69 — Show all tmux panes in optional sidebar tree](https://github.com/snirt/agenmux/issues/69)  
**State:** OPEN  
**Plan path after acceptance:** `docs/plans/2026-09-09-show-all-panes-sidebar.md`

**Goal:** Add optional `display.show_all_panes = true` sidebar hierarchy while preserving default agent-only behavior and every agent-only runtime contract.

**Architecture:** Change `scan::scan` to return one `ScanSnapshot` containing complete pane inventory and separate agent projection from one existing `list-panes -a` command. Sidebar joins debounced agent state onto inventory by scanner-provided index, then renders selectable pane occurrences. Detection, attention, notifications, cache, status, and CLI consume only `ScanSnapshot.agents`.

**Tech stack:** Rust 2021, existing tmux control-mode connection, TOML configuration, current renderer/private-tmux tests. No dependency, module split, or second recurring pane query.

## Discovery and fixed decisions

- Baseline: `40dfdc62fb3171d3d8d12332bffd3d47c801938c`, clean `AX-69-show-all-panes-sidebar`.
- `mise exec rust@latest -- cargo test --all-targets` passes. Default Rust 1.89 fails before tests because `mac-usernotifications@0.3.1` requires Rust 1.90.
- Primary LSP diagnostics: zero errors across relevant files.
- `list-panes -a` duplicates linked-window occurrences; same physical pane can appear under multiple sessions. Display selection therefore uses stable occurrence key `(session_id, window_id, pane_id)`, while tracker/navigation keep physical `pane_id`.
- `show_all_panes=false` keeps existing cached first frame, renderer bytes, search semantics, row map, and `no agents`.
- `show_all_panes=true` never presents agent-only cache as complete tree; first frame may show `no panes` until existing immediate scan completes.
- Search fields: session name/ID, window name/index/ID, pane index/ID/title/command/path, and attached agent name/state/cwd/subject/location.
- Match semantics:
  - Session match includes all descendant panes.
  - Window match includes all descendant panes plus session context.
  - Pane or agent match includes matching pane plus window/session ancestors.
  - Status filter matches exact agent states only, then adds ancestors.
- Filter counts mean selectable matching panes / total selectable panes for current mode. Headers and subjects never count.
- Window collapse uses complete sidebar-excluded inventory, not filtered count or raw `window_panes`.
- True-mode row labels:
  ```text
  work
  ├─ 1 editor · nvim
  └─ 2 server
     ├─ 1 npm
     └─ 2 ● claude working · repo
          Implement sidebar tree
  ```
- In true mode, session/window headers and subject continuation lines map to `-`; only pane rows map to pane IDs. False mode keeps current subject-line mapping.
- Current sidebar ownership test remains narrow: exact `self_pane`, `@agenmux=1`, or processless `pane_title=agenmux`. Ordinary user panes merely titled `agenmux` remain visible.
- Tabs/newlines or malformed IDs/numbers make that tmux row malformed; skip it and continue. PID `0` remains valid for processless panes.
- Popup auto-height remains agent-cache-based; normal scrolling handles larger tree. No topology cache added.

## Current flow to preserve

1. `main::{cmd_scan,cmd_status}` calls `run_scan`.
2. `scan::scan` runs one `LIST_FMT`, identifies agents, captures only recognized agents, then calls public detector functions.
3. `scan::{to_tsv,from_tsv,status_segment}` owns six-column agent-only output/cache.
4. `Sidebar::scan_tick` writes agent cache, calls `Tracker::update`, emits agent notifications, rebuilds projection, and renders.
5. `sidebar::daemon::mirror_tick` separately inventories Agenmux mirror panes for writer/lifecycle management; this is not content enumeration and remains unchanged.
6. `sidebar::filter` owns query/status projection and selection restoration.
7. `sidebar::render` owns visual rows and `agenmux-rows` mouse map.
8. `input::click` and `Sidebar::jump` already validate and target exact pane IDs.
9. `app_config::reload` publishes `@agenmux-reload`; `LiveConfig::refresh` adopts file changes without daemon restart.

## File map

**Modify**

- `src/app_config.rs` — schema, resolved default, source reporting, help, tests.
- `examples/config.toml` — explicit default.
- `src/scan.rs` — pane inventory, strict parsing, composite scan result.
- `src/main.rs` — CLI selects agent projection.
- `src/sidebar.rs` — inventory state, mode adoption, occurrence selection, scan flow, jump/focus.
- `src/sidebar/filter.rs` — pane projection and hierarchy-aware filtering.
- `src/sidebar/render.rs` — true-mode tree, row map, empty state, renderer fixtures.
- `src/sidebar/overlay.rs` — true-mode help says “jump to pane”.
- `tests/fixtures/sidebar/dark.frames` — append sanitized true-mode frames; retain existing false-mode prefix unchanged.
- `tests/plugin.rs` — private-tmux scanner/config/reload/query-count coverage.
- `tests/navigation.sh` — ordinary-pane keyboard and mouse navigation through public runtime.
- `README.md` — configuration and interaction documentation.

**Do not modify unless a failing test proves necessity**

- `src/sidebar/daemon.rs` — existing reload call already reaches `adopt_reload`.
- `src/attention.rs` — remains agent-only.
- `src/input.rs` — existing pane-ID click transport already supports ordinary panes.
- `src/detect.rs`, `src/procs.rs`, `src/notifications.rs` — unchanged agent behavior.
- `src/panes.rs`, `src/setup.rs`, `src/toggle.rs` — no new lifecycle/config option required.
- `Cargo.toml`, `Cargo.lock` — no dependency.

## Internal interfaces

Add in `src/scan.rs`:

```rust
pub struct PaneMeta {
    pub pane: String,
    pub pane_index: u32,
    pub pane_title: String,
    pub command: String,
    pub path: String,
    pub window_id: String,
    pub window_index: u32,
    pub window_name: String,
    pub session_id: String,
    pub session_name: String,
    pub agent_index: Option<usize>,
}

pub struct ScanSnapshot {
    pub panes: Vec<PaneMeta>,
    pub agents: Vec<PaneRow>,
}

pub fn scan(
    tmux: &mut Tmux,
    confs: &[AgentConf],
    cache: &mut IdentCache,
    subj: &mut SubjectCache,
    self_pane: Option<&str>,
) -> Result<ScanSnapshot, TmuxError>;
```

`agent_index` points into `ScanSnapshot.agents`. `Tracker::update` preserves agent order, so index remains valid after debounce.

Add in `src/sidebar.rs`:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
struct PaneOccurrence {
    session_id: String,
    window_id: String,
    pane: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VisiblePane {
    Agent(usize),
    Inventory(usize),
}
```

`VisiblePane::Agent` supports cached/default agent mode. `VisiblePane::Inventory` supports true mode. Headers never enter `visible`.

## Task 0: Record accepted plan

**File:** Create `docs/plans/2026-09-09-show-all-panes-sidebar.md`

- [ ] Wait for explicit plan acceptance.
- [ ] Save this accepted plan verbatim under project-required path.
- [ ] Confirm no implementation started before acceptance.
- [ ] Commit:

```bash
git add docs/plans/2026-09-09-show-all-panes-sidebar.md
git commit -m "docs: plan all-pane sidebar tree"
```

## Task 1: Add configuration switch

**Files:** `src/app_config.rs`, `examples/config.toml`, `tests/plugin.rs`

**Interfaces:** `DisplayConfig::show_all_panes: Option<bool>` and `AppConfig::show_all_panes: bool`.

- [ ] **RED:** Extend `defaults_and_empty_layer_semantics`:

```rust
assert!(!default.show_all_panes);
let enabled = parse("[display]\nshow_all_panes = true").unwrap();
let enabled = resolve(&enabled, &BTreeMap::new()).unwrap();
assert!(enabled.show_all_panes);
assert_eq!(enabled.sources["display.show_all_panes"], "file");
```

- [ ] **RED:** Add wrong-type rejection:

```rust
assert!(parse("[display]\nshow_all_panes = 'true'").is_err());
```

- [ ] **RED:** Add `show_all_panes = false` under `[display]` in `examples/config.toml`. Existing CLI help drift test must fail until help includes it.
- [ ] **RED:** In `setup_resolves_legacy_options_without_copying_behavior`, assert effective output:

```rust
assert_eq!(row("display.show_all_panes"), "false default");
```

- [ ] Run and confirm failures:

```bash
mise exec rust@latest -- cargo test --bin agenmux app_config::tests::defaults_and_empty_layer_semantics
mise exec rust@latest -- cargo test --test cli config_help_lists_every_configurable_key_without_a_server
mise exec rust@latest -- cargo test --test plugin setup_resolves_legacy_options_without_copying_behavior
```

- [ ] **GREEN:** Wire field through `DisplayConfig`, `AppConfig`, `resolve_cli` defaults, `apply`, `sources`, `rows`, and `help`. Do not add `@agenmux-*` compatibility option.
- [ ] Run:

```bash
mise exec rust@latest -- cargo test --bin agenmux app_config
mise exec rust@latest -- cargo test --test cli
mise exec rust@latest -- cargo test --test plugin setup_resolves_legacy_options_without_copying_behavior
```

- [ ] Commit:

```bash
git add src/app_config.rs examples/config.toml tests/plugin.rs
git commit -m "feat(config): add all-pane sidebar switch"
```

## Task 2: Return inventory and agent projection from one scan

**Files:** `src/scan.rs`, `src/main.rs`, `src/sidebar.rs`, `tests/plugin.rs`

- [ ] **RED:** Add parser tests containing valid agent/ordinary rows, PID `0`, missing fields, invalid numeric indexes, embedded tab/newline fragments, `self_pane`, marked sidebar, processless titled sidebar, and ordinary nonzero-PID pane titled `agenmux`.
- [ ] Assert malformed/sidebar rows are absent while later valid rows survive.
- [ ] **RED:** Add private-tmux test with fake `codex`, ordinary `sleep`, and sidebar panes. Assert:
  - inventory contains agent and ordinary panes;
  - agent projection contains only fake `codex`;
  - `scan` and `list` stdout remain identical six-column agent-only TSV;
  - `status` excludes ordinary panes.
- [ ] Run and confirm failures:

```bash
mise exec rust@latest -- cargo test --bin agenmux scan::tests
mise exec rust@latest -- cargo test --test plugin scan_keeps_inventory_separate_from_agent_output
```

- [ ] **GREEN:** Expand `LIST_FMT` with session/window/pane IDs, indexes, names, command, path, title, and `@agenmux`.
- [ ] Parse each line once into validated metadata. Skip malformed records; never convert malformed numeric text to zero.
- [ ] Exclude sidebar records before `procs::identify` and `capture-pane`.
- [ ] Push every valid work pane into `ScanSnapshot.panes`; push only detected agents into `.agents`; set `agent_index`.
- [ ] Keep `PaneRow`, `to_tsv`, `from_tsv`, and `status_segment` unchanged.
- [ ] Change `main::run_scan` to return `scan(...).map(|snapshot| snapshot.agents)`.
- [ ] Temporarily adapt `Sidebar::scan_tick` to consume `.agents`; tree inventory becomes active in Task 3.
- [ ] Add temp `AGENMUX_DEBUG` assertion: completed scan-note count equals content-query count for exact `list-panes -a -F`; existing filtered mirror query does not count.
- [ ] Run:

```bash
mise exec rust@latest -- cargo test --bin agenmux scan::tests
mise exec rust@latest -- cargo test --test cli scan_is_an_exact_alias_for_list_without_a_server
mise exec rust@latest -- cargo test --test plugin scan_keeps_inventory_separate_from_agent_output
mise exec rust@latest -- cargo test --test plugin sidebar_refresh_uses_one_content_enumeration
```

- [ ] Commit:

```bash
git add src/scan.rs src/main.rs src/sidebar.rs tests/plugin.rs
git commit -m "refactor(scan): expose pane inventory with agents"
```

## Task 3: Render and navigate complete hierarchy

**Files:** `src/sidebar.rs`, `src/sidebar/filter.rs`, `src/sidebar/render.rs`, `src/sidebar/overlay.rs`, `tests/fixtures/sidebar/dark.frames`

- [ ] **RED:** Extend semantic renderer fixture with sanitized inventory matching issue example plus second session. Assert exact hierarchy, single-pane collapse, expanded multi-pane window, agent details, and subject indentation.
- [ ] Assert existing false-mode fixture sections remain byte-identical.
- [ ] Assert true-mode rows map:
  - session header: `-`
  - expanded window header: `-`
  - each pane row: exact `%pane_id`, selection ordinal, selected flag
  - subject: `-`
- [ ] Add true empty inventory frame expecting `no panes`; retain false empty frame expecting `no agents`.
- [ ] Run and confirm failure:

```bash
mise exec rust@latest -- cargo test --bin agenmux sidebar::render::tests::semantic_renderer_frames -- --nocapture
```

- [ ] **GREEN:** Store latest `ScanSnapshot.panes` in `Sidebar`; keep tracker-updated `rows` agent-only.
- [ ] Replace visible-index assumptions with `VisiblePane` helpers for pane ID, occurrence, optional agent row, state, and active matching.
- [ ] Preserve cached first frame with `VisiblePane::Agent`; true mode ignores partial cache until first inventory scan.
- [ ] Render true-mode groups by `(session_id, window_id)` in scanner order.
- [ ] Determine collapse from complete inventory counts after sidebar exclusion, before filtering.
- [ ] Preserve existing state glyph/background behavior for agent pane rows. Use neutral styling for ordinary panes.
- [ ] Make `jump`, focus snap, animation checks, scrolling, and row counts consume selectable pane entries.
- [ ] Use `PaneOccurrence` for restoration; if exact occurrence vanished, fall back to same physical pane, then nearest valid ordinal.
- [ ] For active linked panes, prefer occurrence whose `session_id == active_session`.
- [ ] Change help text to “jump to pane” only when all-pane mode is active.
- [ ] Run:

```bash
mise exec rust@latest -- cargo test --bin agenmux sidebar::render
mise exec rust@latest -- cargo test --bin agenmux sidebar::filter
mise exec rust@latest -- cargo test --bin agenmux sidebar
```

- [ ] Commit:

```bash
git add src/sidebar.rs src/sidebar/filter.rs src/sidebar/render.rs \
  src/sidebar/overlay.rs tests/fixtures/sidebar/dark.frames
git commit -m "feat(sidebar): render complete tmux hierarchy"
```

## Task 4: Preserve hierarchy through search and status filters

**Files:** `src/sidebar/filter.rs`, `src/sidebar/render.rs`, `tests/fixtures/sidebar/dark.frames`

- [ ] **RED:** Build unit inventory containing duplicate session/window names, linked-window occurrence, ordinary panes, and blocked/working/idle agents.
- [ ] Add exact tests:
  - session name/ID query returns all descendants only for matching session occurrences;
  - window name query returns full matching window subtrees;
  - window index query respects per-session occurrences;
  - pane index/ID/title/command/path query returns matching panes only;
  - agent name/state/cwd/subject/location query returns host panes only;
  - matching remains case-insensitive;
  - exact status filter returns matching agent panes only;
  - ordinary command/title containing `working` never matches working status;
  - physical multi-pane window remains expanded when filter leaves one pane;
  - no matches produce no orphan headers.
- [ ] Keep existing false-mode tests unchanged and green.
- [ ] Run and confirm failures:

```bash
mise exec rust@latest -- cargo test --bin agenmux sidebar::filter::tests
```

- [ ] **GREEN:** Implement true-mode projection:
  ```text
  session_match || window_match || pane_match || agent_match
  ```
- [ ] Use IDs for grouping/context and labels for matching; never group by display names.
- [ ] Status path ignores query and non-agent metadata, matching current mutual exclusion.
- [ ] Render only ancestors of projected panes; include descendants only for matched session/window.
- [ ] Append renderer fixture frames for session, window, pane, agent, status, and absent queries.
- [ ] Run:

```bash
mise exec rust@latest -- cargo test --bin agenmux sidebar::filter
mise exec rust@latest -- cargo test --bin agenmux sidebar::render::tests::semantic_renderer_frames -- --nocapture
```

- [ ] Commit:

```bash
git add src/sidebar/filter.rs src/sidebar/render.rs tests/fixtures/sidebar/dark.frames
git commit -m "feat(sidebar): retain tree context while filtering"
```

## Task 5: Prove live reload and exact-pane navigation

**Files:** `src/sidebar.rs`, `tests/plugin.rs`, `tests/navigation.sh`

- [ ] **RED:** Add private-tmux daemon test with:
  - two sessions;
  - multiple windows;
  - one single ordinary window;
  - one multi-pane mixed window;
  - at least one fake agent;
  - processless sidebar panes.
- [ ] Start false, record control client and sidebar IDs, then write true config and invoke public `config reload`.
- [ ] Assert without restart:
  - hierarchy appears;
  - sidebar panes remain excluded;
  - selected agent occurrence remains selected;
  - daemon/control/sidebar IDs remain unchanged;
  - all-non-agent session appears only in true mode.
- [ ] Reload false and assert ordinary panes disappear, agent selection remains where visible, and `no agents` returns for all-non-agent case.
- [ ] Add invalid reload between valid transitions and assert last valid mode remains.
- [ ] Extend `tests/navigation.sh`: while true mode is live, create temporary ordinary pane as last selectable row; use actual `G` then `Enter` to jump to it; return to sidebar and use first/second mouse clicks to select then jump to same exact pane. Kill temporary pane and restore false mode before existing assertions continue.
- [ ] Run and confirm failures:

```bash
mise exec rust@latest -- cargo test --test plugin all_panes_reload_preserves_daemon_and_selection -- --nocapture
/bin/bash -c 'mise exec rust@latest -- cargo build && AGENMUX_BIN=target/debug/agenmux ./tests/navigation.sh'
```

- [ ] **GREEN:** Track adopted display mode inside `Sidebar::adopt_reload`; when mode changes, rebuild projection, restore occurrence selection, reconsider active pane even when `active == last_active`, and clear `last_frame`.
- [ ] Keep daemon refresh order unchanged unless test proves same-tick render failure. If adjustment is needed, change only call order in `src/sidebar/daemon.rs`; do not add query or state owner.
- [ ] Verify cache throughout test remains six-column agent-only TSV and tracker emits no ordinary-pane event.
- [ ] Run:

```bash
mise exec rust@latest -- cargo test --test plugin all_panes_reload_preserves_daemon_and_selection -- --nocapture
/bin/bash -c 'mise exec rust@latest -- cargo build && AGENMUX_BIN=target/debug/agenmux ./tests/navigation.sh'
```

- [ ] Commit:

```bash
git add src/sidebar.rs tests/plugin.rs tests/navigation.sh
git commit -m "test(sidebar): cover all-pane reload and navigation"
```

## Task 6: Document, fully verify, and inspect privacy

**File:** `README.md`

- [ ] Document `display.show_all_panes = false`, hierarchy/collapse rules, pane-only selection, search ancestry, status ancestry, and mode-specific empty states.
- [ ] State `scan`, `list`, status, detection, attention, and notifications remain agent-only.
- [ ] Run targeted diagnostics before builds:

```bash
# Run through pi LSP diagnostics:
# src/app_config.rs src/scan.rs src/main.rs src/sidebar.rs
# src/sidebar/filter.rs src/sidebar/render.rs src/sidebar/overlay.rs
# tests/plugin.rs
```

- [ ] Run full CI-equivalent checks:

```bash
/bin/bash -c 'mise exec rust@latest -- cargo test --locked'
/bin/bash -c 'mise exec rust@latest -- cargo build --release --locked'
/bin/bash -c 'AGENMUX_BIN=target/release/agenmux ./tests/run.sh'
```

- [ ] Commit docs only after checks pass:

```bash
git add README.md
git commit -m "docs: explain all-pane sidebar mode"
```

## Live verification and deployment

Use fresh private tmux server first; never assume checkout binary is deployed.

- [ ] Inspect configured binary and derive loaded plugin root:

```bash
bin="$(tmux show-option -gqv @agenmux-bin)"
printf 'binary=%s\n' "$bin"
root="$(cd "$(dirname "$bin")" && pwd)"
while [ "$root" != / ] && [ ! -d "$root/agents" ]; do
  root="$(dirname "$root")"
done
test -d "$root/agents"
printf 'plugin_root=%s\n' "$root"
tmux show-option -gqv @agenmux-plugin-dir
find "$root/agents" -maxdepth 1 -type f -name '*.conf' -print | sort
```

- [ ] Deploy worktree binary and restart active daemon safely:

```bash
/bin/bash -c 'mise exec rust@latest -- ./scripts/dev-bin.sh use'
tmux show-option -gqv @agenmux-bin
tmux show-option -gqv @agenmux-plugin-dir
```

- [ ] In fresh tmux pane, launch real configured agent, capture pane title/screen only to temporary untracked location, and drive active work then completion:

```bash
agent_pane="$(tmux new-window -d -P -F '#{pane_id}' -n ax69-live "$SHELL")"
tmux send-keys -t "$agent_pane" 'pi' Enter
tmux send-keys -t "$agent_pane" \
  "Run /bin/bash -c 'for n in 1 2 3 4 5; do echo \$n; sleep 1; done', then report done." Enter
```

- [ ] Through deployed public scanner, verify pane reports `working` during activity and `idle` after completion:

```bash
"$bin" scan | awk -F '\t' -v pane="$agent_pane" '$1 == pane { print $4 }'
```

- [ ] Verify false → true → false live reload visually without daemon/control-client identity changing. Then restart daemon once after config changes and repeat working → idle detector transition, per project rule.
- [ ] Confirm Enter and mouse jump to exact ordinary and agent panes.
- [ ] Restore release binary after live verification:

```bash
/bin/bash -c './scripts/dev-bin.sh stop'
```

## Final diff/privacy checks

- [ ] Inspect exact tracked and untracked scope:

```bash
git status --short
git diff --check
git diff --stat
git diff -- . ':(exclude).pi'
git log --oneline --decorate -8
```

- [ ] Scan changed/new files for secrets and private identifiers:

```bash
git ls-files -co --exclude-standard -z |
  xargs -0 grep -nEI \
  '(api[_-]?key|token|secret|password|authorization:|BEGIN [A-Z ]*PRIVATE KEY|/Users/|snir\.turgeman|@[^ ]+\.[^ ]+)' \
  || true
```

- [ ] Review every fixture/log-like addition manually. Keep only synthetic names such as `work`, `server`, `repo`, `claude`, `codex`, `npm`; no raw captures, prompts, home paths, hostnames, emails, sockets, or agent session data.
- [ ] Confirm `.pi/` remains ignored and absent from commits.
- [ ] Run `lens_diagnostics mode=all`; fix every blocking error before declaring completion.

## Acceptance coverage

- Config surfaces: Task 1.
- False-mode compatibility: Tasks 1–4 plus unchanged legacy frame prefix and full suite.
- Full hierarchy/single-pane collapse: Task 3.
- Agent/ordinary keyboard and mouse navigation: Task 5.
- Search/status ancestor context: Task 4.
- False → true → false live reload and selection: Task 5.
- One content enumeration: Task 2 debug-count test and scanner interface.
- Mixed panes/windows/sessions/private tmux: Tasks 3 and 5.
- Agent-only detector/status/notifications/cache/CLI: Tasks 2 and 5.
- Deployment, real-agent transition, privacy/diff checks: Task 6.

Implementation remains blocked until explicit plan acceptance.

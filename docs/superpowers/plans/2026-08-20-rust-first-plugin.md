# Rust-First Plugin Migration Implementation Plan

> **For agentic workers:** use the `subagent-driven-development` skill to
> implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for
> tracking.

**Goal:** Make Rust own every plugin behavior that can run after the native binary is available, while preserving the current tmux UI, navigation, lifecycle, update, detection, and notification contracts.

**Architecture:** Keep `agents-mon.tmux` and a reduced `scripts/install-bin.sh` as the unavoidable TPM/pre-binary bootstrap. Add explicit Rust commands for setup, toggle, mouse input, pane lifecycle, and release switching; all commands reuse one Rust tmux adapter and the existing daemon/sidebar state. Remove the duplicated Bash scanner/sidebar only after native installation opens the plugin successfully on all supported release platforms.

**Tech Stack:** Rust 2021, existing `regex`/`libc`/`sysinfo` dependencies, tmux control mode and CLI, Cargo integration tests, isolated tmux servers, existing shell smoke tests.

## Global Constraints

- Preserve tmux options, hook indexes, key tables, pane title `agents-mon`, temp-file names, TSV output, CLI output, and exit codes unless a task explicitly replaces an internal contract.
- Preserve split and popup behavior, mouse behavior, search/filter/navigation semantics, layout restoration, multi-client targeting, processless panes, update/rollback, notifications, agent config overrides, and status-line output.
- Keep exact originating-client checks for mouse actions; never infer a different client after a background delay.
- Keep size-matched layout restoration: never apply an absolute tmux layout after the window size changes.
- Keep `agents-mon.tmux` sourceable by TPM before any Rust binary exists.
- Do not add a Rust HTTP client or async runtime. Bootstrap may continue using `curl`, `tar`, `git`, and platform tools.
- Keep one coherent Bash fallback until Task 8 proves the native bootstrap path; do not partially port `scripts/scan.sh` or `scripts/sidebar.sh`.
- Each task must pass `cargo test` and the named isolated-tmux checks before commit.

## Target File Structure

- `src/main.rs` — CLI dispatch only.
- `src/plugin.rs` — native setup/toggle, client selection, click/wheel handling, pane lifecycle, layout restore, and teardown.
- `src/release.rs` — release discovery, verified package staging through the bootstrap fetch primitive, source switching, and restart coordination.
- `src/tmux.rs` — shared tmux transport, command execution, output parsing, and escaping.
- `src/sidebar.rs` — existing daemon/sidebar state, plus in-memory wheel debounce; no script spawning.
- `tests/plugin.rs` — Rust integration tests against private tmux servers.
- `tests/release.rs` — Rust update/rollback tests using local fixtures and command stubs.
- `agents-mon.tmux` — minimal TPM/pre-binary bootstrap and invocation glue.
- `scripts/install-bin.sh` — only code that must work before Rust exists: platform selection, download, SHA-256 verification, atomic binary install, and local Cargo fallback.
- `scripts/install-app.sh` — macOS app-bundle packaging/codesign; keep because it is platform packaging, not plugin runtime logic.
- `scripts/version.sh` — release/CI manifest check usable before the binary exists.

---

### Task 1: Freeze the Shell Behavior as Native Acceptance Tests

**Files:**
- Create: `tests/plugin.rs`
- Modify: `tests/navigation.sh`
- Modify: `tests/run.sh`

**Interfaces:**
- Consumes: current scripts and `agents-mon` binary.
- Produces: reusable private-tmux helpers and behavior tests that later tasks reroute from shell scripts to Rust commands.

- [ ] **Step 1: Extract the existing private tmux-server setup into Rust test helpers**

Create helpers in `tests/plugin.rs` with these signatures:

```rust
struct TestTmux {
    socket: String,
    tmp: std::path::PathBuf,
}

impl TestTmux {
    fn new(name: &str) -> Self;
    fn tmux(&self, args: &[&str]) -> std::process::Output;
    fn bin(&self, args: &[&str]) -> std::process::Output;
}

impl Drop for TestTmux {
    fn drop(&mut self);
}
```

Use `CARGO_BIN_EXE_agents-mon`, a unique `tmux -L` socket, `TMPDIR`, and `AGENTS_MON_DIR`; kill the private server in `Drop`.

- [ ] **Step 2: Add missing characterization tests**

Add tests named:

```rust
#[test] fn restore_skips_a_layout_after_window_size_changes();
#[test] fn mirror_add_is_idempotent_under_concurrent_calls();
#[test] fn wheel_off_moves_without_jumping();
#[test] fn wheel_custom_delay_jumps_only_once();
#[test] fn newest_non_control_client_wins();
#[test] fn stale_click_origin_is_a_noop();
```

For now invoke the current scripts. Assert observable tmux state—pane count/title/width, selected pane/window, and client key table—not command strings.

- [ ] **Step 3: Run the characterization tests**

Run:

```bash
cargo test --test plugin -- --test-threads=1
./tests/navigation.sh
./tests/run.sh
```

Expected: PASS on the current Bash implementation.

- [ ] **Step 4: Commit**

```bash
git add tests/plugin.rs tests/navigation.sh tests/run.sh
git commit -m "test: freeze plugin shell behavior"
```

### Task 2: Add a Shared Rust tmux Command Layer

**Files:**
- Modify: `src/tmux.rs`
- Modify: `src/main.rs`
- Test: `src/tmux.rs`

**Interfaces:**
- Consumes: `TMUX`, normal tmux argv, current control-mode `Tmux`.
- Produces:

```rust
pub fn command(args: &[&str]) -> Result<String, TmuxError>;
pub fn command_status(args: &[&str]) -> Result<(), TmuxError>;
pub fn lines(args: &[&str]) -> Result<Vec<String>, TmuxError>;
pub fn format_truth(value: &str) -> bool;
pub fn quote(value: &str) -> String;
```

- [ ] **Step 1: Write unit tests for output, errors, and quoting**

Cover empty output, non-zero status including stderr, embedded single quotes, `#`, commas, `}`, tabs, and newlines. Use a temporary executable tmux stub selected through a test-only command-path constructor; do not mutate process-global `PATH` across parallel tests.

- [ ] **Step 2: Verify the tests fail**

Run: `cargo test tmux::tests`

Expected: FAIL because the public helpers do not exist.

- [ ] **Step 3: Implement the minimum synchronous adapter**

Use `std::process::Command`; keep the existing persistent control-mode `Tmux` unchanged for daemon scans. Return structured `TmuxError` on spawn/non-zero/UTF-8 failure. Do not add a generic command-builder abstraction.

- [ ] **Step 4: Run tests**

Run: `cargo test tmux::tests`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/tmux.rs src/main.rs
git commit -m "refactor: share tmux command handling"
```

### Task 3: Move Click and Wheel Behavior into Rust

**Files:**
- Create: `src/plugin.rs`
- Modify: `src/main.rs`
- Modify: `src/sidebar.rs`
- Modify: `scripts/click.sh`
- Modify: `scripts/scroll.sh`
- Test: `tests/plugin.rs`
- Test: `tests/navigation.sh`

**Interfaces:**
- Consumes: click pane/y/client, wheel pane/direction, daemon key FIFO, row-map files.
- Produces CLI commands:

```text
agents-mon click <pane-id> <mouse-y> <client-name>
agents-mon wheel <pane-id> <up|down>
```

```rust
pub fn click(pane: &str, y: usize, client: &str) -> i32;
pub fn wheel(pane: &str, direction: Direction) -> i32;
```

- [ ] **Step 1: Change characterization tests to call missing Rust commands**

Reroute only click/wheel test invocations from `bash scripts/*.sh` to `agents-mon click|wheel`. Keep assertions unchanged.

- [ ] **Step 2: Verify focused failures**

Run:

```bash
cargo test --test plugin stale_click_origin_is_a_noop -- --exact
cargo test --test plugin wheel_off_moves_without_jumping -- --exact
```

Expected: FAIL with CLI usage status 2.

- [ ] **Step 3: Implement click behavior**

Port all guards from `scripts/click.sh`: require the exact client, verify client and clicked pane still exist, select the correct shared/per-pane rows file, map the visual row, revalidate target pane, clear native navigation state, switch only the originating client, and enter the `agents-mon` key table on non-agent rows.

- [ ] **Step 4: Implement daemon-owned wheel debounce**

Send one logical `j`/`k` action immediately. Replace `${TMPDIR}/agents-mon-wheel`, token files, subshell, and `sleep` with one daemon timer: each wheel command writes a timestamp/generation and requested jump deadline through the existing FIFO; the daemon jumps only when the newest generation expires. Parse `@agents-mon-wheel-jump` exactly: empty = `0.3`, `off` = no jump, non-negative seconds = custom delay.

- [ ] **Step 5: Keep temporary compatibility wrappers**

Reduce scripts to:

```bash
#!/usr/bin/env bash
exec "${AGENTS_MON_BIN:-$(cd "$(dirname "$0")/.." && pwd)/target/release/agents-mon}" click "$@"
```

and the equivalent `wheel` wrapper. Live tmux servers may retain old script paths across plugin updates.

- [ ] **Step 6: Run tests**

Run:

```bash
cargo test
./tests/navigation.sh
./tests/run.sh
```

Expected: PASS; no `agents-mon-wheel` temp file is created.

- [ ] **Step 7: Commit**

```bash
git add src/main.rs src/plugin.rs src/sidebar.rs scripts/click.sh scripts/scroll.sh tests/plugin.rs tests/navigation.sh
git commit -m "feat: handle mouse navigation in Rust"
```

### Task 4: Move Processless Pane Lifecycle into Rust

**Files:**
- Modify: `src/plugin.rs`
- Modify: `src/main.rs`
- Modify: `src/sidebar.rs`
- Modify: `scripts/mirror-add.sh`
- Modify: `scripts/teardown.sh`
- Modify: `scripts/orphan.sh`
- Modify: `scripts/pin.sh`
- Modify: `scripts/restore.sh`
- Test: `tests/plugin.rs`
- Test: `tests/run.sh`
- Test: `tests/daemon-orphan.sh`

**Interfaces:**
- Produces CLI commands:

```text
agents-mon pane-add [window-id]
agents-mon pane-orphan
agents-mon pane-pin
agents-mon pane-restore [window-id]
agents-mon teardown
```

```rust
pub fn pane_add(window: Option<&str>) -> i32;
pub fn pane_orphan() -> i32;
pub fn pane_pin() -> i32;
pub fn pane_restore(window: Option<&str>) -> i32;
pub fn teardown() -> i32;
```

- [ ] **Step 1: Reroute lifecycle characterization tests to missing Rust commands**

Keep shell-wrapper tests once each for upgrade compatibility, but make the behavior matrix call the native commands.

- [ ] **Step 2: Verify failures**

Run: `cargo test --test plugin -- --test-threads=1`

Expected: lifecycle tests FAIL with CLI usage status 2.

- [ ] **Step 3: Port add/restore/pin as one transaction boundary**

Preserve: `@agents-mon-on` guard; `pi` session exclusion; `tmux wait-for -L/-U` lock; duplicate-title guard; saved `@agents-mon-layout-@N`; `split-window -I -hbf -d`; `pane_pid=0`; `allow-rename off`; title; width default 30; cleanup after split failure; exact window-size check before restore.

Use a Rust RAII guard to always run `wait-for -U`, including command errors and signal-free early returns. Do not invent a second lock.

- [ ] **Step 4: Port teardown and orphan recovery**

Preserve all current mirror-mode behavior: affect only `agents-mon` panes; relocate only clients stranded on the orphan window; ignore control-mode clients; prefer last window then another window/session; kill the orphan window; clear saved layout/winsize/on/control-client options; remain idempotent.

- [ ] **Step 5: Remove daemon script spawning**

Replace `src/sidebar.rs` calls to `scripts/teardown.sh` with `plugin::teardown()`. Keep failures best-effort where current shell ignores them.

- [ ] **Step 6: Reduce scripts to `exec agents-mon <command>` wrappers**

Retain filenames through the next release because previously installed hooks may still call them.

- [ ] **Step 7: Run lifecycle tests**

Run:

```bash
cargo test --test plugin -- --test-threads=1
./tests/daemon-orphan.sh
./tests/run.sh
```

Expected: PASS, including concurrent pane-add and size-mismatch restore.

- [ ] **Step 8: Commit**

```bash
git add src/main.rs src/plugin.rs src/sidebar.rs scripts/mirror-add.sh scripts/teardown.sh scripts/orphan.sh scripts/pin.sh scripts/restore.sh tests/plugin.rs tests/run.sh tests/daemon-orphan.sh
git commit -m "feat: manage sidebar panes in Rust"
```

### Task 5: Move Hook and Key-Table Installation into Rust

**Files:**
- Modify: `src/plugin.rs`
- Modify: `src/main.rs`
- Modify: `scripts/hooks.sh`
- Modify: `agents-mon.tmux`
- Test: `tests/plugin.rs`
- Test: `tests/navigation.sh`

**Interfaces:**
- Produces CLI command: `agents-mon setup`.
- `setup()` installs hook indexes `[42]`, `[43]`, `[44]`, key tables `agents-mon`/`agents-mon-search`, wheel bindings, nav contract version, mouse bindings, hidden-window picker, and status interpolation.

- [ ] **Step 1: Add a setup snapshot test**

On a private server, install custom root bindings, mouse on/off, status-left/right placeholders, popup key, hide-windows pattern, and tmux-version fixture. Run `agents-mon setup`, then assert semantic outputs from `tmux show-hooks`, `list-keys`, and `show-options`. Do not compare ordering or whitespace.

- [ ] **Step 2: Verify failure**

Run: `cargo test --test plugin setup_preserves_root_bindings_and_installs_plugin_tables -- --exact`

Expected: FAIL with CLI usage status 2.

- [ ] **Step 3: Port hook installation**

Move the generated key loop and hook command construction from `scripts/hooks.sh` into `plugin::setup()`. Preserve synchronous search/filter/text delivery, framed `text-XX` packets, root-table cloning, native wheel fallback, tmux `<3.2` follow fallback until Task 8, hook indexes, and `@agents-mon-nav-version`.

- [ ] **Step 4: Port entrypoint configuration that requires the binary**

Move config-reload hook recovery, mouse bindings, hide-window picker, and status placeholder replacement from `agents-mon.tmux` into `setup()`. Keep only pre-binary key/install/status bootstrap in `agents-mon.tmux`.

- [ ] **Step 5: Keep `hooks.sh` as one compatibility exec**

```bash
#!/usr/bin/env bash
exec "$(cd "$(dirname "$0")/.." && pwd)/target/release/agents-mon" setup
```

- [ ] **Step 6: Run tests**

Run:

```bash
cargo test --test plugin -- --test-threads=1
./tests/navigation.sh
./tests/run.sh
```

Expected: PASS with existing key semantics unchanged.

- [ ] **Step 7: Commit**

```bash
git add src/main.rs src/plugin.rs agents-mon.tmux scripts/hooks.sh tests/plugin.rs tests/navigation.sh
git commit -m "feat: install tmux integration from Rust"
```

### Task 6: Move Split and Popup Toggle into Rust

**Files:**
- Modify: `src/plugin.rs`
- Modify: `src/main.rs`
- Modify: `src/sidebar.rs`
- Modify: `scripts/toggle.sh`
- Modify: `agents-mon.tmux`
- Test: `tests/plugin.rs`
- Test: `tests/navigation.sh`
- Test: `tests/daemon-orphan.sh`

**Interfaces:**
- Produces CLI command: `agents-mon toggle [split|popup] [client-name]`.
- Consumes current options `@agents-mon-display`, `@agents-mon-width`, `@agents-mon-height`, control-client/on state, scan cache, popup pin/jump files.

- [ ] **Step 1: Add native toggle acceptance tests**

Cover first split open, repeated open, stale control client recovery, all-window pane creation, selected visual sidebar plus client key table, popup close, popup jump/reopen, calculated/fixed height, and killed-popup stale-pin cleanup.

- [ ] **Step 2: Verify failure**

Run: `cargo test --test plugin native_toggle_preserves_split_and_popup_behavior -- --exact`

Expected: FAIL with CLI usage status 2.

- [ ] **Step 3: Implement split toggle**

Port the Rust branch of `scripts/toggle.sh`: resolve/validate daemon control client, teardown crash leftovers, set `@agents-mon-on`, spawn detached `agents-mon daemon` with null stdio, add panes to every window, run setup, refresh nav contract, choose the exact/latest real client, select its sidebar pane, and switch only that client to the plugin table.

- [ ] **Step 4: Implement popup ownership loop**

Preserve pin toggling, stable popup owner, default width 40, minimum height 15, cache-based fleet sizing, client-height cap, `AGENTS_MON_PIN`, `AGENTS_MON_POPUP_CLIENT`, jump-file handoff, exact-client switch, reopen-after-jump, and stale-pin cleanup when the sidebar exits unexpectedly.

- [ ] **Step 5: Remove Rust-to-shell update/follow dependencies exposed by toggle**

For processless native mode, call Rust pane lifecycle directly. Leave legacy fallback branches untouched until Task 8.

- [ ] **Step 6: Reduce `toggle.sh` to compatibility dispatch**

If the binary exists, `exec agents-mon toggle "$@"`; otherwise run the legacy Bash fallback. This preserves old live bindings during the migration release.

- [ ] **Step 7: Run tests**

Run:

```bash
cargo test
./tests/navigation.sh
./tests/daemon-orphan.sh
./tests/run.sh
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add src/main.rs src/plugin.rs src/sidebar.rs agents-mon.tmux scripts/toggle.sh tests/plugin.rs tests/navigation.sh tests/daemon-orphan.sh
git commit -m "feat: toggle native views from Rust"
```

### Task 7: Move Release Refresh and Version Switching into Rust

**Files:**
- Create: `src/release.rs`
- Modify: `src/main.rs`
- Modify: `src/sidebar.rs`
- Modify: `scripts/install-bin.sh`
- Modify: `scripts/update.sh`
- Test: `tests/release.rs`
- Test: `tests/run.sh`

**Interfaces:**
- Produces CLI commands:

```text
agents-mon releases refresh
agents-mon update [latest|vX.Y.Z]
```

```rust
pub fn refresh(plugin_dir: &Path) -> i32;
pub fn update(plugin_dir: &Path, target: &str) -> i32;
```

- [ ] **Step 1: Port existing update tests to a Rust integration test**

Use a local bare Git remote, fixture tags, a fake verified package directory, and tmux stub. Cover latest resolution, explicit rollback, dirty-tree refusal, unknown tag, detached checkout, tarball copy, open-view restart, closed-view no-reopen, daemon shutdown wait, and source/binary version match.

- [ ] **Step 2: Verify failure**

Run: `cargo test --test release`

Expected: FAIL because update/release commands do not exist.

- [ ] **Step 3: Implement release metadata refresh**

Use `std::process::Command` with existing `curl` and `git`; preserve one-day throttling files, redirect-derived latest tag, semver-like tag ordering already used by the sidebar, atomic writes, and best-effort failure. Do not add HTTP or semver dependencies.

- [ ] **Step 4: Implement source switching and restart**

Port all safety rules from `scripts/update.sh`: validate `v[0-9]*`; no-op current version; dirty Git refusal; fetch/verify tag; detached checkout; tarball staging/copy only after verified fetch; clear install marker; install matching engine; teardown; wait up to 8 seconds for old control client; rerun setup; reopen only if previously open; display the same user messages.

The verified archive fetch remains callable through reduced `scripts/install-bin.sh fetch` until bootstrap is redesigned; Rust must never duplicate checksum rules.

- [ ] **Step 5: Call Rust directly from the sidebar**

Replace `src/sidebar.rs` spawns of `install-bin.sh refresh` and `update.sh` with `release::refresh()` and a detached invocation of the current binary's `update` command. Update must detach before replacing source/binary.

- [ ] **Step 6: Reduce `update.sh` to an exec wrapper**

Keep the public documented path for one compatibility release:

```bash
#!/usr/bin/env bash
exec "$(cd "$(dirname "$0")/.." && pwd)/target/release/agents-mon" update "${1:-latest}"
```

- [ ] **Step 7: Run tests**

Run:

```bash
cargo test --test release -- --test-threads=1
cargo test
./tests/run.sh
```

Expected: PASS; dirty trees remain untouched on failure.

- [ ] **Step 8: Commit**

```bash
git add src/main.rs src/release.rs src/sidebar.rs scripts/install-bin.sh scripts/update.sh tests/release.rs tests/run.sh
git commit -m "feat: switch plugin releases from Rust"
```

### Task 8: Make Native Bootstrap Reliable, Then Remove Runtime Duplication

**Files:**
- Modify: `agents-mon.tmux`
- Modify: `scripts/install-bin.sh`
- Delete: `scripts/scan.sh`
- Delete: `scripts/sidebar.sh`
- Delete: `scripts/client.sh`
- Delete: `scripts/follow.sh`
- Delete: `scripts/click.sh`
- Delete: `scripts/scroll.sh`
- Delete: `scripts/hooks.sh`
- Delete: `scripts/mirror-add.sh`
- Delete: `scripts/orphan.sh`
- Delete: `scripts/pin.sh`
- Delete: `scripts/restore.sh`
- Delete: `scripts/teardown.sh`
- Delete: `scripts/toggle.sh`
- Delete: `scripts/update.sh`
- Modify: `tests/run.sh`
- Modify: `tests/navigation.sh`
- Modify: `tests/sanity.sh`
- Modify: `README.md`
- Modify: `CONTRIBUTING.md`

**Interfaces:**
- `agents-mon.tmux` guarantees that key activation either runs a verified native binary or displays a clear install failure; it never invokes removed runtime scripts.
- Public runtime CLI is `agents-mon list|status|detect|sidebar|daemon|key|click|wheel|setup|toggle|pane-*|teardown|releases|update`.

- [ ] **Step 1: Add a clean-checkout bootstrap test**

In `tests/sanity.sh`, start from a package with no `target/release/agents-mon`, source `agents-mon.tmux`, trigger the toggle key immediately, and assert that installation completes and the same action opens the native sidebar. Run for both mocked verified-download and Cargo-fallback paths; assert checksum failure never executes or installs the staged file.

- [ ] **Step 2: Make `agents-mon.tmux` serialize first use with install**

Keep background eager install on source. Bind toggle through one bootstrap function/command that:

1. executes the binary immediately when present;
2. otherwise acquires one install lock;
3. runs `scripts/install-bin.sh`;
4. verifies the installed executable;
5. runs `agents-mon setup` and the originally requested toggle;
6. reports `agents-mon: native engine installation failed` through tmux on failure.

This intentionally replaces the old first-open Bash UI with a possibly delayed but behavior-equivalent native open.

- [ ] **Step 3: Prove release-platform bootstrap before deleting fallback**

Run the GitHub release matrix equivalent locally/CI for macOS ARM64/x86_64 and Linux ARM64/x86_64 archives. Verify SHA-256, executable bit, `--version`, clean-checkout immediate toggle, status interpolation, popup, split, and notification-helper presence where applicable.

Expected: all supported archives PASS. If any platform fails, stop here and retain the coherent `scan.sh`/`sidebar.sh` fallback; do not delete only part of it.

- [ ] **Step 4: Delete runtime wrappers and duplicated fallback**

Update every tmux hook/binding to call the binary directly before deleting compatibility paths. Remove Bash-specific branches from tests. Keep only:

```text
agents-mon.tmux
scripts/install-bin.sh
scripts/install-app.sh
scripts/version.sh
```

`agents-mon.tmux` remains because TPM needs a sourceable entrypoint; `install-bin.sh` remains because Rust cannot run before it exists; `install-app.sh` remains platform packaging; `version.sh` remains pre-binary release/CI validation.

- [ ] **Step 5: Update docs and contributor model**

Document Rust as the only runtime engine, the first-use install wait/failure message, the four remaining shell files and why each remains, native CLI commands, release switching, and how to run Rust/private-tmux tests. Remove Bash fallback requirements and parity instructions.

- [ ] **Step 6: Run the full gate**

Run:

```bash
cargo fmt --check
cargo test
./tests/run.sh
./tests/navigation.sh
./tests/daemon-orphan.sh
./tests/sanity.sh
rg -n 'scripts/(scan|sidebar|client|follow|click|scroll|hooks|mirror-add|orphan|pin|restore|teardown|toggle|update)\.sh' --glob '!docs/superpowers/plans/**' .
```

Expected: every test PASS; final `rg` returns no matches.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "refactor: make Rust the sole plugin runtime"
```

## Logic-Preservation Matrix

| Existing behavior | Preserved by |
|---|---|
| Agent config/detection/subject rules and TSV/status output | Existing `conf.rs`, `detect.rs`, `procs.rs`, `scan.rs`; `tests/parity.rs` |
| Idle debounce, done state, attention and notifications | Existing `attention.rs` and sidebar tests |
| Search/filter/key decoding/help/version picker | Existing `sidebar.rs` tests plus `tests/navigation.sh` |
| Exact-client click and stale-origin safety | Task 3 native click tests |
| Wheel one-row movement and settle-to-jump | Task 3 daemon timer tests |
| Processless pane creation and width pinning | Task 4 private-tmux tests |
| Layout snapshots and size-safe restore | Tasks 1/4 restore regression test |
| Concurrent hook pane-add race | Tasks 1/4 concurrent idempotence test |
| Stranded-client orphan recovery | Task 4 plus `tests/daemon-orphan.sh` |
| Hook/key-table semantics and config reload | Task 5 setup snapshot/navigation tests |
| Split and popup lifecycle | Task 6 native toggle tests |
| Dirty-tree update refusal and rollback | Task 7 release tests |
| First-use availability without Bash runtime | Task 8 clean-checkout bootstrap gate |
| macOS notification bundle | Existing `install-app.sh` and sanity/release package checks |

## Deliberately Retained Shell Boundary

- `agents-mon.tmux`: TPM/tmux loads shell before the binary can exist.
- `scripts/install-bin.sh`: downloads/verifies/builds the binary that would otherwise be needed to run an installer.
- `scripts/install-app.sh`: thin macOS bundle/codesign packaging around platform commands.
- `scripts/version.sh`: validates release tags from `Cargo.toml` in CI and pre-binary bootstrap environments.

Converting these four files to Rust either creates a bootstrap cycle or merely wraps platform commands without removing a runtime dependency. Stop after Task 8 unless packaging changes make one of them unnecessary.

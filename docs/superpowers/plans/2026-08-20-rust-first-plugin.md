# Rust-First Plugin Migration Implementation Plan

> **For agentic workers:** use the `subagent-driven-development` skill to
> implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for
> tracking.

**Goal:** Make Rust the sole plugin runtime while preserving the current tmux UI, navigation, pane lifecycle, update/rollback, detection, and notification behavior.

**Architecture:** Prove native installation first, then immediately delete the duplicated Bash scanner/sidebar. Port the remaining shell adapters behind focused Rust modules: `input` for mouse events, `panes` for processless pane lifecycle, `setup` for hooks/key tables, `toggle` for split/popup ownership, and `release` for updates. Retain only shell that must run before Rust exists or packages a macOS app.

**Tech Stack:** Rust 2021, existing `regex`/`libc`/`sysinfo` dependencies, tmux control mode and CLI, Cargo tests, isolated tmux servers, existing release smoke tests.

## Global Constraints

- Preserve tmux options, hook indexes, key tables, pane title `agents-mon`, temp-file names, TSV output, public CLI output, and exit codes unless a task explicitly replaces an internal contract.
- Preserve split and popup behavior, mouse behavior, search/filter/navigation, layout restoration, multi-client targeting, processless panes, update/rollback, notifications, agent overrides, and status output.
- Keep exact originating-client checks for mouse actions; never infer another client after a background delay.
- Restore saved layouts only when their embedded size matches the current window size.
- Keep `agents-mon.tmux` and `scripts/install-bin.sh` usable before a Rust binary exists.
- Do not add an HTTP client, async runtime, command-builder framework, or semver dependency. Continue using installed `curl`, `tar`, `git`, and platform tools where needed.
- Do not delete the Bash scanner/sidebar until Task 2's native bootstrap gate passes on every supported release platform.
- After Task 2 passes, the Bash runtime fallback is intentionally gone; compatibility wrappers may call Rust unconditionally until their callers are migrated.
- Preserve public CLI commands `scan` (alias of `list`) and `notification-open`; delete only the unused `mirror` command.
- Each task must pass `cargo test` and its named isolated-tmux checks before commit.

## Target File Structure

- `src/main.rs` — CLI dispatch only.
- `src/tmux.rs` — persistent control-mode transport plus small synchronous tmux helpers.
- `src/input.rs` — click validation/row lookup and wheel input.
- `src/panes.rs` — processless pane add, orphan recovery, width pinning, layout restoration, teardown, and real-client selection shared by input/toggle.
- `src/setup.rs` — tmux hooks, key tables, mouse bindings, picker filtering, and status interpolation.
- `src/toggle.rs` — split daemon startup and popup ownership loop.
- `src/release.rs` — release discovery and update/rollback coordination.
- `src/sidebar.rs` — existing sidebar/daemon state and one in-memory wheel jump deadline; no shell spawning.
- `tests/plugin.rs` — Rust integration tests against private tmux servers.
- `tests/release.rs` — update/rollback tests using local repositories and command fixtures.
- `agents-mon.tmux` — minimal TPM/pre-binary bootstrap.
- `scripts/install-bin.sh` — pre-binary platform selection, verified download, atomic install, and Cargo fallback.
- `scripts/install-app.sh` — macOS app bundle and codesign packaging.
- `scripts/version.sh` — pre-binary manifest/tag validation for bootstrap and CI.

---

### Task 1: Freeze Current Behavior

**Files:**
- Create: `tests/plugin.rs`

**Interfaces:**
- Consumes: current shell entrypoints and `agents-mon` binary.
- Produces: private-tmux helpers and characterization tests reused unchanged as shell calls move to Rust.

- [ ] **Step 1: Create private tmux test helpers**

```rust
struct TestTmux {
    socket: String,
    tmp: std::path::PathBuf,
}

impl TestTmux {
    fn new(name: &str) -> Self;
    fn tmux(&self, args: &[&str]) -> std::process::Output;
    fn script(&self, name: &str, args: &[&str]) -> std::process::Output;
    fn bin(&self, args: &[&str]) -> std::process::Output;
}

impl Drop for TestTmux {
    fn drop(&mut self);
}
```

Use `CARGO_BIN_EXE_agents-mon`, a unique `tmux -L` socket, `TMPDIR`, and `AGENTS_MON_DIR`. Kill the private server in `Drop`.

- [ ] **Step 2: Add the missing characterization tests**

```rust
#[test] fn restore_skips_a_layout_after_window_size_changes();
#[test] fn mirror_add_is_idempotent_under_concurrent_calls();
#[test] fn wheel_off_moves_without_jumping();
#[test] fn wheel_custom_delay_jumps_only_once();
#[test] fn newest_non_control_client_wins();
#[test] fn stale_click_origin_is_a_noop();
```

Invoke current scripts. Assert observable tmux state—pane count/title/width, selected pane/window, and client key table—not generated command strings.

- [ ] **Step 3: Run the characterization tests**

```bash
cargo test --test plugin -- --test-threads=1
./tests/navigation.sh
./tests/run.sh
```

Expected: PASS on the current implementation.

- [ ] **Step 4: Commit**

```bash
git add tests/plugin.rs
git commit -m "test: freeze plugin shell behavior"
```

### Task 2: Prove Native Bootstrap and Delete the Duplicated Runtime First

**Files:**
- Modify: `agents-mon.tmux`
- Modify: `scripts/install-bin.sh`
- Modify: `scripts/toggle.sh`
- Modify: `scripts/click.sh`
- Modify: `scripts/hooks.sh`
- Modify: `scripts/orphan.sh`
- Delete: `scripts/scan.sh`
- Delete: `scripts/sidebar.sh`
- Delete: `scripts/follow.sh`
- Delete: `scripts/restore.sh`
- Delete: `src/mirror.rs`
- Modify: `src/main.rs`
- Modify: `src/sidebar.rs`
- Modify: `tests/run.sh`
- Modify: `tests/navigation.sh`
- Modify: `tests/sanity.sh`
- Modify: `README.md`
- Modify: `CONTRIBUTING.md`

**Interfaces:**
- `agents-mon.tmux` guarantees that activation either runs a verified native binary or displays a clear install failure.
- Existing public commands remain: `--version`, `scan`, `list`, `status`, `detect`, `sidebar`, `daemon`, `key`, and `notification-open`.
- The unused `mirror` command is removed.

- [ ] **Step 1: Add clean-checkout bootstrap tests**

In `tests/sanity.sh`, start from a plugin package with no `target/release/agents-mon`, source `agents-mon.tmux`, trigger split and popup immediately, and assert that installation finishes and the requested native view opens. Cover a mocked verified-download path and Cargo fallback. Assert checksum failure neither executes nor installs the staged binary and displays:

```text
agents-mon: native engine installation failed
```

- [ ] **Step 2: Serialize first activation with installation**

Keep background eager installation. Bind first activation through minimal bootstrap shell that:

1. executes the binary immediately when present;
2. otherwise acquires one `tmux wait-for` install lock;
3. runs `scripts/install-bin.sh`;
4. verifies the installed executable;
5. re-enters `agents-mon.tmux` so the installed version owns bindings/hooks;
6. invokes the current tree's `scripts/toggle.sh` with the original split/popup arguments (the existing script already selects the Rust daemon/sidebar once the binary exists; native `agents-mon toggle` is added later in Task 7);
7. reports the failure message through tmux and exits non-zero on failure.

Do not create a second installer.

- [ ] **Step 3: Run the release-platform gate**

Run the GitHub release matrix for macOS ARM64/x86_64 and Linux ARM64/x86_64. Verify SHA-256, executable bit, `--version`, clean-checkout immediate split/popup, status interpolation, and notification-helper presence where applicable.

Expected: every supported archive PASS. If one fails, stop this task and retain all fallback files.

- [ ] **Step 4: Remove the moving-sidebar fallback as one coherent unit**

After the gate passes:

- delete `scan.sh` and `sidebar.sh`;
- remove no-binary and single-sidebar branches from `toggle.sh`, `click.sh`, `hooks.sh`, and `orphan.sh`;
- delete `follow.sh` and `restore.sh`;
- remove the no-op `follow.sh` spawn from `Sidebar::jump` (popup returns before it; processless native panes do not relocate);
- remove the **entire** moving-sidebar follow hook from `hooks.sh` — both branches of
  `after-select-window[42]`, `client-session-changed[42]`, and `session-window-changed[42]`.
  The tmux `>=3.2` native-join branch is the one that spawns `restore.sh` (`hooks.sh:17`);
  the `<3.2` branch is the one that spawns `follow.sh`. Both are guarded on
  `@agents-mon-sidebar`, which native mode never sets, so both are dead — but deleting
  either script while keeping its branch leaves live tmux servers holding a hook that
  spawns a missing file across the upgrade;
- change status fallback to install-or-empty rather than invoke `scan.sh`.

- [ ] **Step 5: Delete the dead mirror process**

Remove `mod mirror`, `src/mirror.rs`, and only the `main.rs` `mirror` arm. Keep both:

```rust
["scan"] | ["list"] => cmd_scan(),
["notification-open", socket, pane, bundle] => notifications::open_pane(socket, pane, bundle),
```

Update usage text and tests accordingly.

- [ ] **Step 6: Delete parity tests; reroute behavior tests that merely used the Bash engine**

Delete shell-fixture comparisons whose only purpose was dual-engine parity — notably the
`scan.sh detect` fixture loop at `tests/run.sh:28`. Keep `tests/parity.rs`, which already runs
`agents-mon detect` against every fixture (no Bash involved), plus navigation, lifecycle,
and CLI tests.

Do **not** delete the two popup-pin tests at `tests/run.sh:440-490`
(`popup-sidebar-signal-removes-pin`, `popup-sidebar-ctrl-d-removes-pin`). They are behavior
tests, not parity tests: they assert that SIGTERM and Ctrl-D each clear `AGENTS_MON_PIN`
without creating a real pane. Reroute them from `bash scripts/sidebar.sh` to
`agents-mon sidebar` — the Rust engine honors the same `AGENTS_MON_PIN` contract, so the
tmux stub, env, and assertions stay as they are. Without this, pin-cleanup has no coverage
until Task 7 adds it.

- [ ] **Step 7: Run the gate again after deletion**

```bash
cargo test
./tests/run.sh
./tests/navigation.sh
./tests/daemon-orphan.sh
./tests/sanity.sh
rg -n 'scripts/(scan|sidebar|follow|restore)\.sh|agents-mon mirror' --glob '!docs/**' .
```

Expected: tests PASS; `rg` returns no matches.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "refactor: remove duplicated Bash runtime"
```

### Task 3: Add Shared Synchronous tmux Helpers

**Files:**
- Modify: `src/tmux.rs`
- Modify: `src/sidebar.rs`
- Test: `src/tmux.rs`
- Test: `tests/navigation.sh`

**Interfaces:**

```rust
pub fn command(args: &[&str]) -> Result<String, TmuxError>;
pub fn command_status(args: &[&str]) -> Result<(), TmuxError>;
pub fn lines(args: &[&str]) -> Result<Vec<String>, TmuxError>;
pub fn format_truth(value: &str) -> bool;
pub fn quote(value: &str) -> String;
```

- [ ] **Step 1: Write pure unit tests**

Test `quote()` and `format_truth()` directly for empty strings, embedded single quotes, `#`, commas, `}`, tabs, newlines, and tmux truth values. Do not add command-path injection or mutate global `PATH`.

- [ ] **Step 2: Verify unit-test failure**

Run: `cargo test tmux::tests`

Expected: FAIL because the pure helpers do not exist.

- [ ] **Step 3: Implement the minimum adapter**

Use `std::process::Command::new("tmux")`; return `TmuxError` on spawn, non-zero status, or invalid UTF-8. Keep the existing persistent control-mode `Tmux` unchanged. Do not add a builder or injectable global.

- [ ] **Step 4: Route existing sidebar tmux subprocesses through the adapter**

Replace the existing direct `Command::new("tmux")` call sites in `src/sidebar.rs` with `command`/`command_status` where their argv and best-effort behavior are unchanged. This provides real execution coverage through the existing private-server navigation tests without an injectable command path.

- [ ] **Step 5: Run tests**

```bash
cargo test tmux::tests
./tests/navigation.sh
cargo test
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/tmux.rs src/sidebar.rs tests/navigation.sh
git commit -m "refactor: share tmux command handling"
```

### Task 4: Move Click and Wheel Input into Rust

**Files:**
- Create: `src/input.rs`
- Modify: `src/main.rs`
- Modify: `src/sidebar.rs`
- Modify: `scripts/click.sh`
- Modify: `scripts/scroll.sh`
- Modify: `tests/plugin.rs`
- Modify: `tests/navigation.sh`

**Interfaces:**

```text
agents-mon click <pane-id> <mouse-y> <client-name>
agents-mon wheel <pane-id> <up|down>
```

```rust
pub fn click(pane: &str, y: usize, client: &str) -> i32;
pub fn wheel(pane: &str, direction: Direction) -> i32;
```

- [ ] **Step 1: Reroute click/wheel tests to missing Rust commands**

Keep assertions unchanged. Retain one wrapper smoke test per script until Task 9.

- [ ] **Step 2: Verify focused failures**

```bash
cargo test --test plugin stale_click_origin_is_a_noop -- --exact
cargo test --test plugin wheel_off_moves_without_jumping -- --exact
```

Expected: FAIL with usage status 2.

- [ ] **Step 3: Port click behavior**

Preserve every `click.sh` guard: exact non-empty client, live client, live clicked pane, shared rows map in native mode, visual row lookup, target-pane revalidation, filter clear, exact-client switch, and non-agent-row entry into `agents-mon` key table. Do not call `follow.sh`; native processless panes already exist in each window.

- [ ] **Step 4: Add two reserved wheel bytes to the existing FIFO protocol**

Map `wheel-up` and `wheel-down` in `send_key` to unused bytes `0x01` and `0x02`. Decode them as `Key::WheelUp` and `Key::WheelDown`. In the existing event loop:

- move selection immediately as `k`/`j` would;
- parse `@agents-mon-wheel-jump` as empty = 300 ms, `off` = no jump, non-negative seconds = custom delay;
- set one `wheel_jump_at: Option<Instant>` after each tick, overwriting the prior deadline;
- include that deadline in the existing poll timeout;
- call `jump()` once when the latest deadline expires.

No timestamp packet, generation counter, temp token, subprocess, or `sleep` is needed: overwriting one deadline implements last-tick-wins.

- [ ] **Step 5: Reduce scripts to compatibility execs**

Because Task 2 removed the no-binary fallback, `click.sh` and `scroll.sh` may now unconditionally execute `agents-mon click|wheel` until Task 9 removes them.

- [ ] **Step 6: Run tests**

```bash
cargo test
./tests/navigation.sh
./tests/run.sh
```

Expected: PASS; no `${TMPDIR}/agents-mon-wheel` file is created.

- [ ] **Step 7: Commit**

```bash
git add src/input.rs src/main.rs src/sidebar.rs scripts/click.sh scripts/scroll.sh tests/plugin.rs tests/navigation.sh
git commit -m "feat: handle mouse input in Rust"
```

### Task 5: Move Processless Pane Lifecycle into Rust

**Files:**
- Create: `src/panes.rs`
- Modify: `src/main.rs`
- Modify: `src/sidebar.rs`
- Delete: `scripts/client.sh`
- Modify: `scripts/mirror-add.sh`
- Modify: `scripts/teardown.sh`
- Modify: `scripts/orphan.sh`
- Modify: `scripts/pin.sh`
- Modify: `tests/plugin.rs`
- Modify: `tests/run.sh`
- Modify: `tests/daemon-orphan.sh`

**Interfaces:**

```text
agents-mon pane-add [window-id]
agents-mon pane-orphan
agents-mon pane-pin
agents-mon teardown
```

```rust
pub fn newest_real_client(format: &str) -> Result<Option<String>, TmuxError>;
pub fn pane_add(window: Option<&str>) -> i32;
pub fn pane_orphan() -> i32;
pub fn pane_pin() -> i32;
pub fn teardown() -> i32;
```

- [ ] **Step 1: Reroute lifecycle characterization tests to missing Rust commands**

Keep one wrapper smoke test per remaining script until Task 9.

- [ ] **Step 2: Verify failure**

Run: `cargo test --test plugin -- --test-threads=1`

Expected: lifecycle tests FAIL with usage status 2.

- [ ] **Step 3: Port client selection and pane add**

Preserve newest non-control-client ordering, `@agents-mon-on`, `pi` session exclusion, `tmux wait-for -L/-U`, duplicate-title guard, saved `@agents-mon-layout-@N`, `split-window -I -hbf -d`, `pane_pid=0`, `allow-rename off`, title, width default 30, and split-failure cleanup.

Use one Rust RAII lock guard that always runs `wait-for -U`. Do not introduce another lock.

- [ ] **Step 4: Port pin, orphan recovery, and teardown**

Preserve native behavior only: affect `agents-mon` panes; relocate only clients stranded on an orphan window; ignore control clients; prefer last window then another window/session; kill orphan window; resize every plugin pane; restore saved layout only on exact size match; clear layout/winsize/on/control-client options; remain idempotent.

- [ ] **Step 5: Remove daemon shell spawning**

Replace `src/sidebar.rs` teardown-script execution with `panes::teardown()`. Preserve existing best-effort error handling.

- [ ] **Step 6: Reduce scripts to compatibility execs**

Map each filename to its native command. `client.sh` gets no wrapper and no compatibility
command — delete it here. Its only callers were `sidebar.sh` (gone in Task 2) and
`toggle.sh` (an exec wrapper after Task 7); no tmux binding or hook ever invoked it
directly, so nothing outside the tree can call it. `newest_real_client()` stays internal.

- [ ] **Step 7: Run lifecycle tests**

```bash
cargo test --test plugin -- --test-threads=1
./tests/daemon-orphan.sh
./tests/run.sh
```

Expected: PASS, including concurrent add and size-mismatch restoration.

- [ ] **Step 8: Commit**

```bash
git add src/panes.rs src/main.rs src/sidebar.rs scripts/client.sh scripts/mirror-add.sh scripts/teardown.sh scripts/orphan.sh scripts/pin.sh tests/plugin.rs tests/run.sh tests/daemon-orphan.sh
git commit -m "feat: manage sidebar panes in Rust"
```

### Task 6: Move Hook and Key-Table Setup into Rust

**Files:**
- Create: `src/setup.rs`
- Modify: `src/main.rs`
- Modify: `agents-mon.tmux`
- Modify: `scripts/hooks.sh`
- Modify: `tests/plugin.rs`
- Modify: `tests/navigation.sh`

**Interfaces:**
- Produces `agents-mon setup`.
- Installs hook indexes `[42]`, `[43]`, `[44]`, key tables `agents-mon`/`agents-mon-search`, mouse bindings, hidden-window picker, status interpolation, and nav version.

- [ ] **Step 1: Add semantic setup tests**

On a private server, install custom root bindings, mouse on/off, status placeholders, popup key, and hide-window pattern. Run `agents-mon setup`; assert semantic output from `show-hooks`, `list-keys`, and `show-options`, ignoring ordering/whitespace.

- [ ] **Step 2: Verify failure**

Run: `cargo test --test plugin setup_preserves_root_bindings_and_installs_plugin_tables -- --exact`

Expected: FAIL with usage status 2.

- [ ] **Step 3: Port setup**

Move hook/key construction from `hooks.sh` into `setup::run()`. Preserve synchronous search/filter/text delivery, `text-XX` packets, root-table cloning, native wheel fallback, hook indexes, and `@agents-mon-nav-version`. Do not port the deleted moving-sidebar follow hooks.

- [ ] **Step 4: Port binary-backed entrypoint configuration**

Move config-reload recovery, mouse bindings, hide-window picker, and status replacement from `agents-mon.tmux` into `setup`. Keep only pre-binary install/activation glue in `agents-mon.tmux`.

- [ ] **Step 5: Reduce `hooks.sh` to `exec agents-mon setup`**

No binary guard is needed after Task 2; bootstrap owns installation before runtime setup.

- [ ] **Step 6: Run tests**

```bash
cargo test --test plugin -- --test-threads=1
./tests/navigation.sh
./tests/run.sh
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/setup.rs src/main.rs agents-mon.tmux scripts/hooks.sh tests/plugin.rs tests/navigation.sh
git commit -m "feat: install tmux integration from Rust"
```

### Task 7: Move Split and Popup Toggle into Rust

**Files:**
- Create: `src/toggle.rs`
- Modify: `src/main.rs`
- Modify: `src/sidebar.rs`
- Modify: `scripts/toggle.sh`
- Modify: `agents-mon.tmux`
- Modify: `tests/plugin.rs`
- Modify: `tests/navigation.sh`
- Modify: `tests/daemon-orphan.sh`

**Interfaces:**
- Produces `agents-mon toggle [split|popup] [client-name]`.
- Consumes `@agents-mon-display`, width/height options, control-client/on state, scan cache, and popup pin/jump files.

- [ ] **Step 1: Add native toggle tests**

Cover first split, repeated split, stale control client, all-window panes, selected visual sidebar plus client key table, popup close, popup jump/reopen, calculated/fixed height, and killed-popup pin cleanup.

- [ ] **Step 2: Verify failure**

Run: `cargo test --test plugin native_toggle_preserves_split_and_popup_behavior -- --exact`

Expected: FAIL with usage status 2.

- [ ] **Step 3: Port split toggle**

Resolve/validate daemon control client, teardown crash leftovers, set `@agents-mon-on`, spawn detached `agents-mon daemon` with null stdio, add panes to every window through `panes`, call `setup`, refresh nav version, choose the exact/latest real client, select its sidebar pane, and switch only that client to the plugin table.

- [ ] **Step 4: Port popup ownership**

Preserve pin toggling, stable owner, width 40, height floor 15, cache-based fleet sizing, client-height cap, `AGENTS_MON_PIN`, `AGENTS_MON_POPUP_CLIENT`, jump-file handoff, exact-client switch, reopen-after-jump, and stale-pin cleanup.

- [ ] **Step 5: Reduce `toggle.sh` to `exec agents-mon toggle`**

No fallback branch remains after Task 2.

- [ ] **Step 6: Run tests**

```bash
cargo test
./tests/navigation.sh
./tests/daemon-orphan.sh
./tests/run.sh
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add src/toggle.rs src/main.rs src/sidebar.rs agents-mon.tmux scripts/toggle.sh tests/plugin.rs tests/navigation.sh tests/daemon-orphan.sh
git commit -m "feat: toggle native views from Rust"
```

### Task 8: Move Release Refresh and Version Switching into Rust

**Files:**
- Create: `src/release.rs`
- Modify: `src/main.rs`
- Modify: `src/sidebar.rs`
- Modify: `scripts/install-bin.sh`
- Modify: `scripts/update.sh`
- Create: `tests/release.rs`
- Modify: `tests/run.sh`

**Interfaces:**

```text
agents-mon releases refresh
agents-mon update [latest|vX.Y.Z]
```

```rust
pub fn refresh(plugin_dir: &Path) -> i32;
pub fn update(plugin_dir: &Path, target: &str) -> i32;
```

- [ ] **Step 1: Port update tests to Rust integration tests**

Use a local bare Git remote, fixture tags, fake verified package, and tmux stub. Cover latest, explicit rollback, dirty-tree refusal, unknown tag, detached checkout, tarball copy, open restart, closed no-reopen, daemon shutdown wait, and source/binary version match.

- [ ] **Step 2: Verify failure**

Run: `cargo test --test release`

Expected: FAIL because update/release commands do not exist.

- [ ] **Step 3: Implement release metadata refresh**

Use existing `curl` and `git`; preserve one-day throttle files, redirect-derived latest tag, current numeric tag ordering, atomic writes, and best-effort failure. Add no dependency.

- [ ] **Step 4: Implement safe source switching**

Preserve `update.sh`: validate `v[0-9]*`; no-op current version; refuse dirty Git; fetch/verify tag; detached checkout; tarball stage/copy only after verified fetch; clear install marker; install matching engine; teardown; wait up to 8 seconds for old control client; preserve messages.

After the target source and binary are installed, always re-enter through the **target tree's** public entrypoint:

```text
bash <target-dir>/agents-mon.tmux
```

If the view was open, reopen with the target's own mechanism:

1. if `<target-dir>/scripts/toggle.sh` exists, run it (required when rolling back to pre-migration tags whose binary has no `toggle` command);
2. otherwise run `<target-dir>/target/release/agents-mon toggle`.

Never assume the binary at the rollback target supports the new `setup` or `toggle` commands.

- [ ] **Step 5: Keep one verified-fetch primitive**

Continue calling reduced `scripts/install-bin.sh fetch` for archive download/checksum/extraction. Rust must not duplicate checksum policy.

- [ ] **Step 6: Call Rust from the sidebar**

Replace `install-bin.sh refresh` and `update.sh` spawns with `release::refresh()` and a detached current-binary `update`. Detach before replacing source/binary.

- [ ] **Step 7: Reduce `update.sh` to a compatibility exec**

Task 9 removes it after all callers use Rust.

- [ ] **Step 8: Run tests**

```bash
cargo test --test release -- --test-threads=1
cargo test
./tests/run.sh
```

Expected: PASS, including rollback to a fixture tag without native `setup`/`toggle`; dirty trees remain untouched.

- [ ] **Step 9: Commit**

```bash
git add src/release.rs src/main.rs src/sidebar.rs scripts/install-bin.sh scripts/update.sh tests/release.rs tests/run.sh
git commit -m "feat: switch plugin releases from Rust"
```

### Task 9: Remove Runtime Compatibility Wrappers

**Files:**
- Modify: `agents-mon.tmux`
- Delete: `scripts/click.sh`
- Delete: `scripts/scroll.sh`
- Delete: `scripts/hooks.sh`
- Delete: `scripts/mirror-add.sh`
- Delete: `scripts/orphan.sh`
- Delete: `scripts/pin.sh`
- Delete: `scripts/teardown.sh`
- Delete: `scripts/toggle.sh`
- Delete: `scripts/update.sh`
- Modify: `tests/run.sh`
- Modify: `tests/navigation.sh`
- Modify: `tests/sanity.sh`
- Modify: `README.md`
- Modify: `CONTRIBUTING.md`

**Interfaces:**
- Final public runtime CLI:

```text
agents-mon --version
agents-mon scan|list|status
agents-mon detect <conf> <screen-file> [title]
agents-mon sidebar|daemon
agents-mon key <name>
agents-mon click <pane> <row> <client>
agents-mon wheel <pane> <up|down>
agents-mon setup
agents-mon toggle [split|popup] [client]
agents-mon pane-add [window]|pane-orphan|pane-pin|teardown
agents-mon releases refresh
agents-mon update [latest|vX.Y.Z]
agents-mon notification-open <socket> <pane> <bundle>
```

- [ ] **Step 1: Point every hook, binding, and internal call directly at Rust**

Run `rg` before deletion. Update all active paths and update tests to invoke native commands. Ensure update/rollback retains the Task 8 old-tag `scripts/toggle.sh` existence check even though the current tree deletes that script.

- [ ] **Step 2: Delete wrappers**

Keep only:

```text
agents-mon.tmux
scripts/install-bin.sh
scripts/install-app.sh
scripts/version.sh
```

- [ ] **Step 3: Update docs**

Document Rust-only runtime, first-use installation wait/failure, retained bootstrap/package scripts, complete CLI, rollback compatibility, and private-tmux tests. Remove Bash fallback and parity instructions.

- [ ] **Step 4: Run the full gate**

```bash
cargo fmt --check
cargo test
./tests/run.sh
./tests/navigation.sh
./tests/daemon-orphan.sh
./tests/sanity.sh
rg -n 'scripts/(scan|sidebar|client|follow|click|scroll|hooks|mirror-add|orphan|pin|restore|teardown|update)\.sh|agents-mon mirror' --glob '!docs/superpowers/plans/**' .
rg -n 'scripts/toggle\.sh' --glob '!docs/superpowers/plans/**' .
```

Expected: every test PASS; the first `rg` returns no matches. The second returns exactly one intentional production reference in `src/release.rs`, used to reopen pre-migration rollback targets through their own entrypoint; tests may contain fixture strings for the same contract.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor: make Rust the sole plugin runtime"
```

## Logic-Preservation Matrix

| Existing behavior | Preserved by |
|---|---|
| Agent config/detection/subject rules and TSV/status | Existing Rust modules and `tests/parity.rs` |
| Idle debounce, done state, attention, notifications | Existing `attention.rs` and sidebar tests |
| Search/filter/key decoding/help/version picker | Existing sidebar tests and `tests/navigation.sh` |
| First-use operation without preinstalled binary | Task 2 bootstrap/platform gate |
| Exact-client click and stale-origin safety | Task 4 input tests |
| Wheel move and settle-to-jump | Task 4 reserved-byte/deadline tests |
| Processless panes, widths, and add race | Task 5 private-tmux tests |
| Size-safe layout restoration | Tasks 1/5 regression test |
| Stranded-client orphan recovery | Task 5 and `tests/daemon-orphan.sh` |
| Hook/key-table semantics and reload | Task 6 setup tests |
| Split and popup lifecycle | Task 7 native toggle tests |
| Dirty-tree refusal and old-tag rollback | Task 8 release tests |
| `scan` alias and notification click entrypoint | Tasks 2/9 CLI tests |
| macOS notification bundle | Retained `install-app.sh` and package checks |

## Deliberately Retained Shell Boundary

- `agents-mon.tmux`: TPM loads shell before the binary can exist.
- `scripts/install-bin.sh`: installs/verifies/builds the Rust binary that cannot install itself before it exists.
- `scripts/install-app.sh`: macOS bundle/codesign packaging around platform commands.
- `scripts/version.sh`: pre-binary manifest/tag validation for bootstrap and CI.

Converting these four files creates a bootstrap cycle or merely rewrites platform command invocation. Stop after Task 9 unless the packaging model changes.

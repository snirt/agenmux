# Agenmux Canonical Rename Implementation Plan

> **For agentic workers:** execute tasks in order. This migration is intentionally incremental because runtime, packaging, and compatibility contracts are coupled.

**Goal:** Make `agenmux` the canonical product, binary, tmux, configuration, package, and notification-app name while preserving one release cycle of compatibility for existing `agents-mon` installations.

**Architecture:** Canonical code writes only Agenmux names. Reads and entrypoints accept legacy names as lower-priority fallbacks. Migration is successful before legacy hooks, panes, app files, or state are removed.

**Tech Stack:** Rust, Bash, tmux, Cargo, GitHub Actions, macOS app bundles.

## Global Constraints

- Canonical names: `agenmux`, `Agenmux`, `AGENMUX_*`, `@agenmux-*`, `#{agenmux}`, `~/.config/agenmux`, `snirt/agenmux`.
- Legacy names remain compatibility inputs or forwarding entrypoints for one release cycle; never remain canonical outputs.
- Canonical values win whenever canonical and legacy values both exist.
- Preserve dated files under `docs/plans/` and captured files under `tests/fixtures/` unless an executable test contract requires a change.
- Rename the macOS bundle ID to `io.github.snirt.agenmux`; a new permission prompt is expected.
- Install `Agenmux.app` completely before removing `AgentsMon.app`.
- Never commit local captures, paths, prompts, sessions, credentials, or private identifiers.
- Use mise-managed latest Rust for builds and tests.

---

### Task 1: Canonicalize Runtime Contracts

**Files:**

- Modify: `Cargo.toml`, `Cargo.lock`
- Create: `src/bin/agenmux-notifier.rs`, `src/bin/agenmux-notifier/broker.rs`
- Rename/remove after parity: `src/bin/agents-mon-notifier.rs`, `src/bin/agents-mon-notifier/broker.rs`
- Modify: `src/main.rs`, `src/conf.rs`, `src/setup.rs`, `src/panes.rs`, `src/sidebar.rs`, `src/toggle.rs`, `src/input.rs`, `src/scan.rs`, `src/focus.rs`, `src/notifications.rs`, `src/notifications/macos.rs`, `src/release.rs`, `src/tmux.rs`, `src/attention.rs`

**Interfaces:**

- Primary executable: `target/{debug,release}/agenmux`.
- Primary notifier: `target/{debug,release}/agenmux-notifier`.
- Legacy executable compatibility is supplied by Task 2 wrappers, not duplicate Rust implementations.
- Canonical pane title/key table/cache/status/option names use `agenmux`; teardown and cleanup recognize both pane titles.
- Option lookup returns `@agenmux-*` first, then the equivalent `@agents-mon-*`.
- Environment lookup returns `AGENMUX_*` first, then `AGENTS_MON_*`.
- Config precedence: built-in `<plugin>/agents`, legacy user `~/.config/tmux-agents-mon/agents`, canonical user `~/.config/agenmux/agents`.

- [ ] Add failing tests for canonical-over-legacy option/config precedence, legacy-pane teardown, and build-aware title.
- [ ] Rename the Cargo package and Rust binaries to Agenmux names; update CLI output and usage.
- [ ] Add small shared compatibility lookups for tmux options and environment variables, then route existing callers through them.
- [ ] Change runtime-created pane titles, key tables, FIFO/cache/state names, status format, diagnostics, and notification text to Agenmux.
- [ ] Keep cleanup/teardown capable of finding legacy pane titles and stale legacy state.
- [ ] Run `mise exec rust@latest -- cargo fmt -- --check`, targeted tests, and LSP diagnostics.

### Task 2: Canonicalize Bootstrap, Releases, and Notification App

**Files:**

- Create: `agenmux.tmux`
- Modify: `agents-mon.tmux` into a forwarding compatibility entrypoint
- Modify: `scripts/install-bin.sh`, `scripts/install-app.sh`, `scripts/dev-bin.sh`, `scripts/version.sh`, `Makefile`, `.github/workflows/build.yml`

**Interfaces:**

- `agenmux.tmux` owns bootstrap and invokes `agenmux`; `agents-mon.tmux` immediately delegates to it.
- Release assets/directories are `agenmux-{linux,macos}-{x86_64,aarch64}` and contain canonical binaries.
- Legacy binary launchers named `agents-mon` and `agents-mon-notifier` delegate to canonical binaries for one cycle.
- `Agenmux.app` contains `agenmux-notifier` and bundle ID `io.github.snirt.agenmux`.
- Installer accepts `AGENMUX_REPO`/`AGENMUX_NOTIFIER_BIN` first and legacy variables second.

- [ ] Add failing packaging/bootstrap tests for canonical assets and old entrypoint forwarding.
- [ ] Move bootstrap logic to `agenmux.tmux`; make the old entrypoint a minimal forwarding wrapper.
- [ ] Update install/update/dev switching to canonical binary/state/package names with legacy read fallback.
- [ ] Package canonical artifacts and compatibility launchers in CI.
- [ ] Install/sign `Agenmux.app`; after success remove obsolete `AgentsMon.app` and leave failures non-destructive.
- [ ] Run shell syntax, release packaging tests, and Make dry runs.

### Task 3: Update Active Tests, Gates, and Documentation

**Files:**

- Modify active files under `tests/` except `tests/fixtures/`
- Modify: `tests/no-stale-runtime-refs.sh`, `tests/no-stale-runtime-refs-self-test.sh`
- Modify: `README.md`, `CONTRIBUTING.md`, `RELEASE_NOTES.md`, `site/index.html`, `.github/ISSUE_TEMPLATE/*.yml`, `AGENTS.md`
- Preserve: `docs/plans/**`, `tests/fixtures/**`

**Interfaces:**

- Tests invoke canonical commands by default and explicitly exercise legacy aliases only in migration cases.
- The stale-reference gate permits legacy names only in an explicit compatibility allowlist.
- Active docs teach Agenmux names; a short migration section lists old-to-new config, option, command, and app mappings.

- [ ] Update test helpers, expected binaries, tmux options, sockets, package names, and assertions.
- [ ] Add a compatibility allowlist gate that fails on accidental new legacy-name usage.
- [ ] Update current-facing docs/site/templates without rewriting historical plans or captured fixture evidence.
- [ ] Run `mise exec rust@latest -- cargo test`, `tests/run.sh`, and documentation/static checks.

### Task 4: Verify Real Upgrade and Rollback

**Files:** No production changes unless verification exposes a defect.

- [ ] Before switching, inspect `@agents-mon-bin`, derive the loaded plugin root, and count loaded `agents/*.conf`.
- [ ] Capture a real legacy sidebar pane and daemon executable.
- [ ] Load the renamed dev build through `agenmux.tmux`; verify pane title `agenmux`, header `agenmux dev`, canonical option/status names, and working → idle detection through the public detector.
- [ ] Verify legacy entrypoint/options still start the canonical runtime.
- [ ] Switch back to the existing release binary without modifying it and verify the daemon/path transition.
- [ ] Run project diagnostics, `git diff --check`, exact diff review, secret/private-identifier scan, and confirm historical fixtures/plans were not mass rewritten.

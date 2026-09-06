# Stale Dev Binary Recovery Implementation Plan

**Goal:** Recover automatically when `@agenmux-bin` points to a removed worktree build.

**Architecture:** Keep recovery at existing shell entry points. Activation clears a missing custom override and adopts the current checkout's debug engine when available, otherwise resuming normal release installation. Dev switching skips teardown only when the previous executable no longer exists, then installs the freshly built debug engine and restores active state.

**Tech Stack:** Bash, tmux integration tests.

## Global Constraints

- No new configuration or dependencies.
- Preserve valid custom binary behavior.
- Test observable entry-point behavior.

### Task 1: Regression checks

**Files:**

- Create: `tests/stale-dev-bin-recovery.sh`
- Modify: `tests/run.sh`

- [x] Exercise `agenmux.tmux activate` with missing `@agenmux-bin`; require stale override removal and current debug engine invocation.
- [x] Exercise `scripts/dev-bin.sh use` with missing prior binary; require successful debug switch without teardown.
- [x] Run focused test and confirm failure before implementation.

### Task 2: Minimal recovery

**Files:**

- Modify: `agenmux.tmux`
- Modify: `scripts/dev-bin.sh`

- [x] Clear missing custom binary override before activation engine checks.
- [x] Skip previous-engine teardown when previous executable is absent.
- [x] Run focused test and full suite.
- [x] Verify live stale-path recovery, inspect diff/status, and scan changed files for private or secret data.

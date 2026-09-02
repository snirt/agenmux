# Local Binary Switch Implementation Plan

> **For agentic workers:** use the `subagent-driven-development` skill to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `make dev-use` and `make dev-stop` commands that switch a live tmux server between this checkout's debug and existing release binaries without overwriting the release binary.

**Architecture:** A small shell helper owns the stateful tmux switch. Make only provides memorable aliases. The helper builds debug code when activating it, preserves whether the sidebar was open, tears down the old daemon, changes `@agents-mon-bin`, reinstalls hooks through the selected binary, and reopens the sidebar when needed.

**Tech Stack:** GNU/BSD make, Bash, Cargo, tmux.

## Global Constraints

- `target/release/agents-mon` remains unchanged.
- `dev-stop` restores the existing release binary, not a downloaded GitHub release.
- Failed activation restores the prior `@agents-mon-bin` value and sidebar state.
- No new dependency.

---

### Task 1: Add local binary switching

**Files:**

- Create: `scripts/dev-bin.sh`
- Modify: `Makefile`
- Modify: `README.md`

**Interfaces:**

- Consumes: tmux global options `@agents-mon-bin`, `@agents-mon-on`, and `@agents-mon-control-client`; existing `agents-mon teardown`, `setup`, and `toggle` commands.
- Produces: `make dev-use` and `make dev-stop`.

- [ ] **Step 1: Add the helper**

Implement `scripts/dev-bin.sh use|stop`. `use` runs `cargo build`, selects `target/debug/agents-mon`, and leaves `target/release/agents-mon` untouched. `stop` selects the existing release binary. Both preserve open/closed state and roll back selection when setup or reopen fails.

- [ ] **Step 2: Add Make aliases**

Add phony `dev-use` and `dev-stop` targets invoking the helper.

- [ ] **Step 3: Document the workflow**

Add the two commands under README development/testing guidance, including the existing-release-binary limitation.

- [ ] **Step 4: Run static checks**

Run:

```sh
bash -n scripts/dev-bin.sh
make -n dev-use dev-stop
cargo test
```

Expected: syntax succeeds, Make resolves both targets, Rust tests pass.

- [ ] **Step 5: Verify against live tmux**

Capture current `@agents-mon-bin`, plugin root, open state, and daemon executable. Run `make dev-use`; verify the daemon executable is `target/debug/agents-mon` and prior open state remains. Run `make dev-stop`; verify `@agents-mon-bin` is unset and daemon executable is `target/release/agents-mon`.

- [ ] **Step 6: Review repository safety**

Inspect `git status` and exact diff. Scan changed files for secrets and private identifiers. Confirm no raw captures, prompts, local paths, or session data were added.

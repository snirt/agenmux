# Safe Make Release Target Implementation Plan

> **For agentic workers:** use the `subagent-driven-development` skill to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `make release` to publish an already-created version commit and tag without allowing accidental releases from feature branches, dirty trees, or diverged history.

**Architecture:** Keep `make bump` local-only. Make targets delegate to focused shell scripts so guard logic is directly lintable and testable. `scripts/release.sh` validates clean `master`, fetches `origin/master`, requires exactly one local bump commit, verifies the Cargo-derived tag points at `HEAD` and is absent remotely, then atomically pushes `master` and the tag. Test it against a local bare Git remote so no network or real repository can be changed.

**Tech Stack:** GNU/BSD Make, POSIX shell, Git, existing `scripts/version.sh`.

## Global Constraints

- `make release` never creates or moves a tag.
- `make release` never pushes from a branch other than `master`.
- Publishing `master` and its tag is one atomic Git push.
- Existing `make bump` behavior remains local-only.
- Tests use a private temporary bare remote.

---

### Task 1: Add guarded release publishing

**Files:**

- Modify: `Makefile`
- Create: `scripts/bump.sh`
- Create: `scripts/release.sh`
- Create: `tests/make-release.sh`
- Modify: `tests/run.sh`

**Interfaces:**

- Consumes: clean local `master`, `origin/master`, Cargo version, matching local tag, one local bump commit.
- Produces: atomically updated remote `master` and matching version tag.

- [ ] **Step 1: Write failing integration test**

Create `tests/make-release.sh` that builds a temporary bare `origin`, checks that feature branches are rejected, creates one correctly named bump commit and matching tag on `master`, runs `make release`, and asserts remote `master` and tag both equal local `HEAD`.

Run:

```bash
bash tests/make-release.sh
```

Expected: FAIL because `Makefile` has no `release` target.

- [ ] **Step 2: Add minimum release target**

Move the existing bump recipe unchanged into `scripts/bump.sh`, make `bump` delegate to it, and add a `release` target delegating to `scripts/release.sh`. The release script:

1. Requires branch `master`.
2. Requires no tracked, staged, or untracked changes.
3. Fetches `master` into `refs/remotes/origin/master`.
4. Requires `origin/master` to be an ancestor exactly one commit behind `HEAD`.
5. Requires that commit message to be `chore: bump version to <Cargo version>`.
6. Requires local `v<Cargo version>` to point at `HEAD`.
7. Refuses an existing remote tag.
8. Runs `git push --atomic origin HEAD:master v<Cargo version>`.

- [ ] **Step 3: Run focused test**

```bash
bash tests/make-release.sh
```

Expected: PASS with `ok   make-release-guards-and-publishes-atomically`.

- [ ] **Step 4: Register and run full suites**

Add `bash "$DIR/tests/make-release.sh"` to `tests/run.sh`.

Run:

```bash
cargo test
./tests/run.sh
```

Expected: all checks pass.

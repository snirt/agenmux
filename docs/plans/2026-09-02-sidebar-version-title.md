# Sidebar Version Title Implementation Plan

> **For agentic workers:** use the `subagent-driven-development` skill to implement this plan task-by-task.

**Goal:** Show `agenmux v<version>` in release builds and `agenmux dev` in debug builds across every sidebar top title.

**Architecture:** Add one build-aware `app_title()` helper in `src/sidebar.rs`. Main rendering and both overlays consume it; main-header spacing derives from its real character width.

**Tech Stack:** Rust, existing sidebar renderer.

## Constraints

- Use `cfg!(debug_assertions)`; add no runtime setting or persisted state.
- Keep update, filtering, and click-row layout intact.
- Test with mise-managed latest Rust.

## Task 1: Add and verify title

**Files:**

- Modify: `src/sidebar.rs`

- [ ] Add a unit test asserting `agenmux dev` for debug builds and `agenmux v<package version>` for release builds.
- [ ] Add `app_title()` and use it in main, help, and versions headers.
- [ ] Replace hard-coded main title width with computed character count.
- [ ] Run `mise exec rust@latest -- cargo test`.
- [ ] Switch live tmux to debug and release binaries; verify title and daemon path in both states.
- [ ] Inspect diagnostics, status, exact diff, and privacy-sensitive content.

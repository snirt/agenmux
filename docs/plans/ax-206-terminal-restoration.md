# AX-206: Restore terminal state after panic and prevent terminal control-key traps

Issue: GitHub #206, "Terminal left in raw mode after a panic, Ctrl+Z, Ctrl+\\ or Ctrl+S".
Status: implemented and verified locally; not deployed or released. Findings below describe the original code.

## Findings

- `Cargo.toml` sets `panic = "abort"` for release builds. Destructors cannot be the only terminal cleanup mechanism.
- `src/input.rs::RawMode::enable` saves stdin termios, clears only `ICANON | ECHO`, and enables bracketed paste. It currently ignores `tcsetattr` failure.
- `RawMode::drop` restores termios and disables bracketed paste, but does not show the cursor.
- Both `src/sidebar.rs::run` (popup/direct sidebar) and `src/pane_writers.rs::run_pane` (split-pane reader) use this guard. Fix the shared owner rather than duplicating cleanup in callers.
- Sidebar termination handlers cover SIGINT and SIGTERM; pane-reader handlers cover SIGHUP and SIGTERM. Neither implements job-control suspension.
- Input decoding already maps Ctrl-C/Ctrl-D to quit as a fallback; configured search cancellation can take precedence. Clearing `ISIG` exposes those existing byte paths instead of delivering SIGINT.
- `run_pane` holds stdout locked during its loop. Emergency cleanup must not depend on formatted printing, stdout locking, or successful flushing of Rust buffers.

## Decision

Keep the existing guard and release abort policy. Disable terminal-generated signals and software flow control while the guard is active:

- Clear `ISIG` alongside `ICANON | ECHO`. Also clear `IEXTEN`: on macOS it keeps Ctrl-O (discard output, `FLUSHO`) and Ctrl-V active in non-canonical mode.
- Clear `IXON` so Ctrl-S cannot freeze output. Also clear `IXOFF` so application input is not subject to software flow control.
- Preserve all other termios settings, including existing VMIN/VTIME choices, and restore the complete saved snapshot afterward.
- Ctrl-Z and Ctrl-\\ become ordinary control bytes, resolved through existing key handling rather than suspending or killing the process. Unbound controls remain ignored.
- Preserve configured bindings and existing Ctrl-C search-cancel/exit behavior. Do not add new key actions.

This is the issue's suggested no-job-control option. External SIGTSTP/SIGSTOP suspension, external SIGQUIT/SIGABRT, SIGKILL, and direct `abort()` are not made recoverable by a panic hook. No claim of cleanup for every fatal signal.

## Implementation steps

### 1. Establish a PTY regression harness

- Add isolated subprocess tests beside `RawMode` in `src/input.rs`, using libc PTYs and the existing test executable where practical. Never mutate the test runner's stdin or global panic hook.
- Parent retains the PTY slave for termios assertions and drains the master with bounded waits, avoiding output backpressure and hung tests.
- Child uses the production guard on stdin/stdout. Capture baseline settings before enabling it; compare every meaningful termios field afterward, not raw struct padding.
- Add an abort-profile probe using the same production implementation. Compile the probe with `panic=abort` (or run a release abort build); a normal `cargo test --release` panic alone does not prove abort behavior.
- Keep panic triggering test-only. Do not introduce a public crash command or production environment toggle.

### 2. Make shared terminal ownership panic-safe

In `src/input.rs`:

- Factor one best-effort restore path for saved termios, bracketed-paste disable (`ESC[?2004l`), and cursor show (`ESC[?25h`). Use it from normal guard teardown and the panic hook.
- Publish the saved snapshot before changing terminal state. Ensure panic cleanup can access valid owned state without borrowing the stack guard or depending on a potentially poisoned mutex.
- Install the hook before terminal mutation. Retain and invoke the previous hook after cleanup so panic diagnostics remain visible.
- Use low-level, non-panicking output for emergency cleanup; handle interrupted/partial writes. Avoid stdout's lock and do not allocate or format panic-path messages.
- Keep cleanup idempotent and inactive after guard teardown. Serialize or isolate tests that exercise process-global hook/state; the production callers currently own one guard per process.
- Treat failed `tcgetattr` as inactive, preserving non-TTY behavior. On failed `tcsetattr`, do not announce successful raw mode or enable bracketed paste; leave cleanup state consistent.
- Apply the input flags from the decision above. Do not change Cargo's panic strategy or add dependencies.

Audit both callers for ordinary return and termination cleanup. Retain sidebar-specific file/pin cleanup and existing signal handlers; do not move filesystem or tmux work into the panic hook. Existing cursor-show output can remain harmlessly redundant unless removing it is clearly safe.

### 3. Cover behavior and document controls

Automated acceptance cases:

- Successful enable clears the selected flags and emits bracketed-paste enable only after termios setup succeeds.
- Normal drop restores the exact saved settings, disables bracketed paste, and shows the cursor.
- Panic restores the terminal and emits cleanup before abort; previous-hook diagnostics remain visible. Exercise panic while stdout is held locked.
- After ordinary teardown, a later panic does not reapply stale terminal settings.
- Non-TTY stdin remains safe and emits no terminal setup/cleanup sequences.
- Through a PTY, Ctrl-Z/Ctrl-\\ do not stop or kill the child; Ctrl-S does not stall subsequent output without Ctrl-Q. Guard teardown restores the original control-character behavior.
- Through the public key readers, Ctrl-C/Ctrl-D still reach the existing quit/cancel paths; representative unbound control bytes remain harmless. Check configured bindings rather than overriding them.

Update `docs/configuration.md` to explain application-owned control keys while the UI is open, including Ctrl-C search cancellation and the absence of keyboard job-control suspension. Leave release notes/version changes for a requested release.

### 4. Verify actual runtime paths

- Reproduce the current issue first in a fresh PTY or scratch tmux environment. Record termios, process state, and terminal control output before/during/after; avoid disturbing the user's active terminal.
- Run the changed popup/direct-sidebar and split-pane-reader paths in a private tmux server. Check ordinary close, existing handled termination signals, Ctrl-C/Ctrl-D, Ctrl-Z, Ctrl-\\, Ctrl-S, and continued redraw/input.
- Ensure scratch tmux key tables do not intercept controls and falsely satisfy the test; inject the bytes into the application's PTY when necessary.
- Prove panic behavior separately with the abort-profile probe; do not crash the active daemon.
- Confirm which binary the scratch server runs before drawing conclusions. No deployment or daemon restart is needed for a planning-only change.

Run verification under explicit Bash:

```bash
/bin/bash -c 'cargo test --locked'
/bin/bash -c 'bash tests/run.sh'
```

Also run the targeted PTY/abort check under explicit Bash using its final test entry point. Perform Rust diagnostics on changed source files, then inspect exact diff and git status. Review new test output/fixtures for secrets, private paths, usernames, and prompts; do not commit raw captures.

## Scope and completion

Expected implementation files: `src/input.rs`, `docs/configuration.md`, and a focused PTY test/probe entry point if needed. Change callers only when the audit reveals a required integration fix. No module split, new dependency, full raw-mode rewrite, job-control state machine, detector changes, release bump, or deployment.

Done when both terminal-owning runtime paths preserve existing key semantics, normal and aborting-panic cleanup pass PTY checks, Ctrl-Z/Ctrl-\\/Ctrl-S no longer trap the keyboard path, and the existing Rust/shell suites pass. Until then the issue remains unverified.

## Verification results

- Before the fix, an actual `sidebar-pane` process on a fresh PTY retained `ISIG` and `IXON`; Ctrl-S blocked a subsequent FIFO frame. Initial regression tests also failed on signal flags and missing panic restoration.
- Four isolated subprocess regressions now cover control bytes through `read_search_key`, normal cleanup, panic cleanup with another thread holding stdout, inactive post-drop cleanup, and non-TTY behavior. A child-only test entry point returns immediately during normal suite runs.
- The automated panic test's previous hook calls `abort()`, preventing unwinding or `Drop` from masking missing cleanup. It also verifies previous-hook diagnostics and terminal cleanup sequences. Core dumps are disabled in these children.
- A separate live check used the actual release binary (`panic=abort`): run `sidebar` with PTY stdin and a closed stdout pipe against a private tmux server. Rendering panicked on broken output, the process exited with SIGABRT, and all saved termios settings were restored. No public crash command or separate probe binary was needed; broken stdout necessarily makes output cleanup best-effort.
- Fresh panes in a private tmux server exercised both `sidebar` and `sidebar-pane`. Ctrl-Z/Ctrl-\\ did not suspend or kill them; search redraw and FIFO frame delivery continued after Ctrl-S without Ctrl-Q. Ctrl-C canceled popup search; Ctrl-D closed both paths and restored canonical input, echo, and ISIG. Existing shell tests also passed popup signal/pin cleanup.
- Scratch `@agenmux-bin` pointed to the checkout's release binary; its derived plugin root contained the six expected agent configurations. The active user daemon was not changed.
- `cargo test --locked`: 299 passed, one existing test ignored. `bash tests/run.sh`: passed. PTY assertions ignore only BSD's kernel-managed `PENDIN` queue-state bit when comparing saved local flags; other termios fields and speeds are checked.
- Follow-up: macOS PTYs kept `IEXTEN`, so Ctrl-O set `FLUSHO` and discarded all output until pressed again. Raw mode now clears `IEXTEN`; the PTY test sends Ctrl-O and asserts the flag. On a private tmux server, both `sidebar-pane` (FIFO frames) and `sidebar` (search redraw) kept updating after Ctrl-O/Ctrl-S, survived Ctrl-Z/Ctrl-\\, and restored `icanon isig iexten echo ixon` on Ctrl-D. `cargo test --locked` and `tests/run.sh` passed.

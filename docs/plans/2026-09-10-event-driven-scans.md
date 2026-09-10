# Event-driven pane scans implementation plan

> **For agentic workers:** use the `subagent-driven-development` skill to implement this plan task-by-task.

**Goal:** Reduce redundant pane captures without weakening current-state detection, using the existing persistent tmux connection.

**Architecture:** Enable control-mode output notifications for the long-lived sidebar/daemon, collect changed pane IDs, and cache captured screens. Run captures for dirty panes on a bounded 500 ms cadence, retain two-second metadata/background-session checks, and force a ten-second reconciliation of cached screens. A command-line scan remains an uncached full snapshot.

**Tech stack:** Existing Rust standard library, libc, regex and tmux control mode. No new dependencies, helper processes, sockets or UI frameworks.

## Global constraints

- Keep one persistent tmux control client per existing runtime instance.
- Output notifications cover only the attached session: never treat background-session silence as evidence that a pane is unchanged.
- Preserve the existing two-second background-session scan cadence; use a 500 ms minimum interval between output-triggered scans and a ten-second maximum cached-screen age.
- Keep reading notifications while waiting for command responses; an event during capture must remain pending for the next scan.
- Keep pane content out of control-mode command response bodies: preserve buffer/file capture transport and protocol-marker safety.
- Never infer idle from silence. State is determined by the existing public detector, with the current attention/debounce behavior retained.
- Preserve working-tree changes belonging to the user; implementation is isolated on `perf/event-driven-scans`.
- Do not split existing source files into modules. Do not change agent detection rules unless live evidence requires a separately explained fix.
- `.pi/` remains ignored. Never commit unsanitized captures, prompts, local paths or session diagnostics. Fixture additions must be reviewed and sanitized.
- Validate with a real agent in a fresh tmux pane through working and idle, confirming the tested binary's plugin root and loaded agent configurations first.

## Design decisions

Direct tmux socket access would duplicate its binary client protocol without reducing capture work. Streaming all pane output into a local terminal emulator would add parsing, memory and compatibility costs. The chosen implementation consumes output only as an invalidation signal, bounded to one dirty entry per pane, while tmux remains the source of screen snapshots.

The installed tmux 3.7c does not expose `pane_output_generation`. Depending on a newer generation format would not improve this installation. Full event coverage across unrelated sessions would require more clients or changes to the user's session/window layout; retain existing polling for those panes instead.

Reliability means recovering the current visible state, not promising a lossless history of every transient agent state. Startup, attachment changes, reconnect/new runtime, metadata changes, resize, new panes and expired cache entries require fresh capture. A failed capture/read must never become a successful cache entry or silently become an empty idle screen.

## Task 1: Integrate bounded event-driven scans and screen caching

**Files:**
- Modify `src/tmux.rs`: collect notifications consistently, track attached-session coverage, enable output for runtime, expose pending changes safely.
- Modify `src/scan.rs`: cache successful screens with identity, dimensions and freshness; keep one-shot scanning uncached.
- Modify `src/sidebar.rs`: own scan cache and deadlines; route events and retain periodic reconciliation/background discovery.
- Modify `src/sidebar/daemon.rs` only if its existing constructor or scheduling contracts require it.
- Modify `README.md`: briefly document actual monitoring behavior and background fallback.
- Add `tests/polling.sh`: isolated real-tmux regression harness with synthetic producers, no external API dependency.
- Modify `tests/run.sh`: invoke the polling harness from the integration entry point used by CI and Makefile.
- Add tests inside existing Rust modules for protocol/scheduling/cache edge cases. Do not extract modules merely to test them.

**Interfaces:**
- Preserve `scan::scan(&mut Tmux, &[AgentConf], &mut IdentCache, &mut SubjectCache, Option<&str>) -> Result<Vec<PaneRow>, TmuxError>` for CLI callers; a runtime-specific cached entry point may delegate common logic.
- Store runtime screen cache in `Sidebar`, not a global/static shared across tmux servers.
- The tmux notification accumulator exposes changed pane IDs, a full-invalidation indication and existing focus/layout activity. It tracks the attached session from control notifications or an explicit query. Unknown coverage must take the full-capture path.
- Consume the pending change set before each scan, never after it: events arriving during the scan belong to the next scan. Session changes invalidate cached coverage immediately.
- Cached screen keys must distinguish pane identity (pane ID, PID, command and detected agent), dimensions and relevant metadata (title, cwd). Screen caching must not suppress per-tick tracker/debounce updates.

- [ ] Add meaningful regression tests before or alongside implementation. Protocol tests feed `%output` adjacent to response blocks and during a response, then assert the pane remains pending after `run`; a session-switch event invalidates coverage; repeated events coalesce. The text `"%end 1 2"` inside a captured pane must not desynchronize commands.
- [ ] Add cache tests proving an unchanged covered pane reuses a successful screen, a dirty pane refreshes, changed metadata/identity/size refreshes, background panes refresh at normal periodic scans, and ten-second expiry refreshes. Failed file reads are errors and never cache success. CLI snapshots remain fresh.
- [ ] Add scheduling tests proving continuous output cannot postpone scans indefinitely, deadlines include periodic scans even during animation, and input remains responsive. Bound notification draining per loop so a busy producer cannot starve input. If partial notification lines are buffered, do not block waiting for a newline in the event-drain path.
- [ ] Implement notification accumulation in both command-response reading and event draining. Discard output payload after extracting its pane ID. Enable output only for long-lived monitoring; CLI scans need no output stream. Preserve existing response framing and `%exit` handling.
- [ ] Implement cache policy and integrate it into the runtime. On output, keep a fixed next-eligible scan deadline rather than resetting a debounce timer for every chunk. The last output must produce a final fresh capture. Keep periodic metadata discovery and full reconciliation independent of output events and focus events.
- [ ] Add debug counters (only when existing debug logging is enabled) for captured versus reused panes and scan duration, without logging screens. Use these in the regression harness to demonstrate reduced idle captures and eventual reconciliation.
- [ ] Implement `tests/polling.sh` using an isolated tmux server/temp runtime and the built binary. Exercise initial discovery, unchanged covered panes, output-only state change with constant title, continuous output and final idle, background session updates, session switching, pane resize and removal, and command-response marker content. It must assert states through the public detector/runtime row cache rather than only matching internal cache functions. Synthetic fixtures in this harness supplement, not replace, the controller's real-agent verification.
- [ ] Run `cargo fmt --check`, `cargo test`, `cargo build --release`, `bash tests/run.sh`, `bash tests/polling.sh`, and the existing navigation harness if its dependencies are present. Request sandbox escalation for isolated tmux access when necessary; never use the default tmux server in an automated harness.
- [ ] Update README monitoring description, inspect exact diff/status and scan changes for private identifiers and secrets. Commit only reviewed task files with a concise message and no agent attribution. Write a full test/evidence report to the provided report path, then return status, commits and concerns.

## Controller verification and review

- [ ] Prepare fresh real-agent pane on an isolated tmux server, inspect `@agenmux-bin`, derive the root using the binary ancestor containing `agents/`, and confirm effective built-in and override rules. Confirm the test daemon uses the implementation binary; do not replace an unrelated active worktree deployment.
- [ ] Observe initial idle, two working UI variants, and completed idle using title + screen captures kept in temporary files. Run each sanitized capture through `agenmux detect` and check the daemon's monitored state during the same transition. Restart the test daemon after any configuration changes.
- [ ] Add sanitized detection-relevant fixture excerpts and fixture expectations through the implementer, preserving stable controls/glyphs/timers rather than matching activity verbs. Record the live result and capture/reuse measurements without raw transcripts in the plan's validation section.
- [ ] Run a task-scoped Sol review for spec and quality, fix findings with the implementer and rerun covering tests. Run a final whole-branch review before handoff.
- [ ] Leave unrelated working-tree edits and the user's current tmux deployment untouched. Deliver the isolated branch/worktree and concrete validation evidence; do not claim automatic deployment.

## Validation results

Live verification used tmux 3.7c, a separate server, an attached terminal client, and real Claude Code sessions in fresh panes. The test server's `@agenmux-bin` resolved to the candidate release binary, its ancestor containing `agents/` matched the implementation worktree, and an empty configuration override directory ensured the checked built-in rules were effective. The test daemon was restarted for the final runtime build.

- Quiet-agent capture count: the existing daemon captured one unchanged agent five times over ten seconds; the final implementation captured an unchanged agent once over ten seconds. This is an 80% reduction in captures for this case, not a claim about total CPU usage.
- Attached-session activity: real working output was reflected in the runtime cache promptly. After the navigation and Unicode-reader fixes, a fresh real agent was working in both detector and daemon through sample 27. The public detector became idle at sample 28 and the daemon cache at sample 29 (roughly a quarter second later), remaining idle through sample 99. The final quiet pane again required one capture over ten seconds.
- Background-session activity: after the control client followed the viewer to another session, real agent start and completion were each reflected about 1.25 seconds after the detector changed, within the retained two-second fallback. The automated harness also covers background updates and control-session switching.
- Real fixtures: two working UI variants and a completed idle negative case retain activity glyphs, timers, interrupt controls and prompt layout. Local paths, the task prompt and other irrelevant session content were removed; the completion time was replaced with a neutral value. Agent detection rules were unchanged.
- Toolchain: the shell's default Rust 1.89 is too old for an existing macOS dependency. The already-installed rustup stable toolchain builds the project; no dependency change or toolchain installation was needed.
- Verification caught two runtime regressions before handoff: extra event scans sampled transient sidebar widths as border drags, and partial Unicode output caused strict control-stream decoding to exit. Layout reconciliation now retains its periodic cadence, and output reading handles raw bytes across line boundaries. The fresh-agent replay above passed after both fixes.
- A final reader refinement discards only opaque output payload bytes while preserving strict validation of command responses. Its fresh-agent replay again passed: working through sample 16, detector idle at 17, daemon idle at 18, stable through 69. Binary root and effective rules were reverified before restarting this build.

Automated verification passed: the release build, the strengthened polling harness, three consecutive uninstrumented navigation runs, and the complete shell integration runner. After the final expiry/readiness fixes, all 177 Rust tests and the polling/navigation harnesses passed again. One layout test failed in an earlier Rust run, then passed its focused rerun and the complete rerun. The shell runner's initial hook-cleanup failure was reproduced on both baseline and candidate with stale local release provenance; synchronizing the ignored build marker resolved it without source changes. Whole-tree `cargo fmt --check` remains blocked by pre-existing formatting in unrelated files; the task diff passes whitespace checks.

Task-scoped review and fix re-review approved the implementation. Whole-branch review then found that off-phase cache expiry needed its own wake deadline, and Linux pipe hangups needed to reach EOF handling. Both were fixed with deterministic regressions while preserving periodic background and layout maintenance. The resulting fresh-agent replay passed working to idle with one sample of lag (about 250 ms), stable through sample 69, retaining one quiet capture per ten seconds. Linux execution was unavailable locally; explicit hangup/error readiness tests and real EOF tests cover the reader on macOS. Final scoped re-review approved both fixes with no new findings. The user's original checkout and active tmux deployment remain untouched.

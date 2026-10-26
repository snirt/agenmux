# Event-driven sidebar resize reporting

Status: implemented and verified on macOS. See evidence and limits below.
Base: `a03f98a` — Unify sidebar daemon and stabilize Rex resizing.

This follows [the shared-daemon plan](2026-10-25-rex-shared-daemon.md).
Current source, rather than assumptions in that older plan, defines the baseline.

## Outcome

A sidebar pane reports its actual PTY dimensions when it starts or resizes.
The daemon updates render geometry and streams an updated complete frame through
its existing per-pane FIFO. Resizing must not wait for periodic geometry polling.

A filesystem path identifies the Unix-domain socket; it is not a regular file
that panes rewrite or watch. No user configuration changes should be needed.

## Baseline implementation and constraints at `a03f98a`

- `src/pane_writers.rs::run_pane` already receives `SIGWINCH` through a self-pipe,
  reads local PTY size, and immediately reprojects its cached frame. It does not
  report that size to the daemon.
- `src/sidebar/daemon.rs` measures geometry through `Multiplexer::sidebar_panes`
  during reconciliation and layout notifications.
- `src/sidebar.rs::event_loop` combines keys, backend notifications, scans, and
  animation. `src/input.rs::poll_inputs` currently has a fixed set of descriptors.
- Complete synchronized frames already travel through `agenmux-frame-{pane}`
  FIFOs. Readers buffer complete frames, recover after torn writes, and clip to
  their own viewport using absolute rows. Preserve these guarantees.
- `PaneWriters::emit` can spend up to 100 ms waiting for writable FIFO capacity.
  Backend scans/captures can also block the event loop for tens or hundreds of
  milliseconds. Adding a socket alone does not establish a smoothness guarantee.
- The daemon uses one logical row layout and a shared click-row map. Independent
  per-pane scrolling/render layouts would require a separate design.

## Proposed design

### Keep the seam small

The pane transport module owns connection state, framing, validation, partial I/O,
reconnection, stale-peer rejection, and bounded queues. Its interface exposes
validated viewport updates and descriptor readiness, not transport details to
multiplexer adapters. Use actual local sockets/socket pairs in tests; a new
public transport trait or dependency is unnecessary.

Keep implementation in existing modules initially. Any later file/module split
requires approval under `AGENTS.md`.

### Data flow

1. `sidebar-pane` wakes on startup or `SIGWINCH` and reads `TIOCGWINSZ`.
2. It immediately reprojects its cached frame, as today, and queues its latest
   actual `(columns, rows)` for the daemon.
3. A persistent Unix stream connects to
   `/tmp/agenmux-ipc-<uid>/<device-hex>-<inode-hex>.sock`, where device/inode
   identify the existing private multiplexer runtime directory. This bounded
   pathname supports long runtime paths and aliases without merging servers.
4. The daemon validates the pane identity and stores the newest viewport report.
5. Geometry dirties a dedicated render deadline, independent of agent scanning.
6. The daemon renders from cached sidebar state and queues complete frames into
   the existing per-pane frame FIFOs. Pane readers remain the final clipping guard.

Do not put keys, paste, mouse packets, or rendered frames into the new socket in
this change. Existing input and frame channels keep their current responsibilities.

### Protocol proposal

Use bounded, versioned JSON Lines records. Proposed maximum record size: 1 KiB.
Examples contain only synthetic identifiers:

```json
{"type":"hello","version":1,"pane":"block:example","cols":35,"rows":46}
{"type":"welcome","version":1,"generation":"opaque"}
{"type":"resize","generation":"opaque","seq":1,"cols":36,"rows":46}
```

- Bind each connection to the pane named in its accepted hello; do not accept a
  different pane identity on subsequent records.
- Validate membership in the daemon's sidebar inventory. Handle startup before
  inventory discovery with a bounded retry/registration path, not an unbounded
  collection of unknown panes or one inventory scan per incoming resize.
- Accept positive dimensions within PTY limits. If ioctl fails or returns zero,
  retain the last valid measurement; never publish `term_size`'s fallback as fact.
- Keep only the newest sequence for the active connection/generation. Retiring a
  connection invalidates any delayed messages from it. A replacement connection
  supersedes its predecessor for that pane.
- Malformed, oversized, unsupported-version, or stale records affect that peer,
  not the daemon. Do not log raw message payloads.

### Nonblocking reporting and reconnects

- Signal handlers only wake the self-pipe. Socket I/O and allocation happen in
  the pane loop, never in a signal handler.
- Connect, accept, read, and write must all be nonblocking, including connection
  establishment when the listener backlog is full. Set close-on-exec on descriptors.
- Maintain one in-flight record and one replaceable latest pending measurement.
  Finish a partially written record before sending its replacement; never splice
  JSON records together or grow a queue for every drag event.
- Watch writability only while bytes are pending; handle EOF/HUP/error without a
  busy loop. Disconnection must not block input or local cached-frame redraws.
- Reconnect with bounded backoff. Re-registration always includes the latest
  actual size, even if no additional resize signal arrives.
- Support startup before the listener and daemon handover without respawning panes.

### Geometry policy and reconciliation

Reported dimensions describe a viewport, not a request to resize every peer.

- The currently watched pane determines canonical render geometry. Reports from
  inactive peers update their records but do not select a session or reshape
  every other pane. Preserve common logical row ordering and the click-row map.
- While a reporting connection is healthy, its latest measurement is authoritative
  for rendering. Periodic backend measurements must not overwrite it with stale
  or inferred dimensions. Until registration or after disconnect, retain safe
  local clipping and reconcile through the existing backend path.
- Keep desired shared width separate from actual achieved width. Only confirmed
  divider-drag evidence may change the saved preference; a size report alone is
  not proof of a drag, completion of a request, or which layout caused the size.
- Rex width feedback must still pair measurements with a sufficiently current
  layout before advancing its search. Do not combine a new PTY size with an old
  ratio and recreate the asynchronous-resize bugs fixed in the base commit.
- Retain periodic reconciliation for topology, ownership, missing panes, settings,
  and clients which do not speak the protocol. Do not remove useful polling merely
  to claim that the whole application is event-driven.
- Standalone popups continue using their existing direct-TTY resize path.

### Scheduling and frame backpressure

- Extend the existing poll loop to include listener/peer descriptors, transport
  timers, and frame-writer readiness. Keep this out of multiplexer-specific code.
- Drain/coalesce resize events with bounded work per peer and iteration. Keys and
  shutdown must not starve behind a resize flood.
- Update stored dimensions immediately and schedule at most one geometry render
  per 16 ms cadence. Preserve the first pending deadline; continuously resetting
  a trailing-edge debounce would starve rendering during a sustained drag.
- Geometry-only rendering uses cached state: no agent capture, backend inventory
  query, or blocking multiplexer command in that fast path.
- Make frame delivery readiness-driven instead of waiting synchronously for FIFO
  capacity. Bound each pane to one in-flight frame and one latest pending frame.
  Preserve partial-write state, complete-frame decoding, and recovery after a
  stalled writer is dropped/reopened. One slow pane must not stall other panes.

### Ownership and compatibility

- Use the existing runtime directory identity and daemon ownership contract. The
  socket lives in a bounded, owner-only per-user directory because macOS limits
  Unix socket pathnames to 104 bytes. Reject unsafe directories, path types, and symlinks.
- Bind/publish only as the current owner. Cleanup must verify endpoint ownership
  or inode identity so an exiting old daemon cannot unlink a replacement's socket.
- Test endpoint replacement and reconnect behavior on macOS and Linux rather than
  assuming that Unix-socket pathname operations behave identically.
- Old pane processes without reports continue through existing reconciliation.
  New panes tolerate an absent/older listener while retaining current rendering.
- Do not widen this change into multi-server discovery. Rex's current runtime is
  scoped to a local backend/user, not a general server-identity namespace; document
  that limitation rather than claim isolation that the runtime does not provide.

## Implementation sequence

1. **Characterize latency and pin invariants.** Measure signal receipt, report
   receipt, render start, frame enqueue/write, and visible result separately.
   Reproduce ordinary drags, active captures, and slow readers before coding.
2. **Add protocol codec and bounded state.** Test fragmented/coalesced records,
   validation, generations, duplicate connections, and partial writes through the
   transport interface using real local socket pairs.
3. **Add daemon listener and lifecycle.** Integrate accepting/draining peers and
   ownership-safe cleanup; retain old geometry fallback. Verify takeover and startup.
4. **Report pane viewports.** Wire startup and the existing self-pipe wake to
   nonblocking reporting, coalescing, and reconnect snapshots. Keep local redraws.
5. **Consume reports on a geometry fast path.** Extend polling and deadlines;
   select watched geometry without backend scans or accidental drag adoption.
6. **Remove frame-writer stalls.** Add bounded readiness-driven delivery, fairness,
   and latest-frame scheduling while preserving framing and recovery semantics.
7. **Resolve measured blocking and prove the user flow.** Address the blocking-I/O
   decision below, then run live tmux/Rex stress checks and document measured limits.

These stages were implemented and tested in order in the working tree; they have
not been committed separately. Existing source modules were kept intact.

## Performance decision

The 16 ms cadence is a scheduler target, not a promise of 60 FPS end to end.
Existing synchronous scans can still prevent the daemon from consuming a report
on time, even if the geometry handler itself never scans.

Measurements separated native PTY resize delay from IPC/render delivery. In the
48-transition tmux stress run with active synthetic-agent captures, command-to-PTY
latency dominated at roughly 214 ms p95; after the PTY change, screen presentation
followed in roughly 34 ms p95. A broad backend-worker rewrite was not justified
by this workload. Geometry and pending frame delivery now outrank starting scans.

A scan already in progress can still delay receipt of a report. This is not a
hard real-time design or a 60 FPS guarantee. If future workloads exceed their
latency budget, isolate serialized backend work with bounded immutable handoff;
never share one control-mode reader across threads or add unbounded jobs.

## Verification and acceptance

- PTY tests: startup snapshot; resize without new output; narrow/wide/short/tall
  viewports; zero/failed measurements; UTF-8 and borders; unchanged click-row mapping.
- Socket tests: every record split, bursts, stale generations/connections,
  unavailable listener, reconnect without another resize, queue bounds, malformed
  peers, and old-owner cleanup after a replacement binds.
- Scheduling tests with a controlled clock: finite render deadline during a flood,
  latest geometry wins, no writable-descriptor spin, input priority, and fairness.
- Backpressure tests: stalled FIFO reader alongside a healthy reader; large frames;
  incomplete-frame recovery; no daemon/input stall or unbounded memory growth.
- Preserve clamp recovery, cancellation, peer width-intent propagation, and the
  first navigation key after daemon handover.
- Live scratch tmux and Rex sessions using the confirmed binary: sustained divider
  dragging, window/font changes, session switches, splits/closes, active agent
  output, keyboard/mouse input during dragging, and daemon restart mid-drag.
- Record p50/p95/worst report-to-frame latency under idle and active scans. Measure
  terminal presentation separately. Do not declare smoothness solely from the
  socket test or a low average. No periodic resize-only backend query should be
  required for connected reporting panes to redraw.
- Confirm final geometry, saved preferences, one owning daemon, bounded queues,
  low idle activity, and removal of scratch panes/socket endpoints.

Repository gates, under explicit Bash with inherited Rex context removed for the
isolated tmux harnesses:

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
env -u REX_SESSION -u REX_BLOCK -u REX_SERVER -u AGENMUX_MUX cargo test --locked
env -u REX_SESSION -u REX_BLOCK -u REX_SERVER -u AGENMUX_MUX \
  AGENMUX_BIN="$PWD/target/debug/agenmux" /bin/bash tests/run.sh
```

## Implementation evidence

- Protocol, backpressure, stale-sequence/connection, unknown-pane, unsafe-endpoint,
  long-runtime, and per-runtime isolation checks use actual local sockets.
- A controlled-clock daemon test proves the first 16 ms deadline is preserved,
  hidden reports do not steal geometry, disconnect restores backend fallback,
  and render reports never overwrite layout-paired drag evidence.
- Real child PTYs exercise startup snapshots, `SIGWINCH`, cached-frame reprojection,
  reconnect without another resize, complete UTF-8 frames, and terminal cleanup.
- FIFO tests cover stalled/healthy readers together, newest-frame coalescing,
  progress-based expiry, large frames, and incomplete-frame recovery.
- The navigation harness now waits for a matching visible scrollbar, rather than
  assuming inventory-cache publication acknowledges screen presentation.

Local macOS observations (milliseconds; not portable latency guarantees):

| Workload | Samples | p50 | p95 | Worst |
| --- | ---: | ---: | ---: | ---: |
| tmux resize command to visible screen, active captures | 48 | 223.0 | 242.0 | 249.5 |
| tmux resize command to observed kernel PTY size | 48 | 198.2 | 213.8 | 216.8 |
| tmux observed PTY size to visible screen | 48 | 22.5 | 34.4 | 45.5 |
| isolated Rex resize API call to captured screen | 16 | 71.1 | 96.2 | 96.2 |

The tmux test reads `TIOCGWINSZ` independently of the layout estimate and confirms
synthetic-agent capture is active before measuring. The isolated Rex server used
the built binary, actual terminal blocks and resize RPCs; a replacement daemon
received the retained size without another resize signal. Its server, panes, and
runtime were removed afterward; normal user sessions were not replaced.

The Rex check was headless, not an interactive GUI/font-change test. Linux has not
been exercised locally. No raw captures, trace logs, or local identifiers are
included here. Final gates passed: `cargo fmt --check`, strict all-target Clippy,
353 Rust tests (one existing diagnostic test ignored), and the full shell suite.
The exact diff and new plan were checked for whitespace, credentials, and private
identifiers. LSP was unavailable; compiler and Clippy checks supplied type validation.

### Resumed verification and Rex layout ownership

Rex divider feedback and window lifecycle now run through `rex_place.lua`;
Rust keeps its state opaque and uses reported PTY sizes only for rendering.
Lua regression checks cover handover, delayed drag measurements, both docking
sides, font changes, constrained widths, cancellation, and unresponsive clients.
`rex-reload` replaces the renderer while retaining blocks and split ratios.

Resumed checks passed on macOS: formatting, strict all-target Clippy, all 353
Rust tests (one diagnostic test ignored), and the full shell suite. A separate
foreground Rex server also exercised the production scan and reconciliation
scripts against real terminal blocks: repeated idle reconciliation and a fresh
layout-owner state preserved block identity and divider ratio. This headless
check does not substitute for an interactive app/font-change test.

Verification leaves the main Rex server and agenmux daemon running unchanged.
Scratch servers use explicit sockets, state directories, and config paths;
cleanup signals only their verified child PID, never `rex server stop`.
The shell harness clears inherited multiplexer context and runtime overrides.
Changes remain uncommitted and were not deployed through a main-daemon reload.

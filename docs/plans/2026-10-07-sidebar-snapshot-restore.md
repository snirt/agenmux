# Sidebar snapshot and restore (#178)

## Goal

Collapse state survives closing the sidebar and restarting tmux. After a tmux
restart the sidebar offers to rebuild the previous sessions, windows and panes;
nothing is recreated until the user presses `R`.

## Decisions

- Both phases ship on one branch, one commit each.
- Agent panes restore as a shell in their saved path. When
  `tmux_management.resume_agents = true` (default `false`), the agent's
  `AGENT_RESUME` command is typed into that shell with `send-keys`, so the pane
  survives the agent exiting.
- All six built-in confs ship an `AGENT_RESUME` (verified against each CLI's
  `--help`): `claude --continue`, `codex resume --last`, `opencode --continue`,
  `pi --continue`, `omp --continue`, `hermes --continue`. Each resumes the most
  recent session for its working directory, so two agents sharing a directory
  resume the same session.
- `R` creates missing sessions and missing windows inside existing sessions. It
  never adds panes to, or relayouts, an existing window.
- `R` needs `tmux_management.enabled`; `x` always works.

## Snapshot file

`<state_dir>/snapshot-<socket name>`, TOML, written atomically (temp + rename),
only when its content changes. `<file>.prev` holds a previous server's snapshot.

```toml
server = "<#{pid}|#{start_time}>"
collapsed = ["work", "work:2"]   # session name, or session:window_index

[[sessions]]
name = "work"
[[sessions.windows]]
index = 2
name = "editor"          # restored only when automatic_rename is off
automatic_rename = true
layout = "<#{window_layout}>"
sidebar = true           # layout includes the agenmux pane (leftmost, index 0)
[[sessions.windows.panes]]
path = "/repo"
agent = "claude"         # conf name; absent for ordinary panes
```

## Phase 1: collapse persistence

- `src/snapshot.rs` owns the file format, path, server-identity decision and
  restore command sequence.
- `run()` and `run_daemon()` open the snapshot after connecting (never
  `new_sidebar`, so unit tests never touch the user's state dir):
  - same server: load `collapsed` keys;
  - different server: rename the file to `.prev` before any write, then load its
    `collapsed` keys;
  - no file: nothing to load.
- Loaded keys sit in a pending set. Each scan converts a pending key to the live
  `$n`/`@n` id when a session/window with that name/index appears.
- Saved keys = live collapsed ids mapped to their current names (a live rename
  re-keys naturally) plus still-pending keys (absent sessions keep theirs),
  capped at 256 keys.
- Written on collapse changes and after periodic or full scans.

## Phase 2: layout and restore

- Periodic/full scans add one `list-windows -a` for layout, window name,
  automatic-rename and pane count; panes, paths and agents come from the scan.
- `sidebar = window_panes > saved panes`: the agenmux pane is always the
  full-height leftmost split (`split-window -hbf`), so it is pane index 0.
- With `.prev` holding sessions, the footer shows
  `Previous layout: 3 sessions, 5 agents · R restore · x dismiss` in place of
  the default hints.
- `R` replays, per missing window: create the window (its first pane is the
  first saved pane, or a placeholder when `sidebar`), split each further pane
  after the previous one with `-c <path>`, `select-layout`, kill the
  placeholder (its column goes to the neighbour; the sidebar re-adds itself),
  then type resume commands. Commands fork plain `tmux`, like jump, so hook
  output never desyncs the control pipe. Failures skip that step.
- `R` and `x` delete `.prev` and drop the banner.

## Known ceilings

- Window keys follow the index; `renumber-windows` can move a collapsed key.
- Popup and split sidebars running together write last-wins collapse state.
- A second restart before `R`/`x` replaces `.prev` with the newer server's
  snapshot.

## Tests

- Unit: same-server vs new-server decision, `.prev` preservation, snapshot
  round-trip, pending-key conversion and retention, replay command sequence
  (fake runner), conf `AGENT_RESUME` parsing, config key.
- Live: fresh tmux socket, collapse, close/reopen sidebar, kill-server, restart,
  check banner, `R` result and `x`.

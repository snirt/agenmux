# tmux-agents-mon

Monitor AI coding agents running in your tmux panes. A sidebar and a status-line
segment show every detected agent and its state:

- red `⣿` (blinks) — **blocked**, waiting for your input (permission prompt, menu)
- yellow spinner `⠹` — **working**, actively running
- green `⣿` (blinks) — **done**, finished while you were elsewhere; clears when you view it
- green `⣿` — **idle**, waiting at the prompt

Supported out of the box: **Claude Code, Codex, Hermes, Oh My Pi, OpenCode, and
Pi**. Adding an agent is one small config file — no code.

Detection is scraping-only: agents are identified by walking each pane's process
tree, state is inferred from the pane's visible screen and title (rules ported
from [herdr](https://github.com/ogulcancelik/herdr)'s detection manifests). No
hooks to install, nothing runs inside your agents.

## Demo

https://github.com/user-attachments/assets/b141a2db-b0f2-4775-bc9c-2aac70075187

## Install

With [TPM](https://github.com/tmux-plugins/tpm):

```tmux
set -g @plugin 'snirt/tmux-agents-mon'
```

Press `prefix + I`. That's it: the plugin downloads and verifies the Rust
engine for your platform in the background, then uses it automatically on the
next toggle. The bash fallback serves the first open while installation runs.
After TPM updates, the native engine is refreshed without removing the old
binary first.

Or manually: clone the repo and add `run-shell /path/to/tmux-agents-mon/agents-mon.tmux`
to `~/.tmux.conf`.

Requirements: tmux, bash, grep, awk, ps. `curl` and `tar` enable the automatic
native download; without them, Cargo builds it when available. Bash is the
fallback while the native engine is being installed or when it cannot be
installed. No required build step.

### Rust engine

The Rust engine is the primary implementation. It runs the scan/sidebar hot
path with one persistent tmux control-mode connection, using roughly 10x less
CPU than the bash fallback. The plugin downloads and verifies a prebuilt binary
automatically; if one is unavailable and [cargo](https://rustup.rs) is
installed, it builds the engine in the background. `make build` does the same
by hand, and `@agents-mon-bin` overrides the binary path. Agent detection stays
in `agents/*.conf`, so adding or tuning agents never needs a rebuild.

GitHub Actions also builds ready-to-use plugin archives for x86_64 and ARM64 on
Linux and macOS. The Linux binaries are statically linked for portability.
Download the archive for your platform from the
[latest GitHub Release](https://github.com/snirt/tmux-agents-mon/releases/latest)
and extract it; its native engine is already installed at
`target/release/agents-mon`.
Each release includes `SHA256SUMS` for verification. Builds from untagged commits
remain available as temporary artifacts on their **Build and Release** workflow
run.

## Usage

- `prefix + A` — toggle the sidebar (left split, auto-refreshes every 2s);
  agents are grouped under their session name, in tmux window order
- **Click an agent row** in the sidebar to jump to that agent's pane
  (requires `set -g mouse on`; clicks elsewhere keep default behavior)
- In the sidebar: `j`/`k` or `↑`/`↓` move the `❯` cursor, `Enter` or `l` jumps to
  the selected agent, `?` shows help (statuses + keys), `q` closes the sidebar;
  long lists scroll to keep the selection visible, and the cursor snaps to
  whichever agent pane currently has focus (instantly with the Rust engine —
  it reacts to tmux focus events)
- Add `#{agents_mon}` anywhere in `status-right`/`status-left` for the compact
  summary, e.g. `⣿1 ⣾2 ⣿1` colored red/yellow/green for blocked/working/idle
  (empty when no agents are running)

```tmux
set -g status-right '#{agents_mon} | %H:%M'
```

### Options

```tmux
set -g @agents-mon-key 'A'          # toggle keybinding (prefix table)
set -g @agents-mon-popup-key 'e'    # optional: dedicated key that always opens the popup
set -g @agents-mon-width '30'       # width (defaults: split 30, popup 40)
set -g @agents-mon-display 'popup'  # make the main key open a popup (default: left split)
set -g @agents-mon-height '15'      # fixed popup height (otherwise sized to the agent list, min. 15)
set -g @agents-mon-hide-windows 'agents*'  # hide matching windows from the prefix+w picker
                                    # (one fnmatch pattern; set to '' to restore the default picker)
```

With both keys set (e.g. `@agents-mon-key 'E'`, `@agents-mon-popup-key 'e'`)
you get `prefix+E` for the split sidebar and `prefix+e` for the floating popup.

In popup mode the same keybinding opens a floating window; close it with
`q` or `Esc` inside (there is no outside toggle — the popup grabs the client).
Click-to-jump works in split mode only; keyboard jump works in both, and the
popup reopens over the selected agent after a jump.

### CLI

```sh
scripts/scan.sh list    # pane_id  session:win.pane  agent  state  dir  subject
scripts/scan.sh status  # the status-line segment
scripts/scan.sh detect agents/codex.conf screen.txt 'pane title'
```

The Rust binary exposes the same commands (with `scan` as an alias for `list`):

```sh
target/release/agents-mon list
target/release/agents-mon status
target/release/agents-mon detect agents/codex.conf screen.txt 'pane title'
```

`sidebar` is an internal command used by the tmux integration.

## Adding / overriding agents

Drop a `.conf` in `~/.config/tmux-agents-mon/agents/`. A file with the same name
as a built-in (see `agents/`) replaces it wholesale. Example:

```bash
# ~/.config/tmux-agents-mon/agents/aider.conf
AGENT_BINS="aider"                 # process names that identify the agent
AGENT_PATH_HINTS=""                # optional: substring of a wrapped script path
BLOCKED_TITLE=''                   # grep -Ei pattern against #{pane_title}
BLOCKED_SCREEN='\(Y\)es/\(N\)o'    # grep -Ei pattern against the pane's bottom 20 lines
WORKING_TITLE=''
WORKING_SCREEN='esc to interrupt'
IDLE_SCREEN=''                     # explicit idle marker (rarely needed)
CHECK_ORDER="bt wt bs ws"          # rule order; first hit wins, fallback is idle
TITLE_STRIP='^aider: '              # optional regex removed from the pane title
SUBJECT_SCREEN=''                   # optional sed -E capture used as the subject line
SUBJECT_CMD=''                      # optional shell snippet used as a final subject fallback
```

`CHECK_ORDER` tokens: `bt`/`bs` blocked title/screen, `wt`/`ws` working
title/screen, `is` idle screen. Order matters when states can look alike —
Claude Code checks working before blocked so an already-answered permission
prompt left on screen doesn't read as blocked.

The sidebar subject shown below an agent is resolved from the cleaned pane
title, then `SUBJECT_SCREEN`, then `SUBJECT_CMD`. The shell snippet can use
`$path`, the pane's working directory. User configs are sourced by the bash
engine, so only install configs you trust; the Rust engine parses the same
assignments and runs `SUBJECT_CMD` when needed.

## Tests

```sh
tests/run.sh       # fast fixture and integration tests
tests/sanity.sh    # release smoke + source build in an isolated tmux server
```

The sanity test requires Nix and network access. It is the same end-to-end
check run for pull requests.

Fixtures in `tests/fixtures/` are real `tmux capture-pane -p` dumps where
possible (`claude-*`, `codex-idle`, `pi-idle`) and synthetic reconstructions for
hard-to-trigger states (`*-blocked`, `oh-my-pi-blocked`, `opencode-*`, `pi-working`). To improve
accuracy, re-capture a real screen into a fixture:

```sh
tmux capture-pane -p -t <pane> > tests/fixtures/claude-blocked.txt
```

## Known limits

- State is inferred from what's on screen; transient redraws can flicker
  (the sidebar debounces transitions to idle by one tick).
- Pane titles are only used when the agent's OSC title escapes reach tmux.
- No Windows support.

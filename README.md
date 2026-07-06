# tmux-agents-mon

Monitor AI coding agents running in your tmux panes. A sidebar and a status-line
segment show every detected agent and its state:

- 🔴 **blocked** — waiting for your input (permission prompt, menu)
- 🟡 **working** — actively running
- ✦ badge — finished working while you were elsewhere; clears when you view it
- 🟢 **idle** — waiting at the prompt

Supported out of the box: **Claude Code, Codex, OpenCode, Pi**. Adding an agent
is one small config file — no code.

Detection is scraping-only: agents are identified by walking each pane's process
tree, state is inferred from the pane's visible screen and title (rules ported
from [herdr](https://github.com/ogulcancelik/herdr)'s detection manifests). No
hooks to install, nothing runs inside your agents.

## Install

With [TPM](https://github.com/tmux-plugins/tpm):

```tmux
set -g @plugin 'snirt/tmux-agents-mon'
```

Or manually: clone the repo and add `run-shell /path/to/tmux-agents-mon/agents-mon.tmux`
to `~/.tmux.conf`.

Requirements: tmux, bash, grep, awk, ps. No build step.

## Usage

- `prefix + A` — toggle the sidebar (left split, auto-refreshes every 2s);
  agents are grouped under their session name, in tmux window order
- **Click an agent row** in the sidebar to jump to that agent's pane
  (requires `set -g mouse on`; clicks elsewhere keep default behavior)
- In the sidebar: `j`/`k` or `↑`/`↓` move the `❯` cursor, `Enter` or `l` jumps to
  the selected agent, `?` shows help (statuses + keys), `q` closes the sidebar;
  the cursor snaps to whichever
  agent pane currently has focus
- Add `#{agents_mon}` anywhere in `status-right`/`status-left` for the compact
  summary, e.g. `🔴1 🟡2 🟢1` (empty when no agents are running)

```tmux
set -g status-right '#{agents_mon} | %H:%M'
```

### Options

```tmux
set -g @agents-mon-key 'A'          # toggle keybinding (prefix table)
set -g @agents-mon-popup-key 'e'    # optional: dedicated key that always opens the popup
set -g @agents-mon-width '30'       # sidebar/popup width
set -g @agents-mon-display 'popup'  # make the main key open a popup (default: left split)
set -g @agents-mon-height '15'      # popup height (popup mode only)
set -g @agents-mon-hide-windows 'agents*'  # hide matching windows from the prefix+w picker
                                    # (one fnmatch pattern; set to '' to restore the default picker)
```

With both keys set (e.g. `@agents-mon-key 'E'`, `@agents-mon-popup-key 'e'`)
you get `prefix+E` for the split sidebar and `prefix+e` for the floating popup.

In popup mode the same keybinding opens a floating window; close it with
`q` or `Esc` inside (there is no outside toggle — the popup grabs the client).
Click-to-jump works in split mode only; keyboard jump works in both.

### CLI

```sh
scripts/scan.sh list    # pane_id  session:win.pane  agent  state  dir
scripts/scan.sh status  # the status-line segment
```

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
```

`CHECK_ORDER` tokens: `bt`/`bs` blocked title/screen, `wt`/`ws` working
title/screen, `is` idle screen. Order matters when states can look alike —
Claude Code checks working before blocked so an already-answered permission
prompt left on screen doesn't read as blocked.

## Tests

```sh
tests/run.sh
```

Fixtures in `tests/fixtures/` are real `tmux capture-pane -p` dumps where
possible (`claude-*`, `codex-idle`, `pi-idle`) and synthetic reconstructions for
hard-to-trigger states (`*-blocked`, `opencode-*`, `pi-working`). To improve
accuracy, re-capture a real screen into a fixture:

```sh
tmux capture-pane -p -t <pane> > tests/fixtures/claude-blocked.txt
```

## Known limits

- State is inferred from what's on screen; transient redraws can flicker
  (the sidebar debounces transitions to idle by one tick).
- Pane titles are only used when the agent's OSC title escapes reach tmux.
- No Windows support.

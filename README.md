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
in `agents/*.conf`, so adding or tuning agents never needs a rebuild. Building
on macOS needs rustc 1.90 or newer (for the native notification helper).

Split mode preserves one empty tmux pane in each window, so switching windows
never changes the layout. Those panes have no shell or `agents-mon` child
process (`pane_pid=0`); the single daemon writes only to sidebar panes currently
visible in attached clients. Hidden panes retain their last frame; with every
client detached, one pane stays warm for the next attach.

GitHub Actions also builds ready-to-use plugin archives for x86_64 and ARM64 on
Linux and macOS. The Linux binaries are statically linked for portability.
Download the archive for your platform from the
[latest GitHub Release](https://github.com/snirt/tmux-agents-mon/releases/latest)
and extract it; its native engine is already installed at
`target/release/agents-mon`.
Each release includes `SHA256SUMS` for verification. Builds from untagged commits
remain available as temporary artifacts on their **Build and Release** workflow
run.

## Updating

When a newer release exists, the sidebar header says so, and says what to do:

```
agents ↑0.1.8
press u to update
```

Press `u` to open the version picker, choose a release, and press `Enter`.
The plugin switches its source *and* its native engine to that release and
reopens itself — so the same key rolls **back** to an older release just as
easily. The check that feeds the notice runs in the background, at most once a
day; nothing is downloaded or changed until you pick a version.

Details worth knowing:

- `Cargo.toml` is the only version source. The engine installed is always the
  one matching the checked-out source, so the two can never drift apart.
- On a git install (TPM or a manual clone) a switch is `git checkout <tag>`,
  leaving the checkout detached at that tag — the normal pinned-plugin state.
  It **refuses to run against a dirty working tree**; commit or stash first.
- On a tarball install the verified release archive is extracted in place.
- TPM's `prefix + U` still works and moves you to the tip of the default branch.
- From a shell: `bash scripts/update.sh v0.1.5` (or `latest`).

## Usage

- `prefix + A` — open the sidebar, or enter its navigation mode while it is
  already open (left split, auto-refreshes every 2s); agents are grouped under
  their session name, in tmux window order. Navigation selects the sidebar and
  preserves your normal tmux pane bindings; `q`/`Esc` closes the sidebar
- **Click an agent row** in the sidebar to jump to that agent's pane
  (requires `set -g mouse on`); clicking anywhere else in the sidebar enters
  navigation, while clicks in regular panes keep tmux's default behavior
- In the sidebar: `j`/`k` or `↑`/`↓` move the `❯` cursor, `Enter` or `l` jumps to
  the selected agent, `u` opens the [version picker](#updating), `?` shows help
  (statuses + keys), `q`/`Esc` closes the sidebar;
  the `❯` cursor is green while navigation is active, long lists scroll to keep
  the selection visible, and the cursor snaps to whichever agent pane currently
  has focus (instantly with the Rust engine — it reacts to tmux focus events)
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
set -g @agents-mon-notifications 'off'  # disable desktop notifications (default: on)
```

With both keys set (e.g. `@agents-mon-key 'E'`, `@agents-mon-popup-key 'e'`)
you get `prefix+E` for the split sidebar and `prefix+e` for the floating popup.

In popup mode the same keybinding opens a floating window; close it with
`q` or `Esc` inside (there is no outside toggle — the popup grabs the client).
Click-to-jump works in split mode only; keyboard jump works in both, and the
popup reopens over the selected agent after a jump.

### Desktop notifications

The Rust engine sends a native desktop notification when an agent finishes or
needs attention while its pane is not focused. The title identifies the agent
and outcome; the body includes the remembered subject, directory, and tmux
target when available. Existing blocked/idle agents are a silent baseline when
the monitor starts, and unchanged states do not repeat notifications.

For complete focus detection, including a pane selected in Ghostty, Kitty, or
another terminal while that application is in the background, enable tmux
focus events:

```tmux
set -g focus-events on
```

The [tmux manual](https://man.openbsd.org/tmux.1#focus-events) notes that clients
may need to detach and attach again after this option changes. With focus events
off, agents-mon conservatively suppresses a notification whenever any real tmux
client has the pane selected. With them on, it suppresses only when at least one
real client both selects the pane and reports itself focused; control-mode
clients are ignored.

Notifications are enabled by default. Disable them with:

```tmux
set -g @agents-mon-notifications off
```

On macOS, agents-mon sends notifications natively through
`UNUserNotificationCenter` via a small helper app built from this repo — no
Homebrew or other runtime dependency. Install it once:

```sh
make install-app
```

This builds the Rust engine, assembles `AgentsMon.app` into `~/Applications`
(pass a different directory to `scripts/install-app.sh` to override), ad-hoc
signs it, shows the macOS permission prompt, and waits for your answer: a test
notification confirms it works, or the command tells you notifications are off
and where to enable them. Allow **AgentsMon** when prompted, or later under
System Settings → Notifications → AgentsMon. Denying keeps notifications
fully silent — there is no fallback around your choice. The app is background-only; it never appears in the
Dock. Once installed, plugin updates refresh the app automatically; the
permission survives updates.

Clicking a notification body activates your terminal (Ghostty, Kitty, iTerm2,
WezTerm, Apple Terminal, and Alacritty are recognized) and jumps the most
recently active real tmux client to the exact pane. Panes that no longer exist
are safe no-ops. Notifications play macOS's built-in `Glass` alert sound.

Without the installed app, agents-mon falls back to the built-in `osascript`,
which displays notifications with the `Glass` sound but cannot handle clicks.

Each notification keeps a small helper process waiting for its click; after 24
hours the notification is closed and the helper exits, so clicks on older
entries do nothing. If several notifications are pending at once, macOS may
route a click to the newest helper only — the click is then ignored rather
than jumping to the wrong pane.

On Linux, agents-mon uses the optional `notify-send` command when a `DISPLAY`
or `WAYLAND_DISPLAY` session is available; Linux notifications are
display-only. Without it, delivery is silently skipped—the rest of the plugin
has no additional runtime requirement. Delivery is best effort and never
interrupts the sidebar if a notifier is unavailable or permission is denied.
The operating system may require notification permission for the sender it
displays.

The sidebar or popup must remain open while the state transition occurs because
notifications use the existing monitor process; no extra daemon is installed.
The Bash fallback does not send notifications. A transition suppressed while
focused is not delivered later merely because focus moves away.

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
target/release/agents-mon --version
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
- Desktop notifications are local to the tmux host; headless and remote hosts
  without a desktop notification service silently skip delivery.
- No Windows support.

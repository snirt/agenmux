# agenmux

[![agenmux logo](site/logo.png)](https://snirt.github.io/agenmux/)

Website: <https://snirt.github.io/agenmux/>

**Rebranding note:** `agents-mon` is now **agenmux**. Existing installations
remain compatible for one release cycle; see [Upgrading from agents-mon](#upgrading-from-agents-mon).

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

Try the interactive demo on the [agenmux website](https://snirt.github.io/agenmux/).

## Quick start

```sh
curl -fsSL https://snirt.github.io/agenmux/install.sh | sh
```

The script clones the plugin to `~/.tmux/plugins/agenmux`, creates
`~/.config/agenmux/agents/`, asks for the launcher keys (default `prefix + A`
sidebar, `prefix + a` popup), shows the lines it wants in your tmux.conf (a
`@plugin` entry if you use TPM, `run-shell` otherwise), writes them once you
confirm, and reloads tmux. Run it again to update; an existing agenmux entry
is left alone.

Or install with [TPM](https://github.com/tmux-plugins/tpm) directly:

```tmux
set -g @plugin 'snirt/agenmux'
```

Press `prefix + I` to install, then `prefix + A` to open the sidebar. The
plugin downloads and verifies the Rust engine for your platform in the
background. If you toggle before installation
finishes, that first activation waits for the same installer; a failed download
or build is reported in tmux instead of running an unverified fallback. After
TPM updates, the native engine is refreshed without removing the old binary.

### Manual install

Clone the repo and add `run-shell /path/to/agenmux/agenmux.tmux` to
`~/.tmux.conf`, then reload tmux.

Requirements: tmux and bash for TPM/bootstrap. `curl` and `tar` enable the
automatic native download; without them, Cargo builds it when available. No
required build step on a supported release platform. A Nerd Font is recommended
for private-use UI icons; the interactive installer prints a visual font check and
warns what to do when its sample icon appears as a box or blank.

### Upgrading from agents-mon

Existing installs continue working for one release cycle. `agents-mon.tmux`,
`@agents-mon-*`, `#{agents_mon}`, `AGENTS_MON_*`, and
`~/.config/tmux-agents-mon/agents/` are accepted as compatibility inputs;
agenmux writes only canonical names. Canonical values win when both exist.

| Legacy | Canonical |
| --- | --- |
| `agents-mon.tmux` | `agenmux.tmux` |
| `@agents-mon-*` | `@agenmux-*` |
| `#{agents_mon}` | `#{agenmux}` |
| `AGENTS_MON_*` | `AGENMUX_*` |
| `~/.config/tmux-agents-mon/agents/` | `~/.config/agenmux/agents/` |

macOS installs `Agenmux.app` under bundle ID `io.github.snirt.agenmux` and
removes the old helper after successful installation. macOS asks for
notification permission again because the bundle identity changed.

## Usage

Press `prefix + A` to open the left sidebar or enter navigation when it is
already open. By default, agents are grouped by session in tmux window order
and refresh every two seconds.

| Input | Action |
| --- | --- |
| `prefix + A` | Open the sidebar or enter navigation |
| Click a selectable row | Select it; click the selected row again to open it (requires `set -g mouse on`) |
| Mouse wheel | Scroll the sidebar without changing selection |
| `j` / `k`, `↑` / `↓` | Move selection |
| `Enter` / `l` | Jump to the selected agent or pane |
| `/` | Search fields available in the current mode |
| `f` | Toggle **User attention** (show done, working, and blocked; hide idle) |
| `Esc` | Exit search and clear filters |
| `u` | Open version picker |
| `?` | Show help |
| `q` / `Q` | Close sidebar |

<details>
<summary>Search, mouse, and navigation details</summary>

Clicks outside selectable rows enter navigation; clicks in regular panes retain
tmux behavior. The first click on a selectable row selects it, and a second click
opens its exact agent or pane. Wheel scrolling moves the list viewport without
changing selection or switching panes.

During search, type normally, then press `Enter` to accept the query and restore
`j`/`k` navigation; press `Enter` again to jump. `↑`/`↓` or
`Ctrl-N`/`Ctrl-P` move while typing, and `Ctrl-U` clears the query without
leaving search. User attention and text filters are mutually exclusive. Matching a
session keeps all its agents visible as context.

Set `display.show_all_panes = true` in the application configuration to turn the
sidebar into a complete tmux navigator. The default is `false`, which preserves
the agent-only list. Press `.` in the sidebar to toggle between the two live;
that view toggle is not written to config, so a reload restores the configured
default. All-pane mode renders sessions, windows, and panes in tmux
order. A pane in a split window shows its pane title when set, otherwise its
running command; a single-pane window shows its window name. A split window gets an accent-colored ` name` header and nested pane rows. A
single ordinary-pane window collapses into a selectable muted ` name` row; a single-agent
window retains its compact agent row. Pane rows and agent description rows are selectable;
session and split-window headers provide context. `Enter` and repeated clicks can jump to
ordinary panes as well as agent panes.
All-pane hierarchy uses indentation without connector glyphs. Nested ordinary panes use a
done-colored `▢ command` row; when selected, its full row uses `theme.colors.pane_bg`. Agent
rows keep their state styling and show the animated status glyph, agent name in its original
style, then pane command. Agent descriptions stay on the next indented line. An ordinary pane
running `nvim` shows the Neovim icon (`nf-linux-neovim`) in place of its window or pane glyph.

Agent rows in both views prefix the agent name with its `AGENT_ICON`. Built-in defaults:
Claude `nf-cod-claude` and Codex `nf-cod-openai` (both need Nerd Fonts 3.5+), Pi and
Oh My Pi `nf-md-pi`, and generic fallbacks for OpenCode (`nf-cod-code`) and Hermes
(`nf-cod-hubot`), which have no dedicated Nerd Fonts glyph. The text name always follows
the icon by default, so a missing glyph never hides which agent a row is.
`display.agent_label` picks `icon-text` (default), `icon`, or `text`; an agent
without an icon always shows its name.

Search in all-pane mode matches session, window, and pane metadata. A session or
window match keeps its pane subtree, while a pane match keeps its session and
window ancestors on screen. User attention filtering keeps done, working, and
blocked agent panes, with their session and window ancestors shown for context;
idle agents are hidden. With no inventory, all-pane mode says `no panes`; default
mode says `no agents` when no agents are detected; either mode says `no matches`
when a search or User attention filter has no results.

This setting changes only sidebar presentation and navigation. `scan`, `list`,
`status`, agent detection, attention tracking, notifications, and the scan cache
remain agent-only; ordinary panes never contribute agent state or alerts.

The header shows active filters, matching/total selectable-pane counts, and
contextual controls only while filtering. Agent cursors follow state color; ordinary-pane
cursors use `theme.colors.muted_fg`. Long lists scroll to keep selection visible.

</details>

Add `#{agenmux}` to `status-right` or `status-left` for the compact summary,
e.g. `⣿1 ⣾2 ⣿1` in red/yellow/green for blocked/working/idle. It stays empty
when no agents are running.

```tmux
set -g status-right '#{agenmux} | %H:%M'
```

### Options

| Option | Default | Purpose |
| --- | --- | --- |
| `@agenmux-key` | `A` | Main launcher in prefix table (resolved display mode); empty disables installation |
| `@agenmux-popup-key` | `e` | Dedicated popup launcher; empty disables installation |
| `@agenmux-width` | sidebar: `30`; popup: `40` | Sidebar or popup width |
| `@agenmux-display` | `split` | Main-key display: `split` (sidebar) or `popup` |
| `@agenmux-height` | auto, up to available height | Fixed popup height; auto prefers at least `15` rows when they fit |
| `@agenmux-hide-windows` | unset | Leave the picker unchanged; a glob excludes matches, `''` restores default picker |
| `@agenmux-notifications` | `on` | Desktop notifications; set `off` to disable |
| `@agenmux-debug` | unset | Absolute path of a trace file for the next sidebar start; see [Troubleshooting](#troubleshooting) |

**Opening bindings belong to tmux configuration, not app TOML.** Bootstrap
reads `@agenmux-key` and `@agenmux-popup-key` (legacy `@agents-mon-*` aliases
accepted). Missing options default to A/e; canonical presence wins over legacy,
including an explicit empty value. Bindings use normal tmux **last-writer-wins**
semantics: plugin loading overwrites an existing binding on the same key, and a
later user binding overwrites the plugin's. No collision registry or migration
is used. Rust setup/toggle neither installs nor removes opening bindings.

For manual opening bindings, disable both plugin launchers **before TPM**:

```tmux
set -g @agenmux-key ''
set -g @agenmux-popup-key ''
# Your plugin declarations go here, before the TPM initialization:
run '~/.tmux/plugins/tpm/tpm'
# Bootstrap publishes this trusted plugin path even while installation is pending.
bind-key A run-shell -b "bash #{q:@agenmux-plugin-dir}/agenmux.tmux activate '' #{q:client_name}"
bind-key e run-shell -b "bash #{q:@agenmux-plugin-dir}/agenmux.tmux activate 'popup' #{q:client_name}"
```

These entrypoints verify the engine and serialize installation before activation;
do not bypass them with a direct binary binding. To customize plugin launchers
instead, set nonempty options before TPM (e.g. `@agenmux-key 'E'`). Reload tmux
configuration to install the new keys. Changing or disabling an option does not
remove any previously installed binding: explicitly `unbind-key A` / `unbind-key e`
(or the old custom keys) when needed, then reload. Invalid app TOML does not
prevent launcher installation; activation still validates before app mutation.

In popup mode the same keybinding opens a floating window; close it with
`q` or `Esc` inside (there is no outside toggle — the popup grabs the client).
Mouse clicks and wheel scrolling work in split mode only (tmux does not
forward mouse events into a popup); keyboard jump works in both, and the popup
reopens over the selected agent after a jump.

### Application configuration

Optional `$XDG_CONFIG_HOME/agenmux/config.toml` (absolute, nonempty XDG root),
otherwise `$HOME/.config/agenmux/config.toml` (absolute HOME):

```toml
version = 1
[display]
mode = "split"
show_all_panes = false
show_frame = true # whole-pane Agenmux frame
sidebar_width = 30
popup_width = 40
popup_height = "auto"
agent_label = "icon-text" # icon-text | icon | text
[behavior]
notifications = true
# hide_windows = "agents*" # omitted: leave your picker alone
```

`agenmux config --help` prints every configurable key with its accepted values
and default. A complete annotated example ships as
[`examples/config.toml`](examples/config.toml).

Press `s` in either split or popup mode to open **Settings** (`keys.normal.settings`
changes or unbinds this opener). The view lists every application setting and
adapts from value/source columns to compact rows with selected-setting details on
narrow panes. `↑`/`↓` moves, `Enter` edits, `C-u` clears the edit buffer, and
`Esc` cancels or returns. Saved values are validated before an atomic file update;
errors keep both last valid file and live configuration.

Each row distinguishes persisted file value from effective value and its winning
source. **Revert to defaults** removes persisted display, behavior, theme, color,
and keymap customizations after confirmation. Higher-precedence CLI or tmux
overrides remain visible and effective. Launcher keys and `agents/*.conf` are not
application settings and are unchanged.

After editing the file, run `agenmux config reload`. It validates the file,
reinstalls the key tables when the keymap moved, and tells running views to
re-read; an invalid file is refused and changes nothing, so a typo cannot take
a live sidebar down. Open sidebars pick up the new settings, theme and hints
within a moment, with no reopen. There is still no file watcher, and nothing
re-reads the file on its own: a process that starts without a reload keeps the
snapshot it read at startup. Plain `agenmux config check` needs no tmux server;
`agenmux config check --effective` also validates current tmux options and
reports their sources. Themes and keys apply to both split and popup renderers.

```toml
[theme]
base = "light" # dark (default), light, or terminal
[theme.colors]
working_bg = "#fff0cc" # override only this semantic role
pane_bg = 236          # selected ordinary pane-row background
# working_fg = 136    # indexed terminal color, 0..255
# header_bg = "default"
```

The selected base supplies every omitted role. `dark` preserves the original
appearance; `light` uses explicit dark foregrounds and pale selected-row fills
(for a light terminal background); `terminal` uses terminal-default backgrounds.
Colors accept only `"default"`, integers 0..255, or exact `"#RRGGBB"` values—not
ANSI strings. Roles are `header_fg/bg`, `pane_bg`, `text_fg`, `muted_fg`,
`accent_fg`, `error_fg`, and `blocked`, `working`, `idle`, `done` each with
`_fg`, `_bg`, `_bg_unfocused`. `pane_bg` fills the selected ordinary pane row;
`muted_fg` colors its `●` markers. Foregrounds color status/cursor marks independently
of their
unchanged glyphs, spinner, and blink. Overrides affect hints, help, and version
views too. No global tmux colors are changed. Close/reopen running views after
activation to consume the new palette.

```toml
[keys.normal]
down = ["n", "PageDown"] # replaces the whole default list ["j", "Down"]
up = ["p", "PageUp"]
close = []               # unbind; Ctrl-C/Ctrl-D still close the popup
settings = ["s"]          # opens the in-app Settings view
[keys.search]
cancel = ["Escape", "C-g"]
```

Normal actions: `down`, `up`, `jump`, `search`, `filter`, `reset`, `help`,
`versions`, `settings`, `close`. Search actions: `up`, `down`, `accept`, `cancel`,
`backspace`, `clear`. Each value replaces that action's default list (at most
16 chords); `[]` unbinds it and drops it from hints and help. Chords are one
printable ASCII character, `Space`, `Up`/`Down`/`Left`/`Right`, `Home`/`End`,
`PageUp`/`PageDown`, `Enter`, `Escape`, `Tab`, `BSpace`, or a `C-x` control
chord the terminal does not intercept (`C-c`/`C-d` are reserved, except `C-c`
for search cancel; `C-@`, `C-a`, `C-b` and `C-l` are reserved by the sidebar's
own key transport; `C-i`/`C-m`/`C-[`/`C-?` are the Tab/Enter/Escape/BSpace
aliases). A chord may serve one action per mode; search chords cannot be
printable because typing owns them. Keys are data: the split-mode tables bind
each chord to a fixed internal action, never to a command from the file.
`agenmux config reload` reinstalls the tables and updates the hints of running
views; the next toggle does the same for the tables on its own.

Tmux mutations are opt-in. With management enabled, the built-ins are `cc`
(create a window in the selected pane's session), `cs` (create a session), `dd`
(delete the selected record), and `r` (rename). `gg` remains available regardless
of this setting. With management on, session rows and multi-pane window rows
become selectable, so the cursor can stand on any record; `dd` deletes that
record with everything under it: a session row kills the session, a window row
(or a window's only pane) kills the window, a pane inside a split window kills
just that pane. `r` renames that same record in place: its name turns into an
edit field preloaded with the current name; empty Enter or Escape cancels. `cc`
and `cs` add a placeholder row to the tree (a window under the selected session,
or a new session at the end) where you type the optional name; Enter creates it
(blank for tmux's default), Escape drops the row. New windows inherit the
selected pane's working directory and show in the invoking client while input
stays in the sidebar. Every mutation leaves the client on the sidebar pane, in the
sidebar key table. Mutation prompts accept input only from that invoking client.
The delete confirmation is inline: the hint row shows the record's name and
stable tmux ID and cancels on Enter, `n`, Escape, or any input other than
lowercase `y`. Window and pane deletes refuse to implicitly destroy a session;
delete the session row so attached clients can be moved safely first. Set
`confirm_delete = false` only if immediate deletion is intentional.

```toml
[tmux_management]
enabled = false       # required for every create/delete sequence
confirm_delete = true

[keys]
sequence_timeout_ms = 1000 # positive; applies live to gg/cc/cs/dd
```

Disabled mutation sequences are not installed, shown in continuation hints, or
listed in `?` help, and direct legacy key packets cannot bypass the gate. Reloading
a disabled setting also clears pending mutation prefixes. Every operation captures and
revalidates tmux pane/window/session IDs before mutation; a stale target reports an
error and refreshes instead of falling back to another resource. Pane splitting is
not part of tmux management.

With tmux management enabled, `e` opens nvim and `og` opens lazygit in a new
window at the selected pane's working directory. Press `o` to see the optional
launcher sequences. The new window is focused in the invoking client, and the
sidebar refreshes to include it. The built-ins can be changed or disabled, and
custom launchers can be added:

```toml
[quick_launchers.nvim]
sequence = "e"
label = "nvim"
command = "nvim"
args = []
working_directory = "selected" # selected pane cwd; "tmux" uses the session default
enabled = true

[quick_launchers.lazygit]
sequence = "og"
label = "lazygit"
command = "lazygit"
args = []
working_directory = "selected"
enabled = true

[quick_launchers.terminal]
sequence = "ot"
label = "terminal"
command = "fish"
args = ["--login"]
working_directory = "selected"
enabled = true
```

Launcher `args` are passed as separate program arguments. With management enabled,
active launcher sequences must not conflict with each other, built-in navigation
sequences, `G`, or a configured normal key. Set a built-in's `enabled` to
`false` to remove it. These settings do not change the separate tmux opener
options `@agenmux-key` and `@agenmux-popup-key`.

Precedence per field: explicit CLI mode > present canonical `@agenmux-*`
option > present legacy `@agents-mon-*` option > file > defaults. Every supplied
layer is validated, even when shadowed. Legacy behavioral options are **not**
copied into canonical options. Explicit empty CLI mode (bootstrap) means use
the resolved mode; other CLI modes must be `split` or `popup`.

An empty canonical option blocks legacy and file values: width resets to
30/40, height to auto, display to split, notifications to on, wheel delay to
300 ms, and picker to the unfiltered picker. Launcher options are separate:
either empty launcher option disables installation. An absent picker setting
never takes over your binding. Notifications accept on/off, true/false, yes/no,
1/0 (case insensitive, surrounding whitespace
ignored). Compatibility display additionally accepts `float`; wheel options
accept finite seconds in 0..=60 or `off`. Width/height integers are 1..=10000
cells, clamped to available space.

`keys.prefix` is rejected as unknown; configure opening keys in tmux instead.
The application still manages panes, private navigation/search tables, mouse
bindings, hooks and the explicitly configured window picker. Setup snapshots
its touched bindings and attempts restoration on failure; rollback errors are
reported too. This is not an atomic tmux transaction.

Width, wheel delay and notifications retain live tmux overrides using the
startup file snapshot. Invalid live overrides retain the last valid settings
with bounded, deduplicated diagnostics. Border dragging writes only the live
`@agenmux-width` override; unsetting it returns to the startup file/default.
Teardown, key/click/wheel delivery, notification-open and existing-popup close
remain available with an invalid file. Ctrl-C/Ctrl-D remain emergency popup
exit paths; split close removes the sidebar panes, not your agent panes.

The TOML file is data, not code: no includes, expansion, shell commands or
agent hooks. Existing `agents/*.conf` and their `SUBJECT_CMD` hooks remain
separate **trusted executable customization**. Bootstrap installs opening
bindings even with an engine installation pending, and verifies the engine
before handing application setup/activation to Rust. The installer uses the
verified engine's internal `notification-eligible` command (0 enabled, 3 disabled, 1 read failure, 2 invalid)
and skips helper installation for every nonzero result.

## Updating

When a newer release exists, the sidebar header says so, and says what to do:

```text
agenmux v0.1.7 ↑0.1.8
u update · / search
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
- From a shell: `target/release/agenmux update v0.1.5` (or `latest`).
  Rollbacks to older releases re-enter that release's own entrypoint, including
  its legacy toggle script when the target predates the Rust-only runtime.

## Desktop notifications

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

<details>
<summary>Focus detection details</summary>

The [tmux manual](https://man.openbsd.org/tmux.1#focus-events) notes that clients
may need to detach and attach again after this option changes. With focus events
off, agenmux conservatively suppresses a notification whenever any real tmux
client has the pane selected. With them on, it suppresses only when at least one
real client both selects the pane and reports itself focused; control-mode
clients are ignored.

</details>

Notifications are enabled by default. Disable them with:

```tmux
set -g @agenmux-notifications off
```

On macOS, agenmux sends notifications natively through
`UNUserNotificationCenter` via a small helper app built from this repo — no
Homebrew or other runtime dependency, and no setup: installing or updating
the plugin automatically places a signed, background-only `Agenmux.app`
into `~/Applications` (skipped while `@agenmux-notifications` is off).
macOS asks for permission with the first notification; allow **agenmux**
when prompted, or later under System Settings → Notifications → agenmux.
Denying keeps notifications fully silent — there is no fallback around your
choice. Plugin updates refresh the app automatically and the permission
survives.

<details>
<summary>Platform implementation and edge cases</summary>

To set up (or verify) permission right now instead of on first use:

```sh
make install-app
```

This assembles and installs the app, shows the permission prompt, waits for
your answer, and confirms with a test notification — or tells you
notifications are off and where to enable them.

Clicking a notification body activates your terminal (Ghostty, Kitty, iTerm2,
WezTerm, Apple Terminal, and Alacritty are recognized) and jumps the most
recently active real tmux client to the exact pane. Panes that no longer exist
are safe no-ops. Notifications play macOS's built-in `Glass` alert sound.

Without the installed app, agenmux falls back to the built-in `osascript`,
which displays notifications with the `Glass` sound but cannot handle clicks.

Each notification keeps a small helper process waiting for its click; after 24
hours the notification is closed and the helper exits, so clicks on older
entries do nothing. If several notifications are pending at once, macOS may
route a click to the newest helper only — the click is then ignored rather
than jumping to the wrong pane.

On Linux, agenmux uses the optional `notify-send` command when a `DISPLAY`
or `WAYLAND_DISPLAY` session is available; Linux notifications are
display-only. Without it, delivery is silently skipped—the rest of the plugin
has no additional runtime requirement. Delivery is best effort and never
interrupts the sidebar if a notifier is unavailable or permission is denied.
The operating system may require notification permission for the sender it
displays.

The sidebar or popup must remain open while the state transition occurs because
notifications use the existing monitor process; no extra daemon is installed.
A transition suppressed while focused is not delivered later merely because
focus moves away.

</details>

## CLI

The Rust binary is the complete runtime (`scan` is an alias for `list`):

```text
agenmux --version
agenmux scan|list|status
agenmux detect <conf> <screen-file> [title]
agenmux sidebar|daemon
agenmux key <name>
agenmux click <pane> <row> <client>
agenmux wheel <pane> <up|down>
agenmux setup
agenmux toggle [split|popup] [client]
agenmux pane-add [window]|pane-orphan|pane-pin|teardown
agenmux releases refresh
agenmux update [latest|vX.Y.Z]
agenmux notification-open <socket> <pane> <bundle>
```

`sidebar`, `daemon`, `key`, mouse, setup, pane lifecycle, and
`notification-open` are internal contracts called by the tmux integration;
the scanner and update commands are suitable for direct shell use.

## Adding / overriding agents

Drop a `.conf` in `~/.config/agenmux/agents/`. A file with the same name
as a built-in (see `agents/`) overrides only the keys it assigns; every other
key keeps its built-in value. Assigning an empty value (`KEY=""`) clears that
key, so a one-line file changes or removes a built-in icon:

```bash
# ~/.config/agenmux/agents/claude.conf
AGENT_ICON="✻"    # or AGENT_ICON="" for the name only
```

A new file name adds a custom agent. Example:

```bash
# ~/.config/agenmux/agents/aider.conf
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
AGENT_ICON=""                       # optional glyph or short string shown before the agent name
```

`AGENT_ICON` width is counted per character, like other sidebar text: Nerd Font
glyphs fit, but double-width characters such as emoji or CJK may misalign rows.

`CHECK_ORDER` tokens: `bt`/`bs` blocked title/screen, `wt`/`ws` working
title/screen, `is` idle screen. Order matters when states can look alike —
Claude Code checks working before blocked so an already-answered permission
prompt left on screen doesn't read as blocked.

The sidebar subject shown below an agent is resolved from the cleaned pane
title, then `SUBJECT_SCREEN`, then `SUBJECT_CMD`. The shell snippet can use
`$path`, the pane's working directory. The Rust engine parses these assignments
and executes `SUBJECT_CMD` through the shell when needed, so only install configs
you trust.

## Tests

```sh
tests/run.sh         # fast fixture and integration tests
tests/sanity.sh      # release smoke + source build (requires tmux 3.7)
make container-test # run the release/real-tmux sanity suite in Docker or Podman
make container-use  # open the current checkout in an interactive tmux harness
make container-install # run the public website installer in a clean tmux harness
make container-install-local # run this checkout's installer before site deployment
```

For live development without overwriting the installed release binary:

```sh
make dev-use                                # run this checkout on the host tmux server
make dev-docker                             # run this checkout with Pi in Docker
make dev-docker REF=master                  # clone and run GitHub master in Docker
make dev-docker REF=my-feature              # clone and run a pushed branch in Docker
make dev-docker TMUX_CONFIG=/path/to/tmux.conf # override host tmux config path
make dev-stop                               # restore the existing release binary
```

`dev-use` builds with mise-managed `rust@latest`. `dev-use` and `dev-stop`
preserve sidebar state. Debug builds show `agenmux dev (YYYY-MM-DD HH:MM)`
with the local build time. `dev-stop` restores the existing local release
binary; it does not download a newer GitHub release. Docker copies
`~/.tmux.conf` into the disposable container by default; set `TMUX_CONFIG` to override its path. Docker drops
host-specific `agenmux.tmux`/`agents-mon.tmux` bootstrap lines and runs the
installer wizard against a writable copy. It does not edit the host config.
Other host-only plugin paths need matching mounts.
The Docker image caches system tools, Rust, and Pi; named volumes cache Cargo
downloads and build output between runs.

The OCI harness accepts Docker or Podman (override detection with
`CONTAINER_ENGINE=podman`). `container-test` builds one pinned image containing
the exact checkout and tmux 3.7b, then runs the release and real-tmux sanity
checks. `container-use` bind-mounts the current checkout, builds
it in an isolated target directory, and attaches to a disposable tmux session
starting in a real shell with a mock Codex agent in a second window. The harness
enables tmux management; press `prefix + A` to exercise the sidebar, then use
`cc` for a window or `cs` for a session. `container-install` instead starts a
clean disposable HOME and runs the public website installer in its shell so its
prompts, clone, binary verification, tmux.conf update, and reload can be exercised
end to end. `container-install-local` uses this checkout's `install.sh` in the same
harness, allowing installer changes to be tested before the website is deployed.
GitHub Actions uses the baked image without a bind mount, so CI always
tests the baked commit. Network access is
required for the release smoke checks. Rust integration tests also create private
tmux servers for exact-client, pane lifecycle, setup, toggle, and release behavior.
The image runs under `C.UTF-8` so tmux preserves the sidebar's Unicode glyphs;
the terminal itself still renders them and must use a Nerd Font for private-use icons.

Any interactive container target can start from a local tmux config:

```sh
AGENMUX_CONTAINER_TMUX_CONF=~/.tmux.conf make container-use
```

The file is mounted read-only, copied into the disposable HOME, and never modified
on the host. The same variable works with `container-install` and
`container-install-local`; installer edits affect only the copy.
Host-specific `default-shell` and `default-command` entries are replaced with
`/bin/bash` and an empty default command inside the disposable copy, so macOS paths
such as `/opt/homebrew/bin/nu` cannot prevent Linux panes from starting.
Existing `agenmux`/`agents-mon`, TPM runner, and `@plugin` lines are also removed
from the disposable copy so the harness exercises a clean install and cannot retain
bindings or plugin-manager commands that point to host-only paths. Choose launcher
keys again when the installer prompts.

Five shell entrypoints remain: `agenmux.tmux` is TPM/pre-binary bootstrap,
`scripts/install-bin.sh` installs and verifies the engine,
`scripts/install-app.sh` packages the macOS notification app,
`scripts/version.sh` validates manifest/release versions, and
`scripts/container-entrypoint.sh` drives the OCI test harness. All plugin runtime
behavior lives in Rust.

Fixtures in `tests/fixtures/` are real `tmux capture-pane -p` dumps where
possible (`claude-*`, `codex-idle`, `pi-idle`) and synthetic reconstructions for
hard-to-trigger states (`*-blocked`, `oh-my-pi-blocked`, `opencode-*`,
`pi-working`). To improve accuracy, re-capture a real screen into a fixture:

```sh
tmux capture-pane -p -t <pane> > tests/fixtures/claude-blocked.txt
```

## Runtime architecture

The Rust engine is the sole runtime implementation. It runs the scan/sidebar
hot path with one persistent tmux control-mode connection. Pane output from the
attached session invalidates cached screens and triggers a scan no more often
than every 500 ms. Inventory and background sessions are still reconciled every
two seconds, and attached-session screens are recaptured at least every ten
seconds; silence is never treated as an agent state. A key arriving mid-scan
stops the capture loop: panes not yet recaptured keep their last screen and
are refreshed on the next output scan. Direct `scan`/`list`
commands always take a fresh snapshot. The plugin downloads and verifies a
prebuilt binary automatically; if one is unavailable and
[cargo](https://rustup.rs) is installed, it builds the engine in the background. `make build` does the same
by hand, and `@agenmux-bin` overrides the binary path. Agent detection stays
in `agents/*.conf`, so adding or tuning agents never needs a rebuild. Building
on macOS needs rustc 1.90 or newer (for the native notification helper).

Sidebar (`split`) mode creates and live-renders the focused window before open
returns. Other windows receive processless panes lazily when a real client visits
them, so startup cost does not grow with hidden-window count. These panes have no
shell or `agenmux` child (`pane_pid=0`); the single daemon scans the global
inventory but writes frames only to windows currently visible in attached clients.
With every client detached, one active pane stays warm for the next attach.

GitHub Actions also builds ready-to-use plugin archives for x86_64 and ARM64 on
Linux and macOS. The Linux binaries are statically linked for portability.
Download the archive for your platform from the
[latest GitHub Release](https://github.com/snirt/agenmux/releases/latest)
and extract it; its native engine is already installed at
`target/release/agenmux`.
Each release includes `SHA256SUMS` for verification. Builds from untagged commits
remain available as temporary artifacts on their **Build and Release** workflow
run.

## Troubleshooting

The sidebar daemon keeps its diagnostics in `$XDG_STATE_HOME/agenmux/daemon.log`
(`~/.local/state/agenmux/daemon.log` when the variable is unset). The file is
owner-only and starts over on every sidebar launch and past 256 KiB. The
generation before it is kept as `daemon.log.1`; anything older is removed, and
a daemon that wrote nothing removes the `.1` as well. Look there first when the sidebar
disappears or a configuration reload does nothing: every `agenmux: ...`
message the daemon would have printed is in it.

For detection bugs and timing questions, enable the trace. It is off by default
and only costs anything when enabled:

```tmux
set -g @agenmux-debug /tmp/agenmux-trace.log   # then reopen the sidebar
```

`AGENMUX_DEBUG=<file>` does the same for direct commands (`agenmux scan`) and
wins over the option when both are set. Each line carries the writing process,
a UTC clock, and the number of the scan it belongs to:

```text
[4242] 12:34:56.789 s17 # scan 23ms reason=output captured=1 reused=3
[4242] 12:34:56.790 s17 # state %5 claude working->idle shown=working ticks=1
[4242] 12:34:56.802 s17 3ms capture-pane -b 'agenmux-4242' -t '%5' -> ok 0B ""
```

Per-scan lines record the trigger, duration, and how many screens were
captured versus reused; per-command lines record every tmux round trip; `state`
lines record each pane's detected state, what the sidebar shows, and the
debounce tick that held a transition back. Release builds never write captured
screen text to the trace. Debug builds (`make dev-use`) add `detect` lines with the
pane title and the last two screen lines, which is what tuning an
`agents/*.conf` rule needs. Review and sanitize either file before attaching it
to an issue: paths, session names, and titles come from your own panes.

## Known limits

- After a tmux server restart through a session-restore tool, restored sidebar
  panes may return as idle shells. Press `prefix+A` to remove them and reopen
  the sidebar. The original per-window layout cannot be recovered.
- State is inferred from what's on screen; transient redraws can flicker
  (the sidebar debounces transitions to idle by one tick).
- Pane titles are only used when the agent's OSC title escapes reach tmux.
- Desktop notifications are local to the tmux host; headless and remote hosts
  without a desktop notification service silently skip delivery.
- No Windows support.

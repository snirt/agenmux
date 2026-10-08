# Installation

## Installer

```sh
curl -fsSL https://snirt.github.io/agenmux/install.sh | sh
```

The script clones the plugin to `~/.tmux/plugins/agenmux` at the latest
stable release tag (`AGENMUX_REF=<branch|tag>` picks another ref), creates
`~/.config/agenmux/agents/`, shows a Nerd Font sample and asks whether it
renders (yes sets `display.agent_label = "icon"`, no sets `"text"`; an existing
`agent_label` is kept, and `AGENMUX_AGENT_LABEL` answers without a prompt),
asks for the launcher keys (default `prefix + A`
sidebar, `prefix + a` popup), shows the lines it wants in your tmux.conf (a
`@plugin` entry if you use TPM, `run-shell` otherwise), writes them once you
confirm, and reloads tmux. It also offers to symlink the `agenmux` command into
`~/.local/bin` and prints the PATH line to add if that directory is not on
your PATH. When tmux is running, it first installs the native
engine with a progress indicator, so the first toggle opens at once; otherwise
the engine installs in the background when tmux starts. Run it again to
update: a release checkout moves to the newest stable tag, a branch checkout
fast-forwards. An existing agenmux entry is left alone.

## TPM

Install with [TPM](https://github.com/tmux-plugins/tpm) directly:

```tmux
set -g @plugin 'snirt/agenmux'
```

Press `prefix + I` to install, then `prefix + A` to open the sidebar. The
plugin downloads and verifies the Rust engine for your platform in the
background. If you toggle before installation
finishes, that first activation waits for the same installer; a failed download
or build is reported in tmux instead of running an unverified fallback. After
TPM updates, the native engine is refreshed without removing the old binary.

## Manual install

Clone the repo and add `run-shell /path/to/agenmux/agenmux.tmux` to
`~/.tmux.conf`, then reload tmux.

Requirements: tmux and bash for TPM/bootstrap. `curl` and `tar` enable the
automatic native download; without them, Cargo builds it when available. No
required build step on a supported release platform. A Nerd Font is recommended
for private-use UI icons; the interactive installer prints a visual font check,
warns what to do when its sample icon appears as a box or blank, and picks the
agent row label from your answer.

## Upgrading from agents-mon

Existing installs keep working. `agents-mon.tmux`,
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

## Updating

When a newer release exists, the sidebar header says so, and says what to do:

```text
agenmux v0.7.0 ↑0.7.1
U update · / search
```

### Automatic updates

Auto-update is on by default (`[behavior] auto_update = true`). At most once a
day, while a sidebar or popup is open, agenmux downloads and verifies the
newest stable release in the background. Nothing running changes: the header
shows `↑0.7.1 ready`, and the version picker says `v0.7.1 ready · next start`.

The prepared release is switched to on the **next fresh start**: the next
sidebar or popup launch while no agenmux view is running from this install on
any tmux server. Adding a sidebar to a running view, moving within a popup,
`config reload`, `list`, `status` and closing the view never switch. The switch
is local (no network) and swaps source, engine and notification helper
together. If the new release fails to start, the previous one is restored and
opened instead, and that release is not retried automatically until a newer
one appears or auto-update is turned back on.

Auto-update skips, with the reason shown in the version picker:

- development checkouts (not exactly on a release tag), dirty git trees, and a
  custom `@agenmux-bin` or debug engine;
- tarball installs whose files differ from the installed release (checked
  against that release's verified archive once, then against the recorded
  baseline).

Turn it off with the `auto-update` row at the top of the version picker (`U`),
the settings view (`s`), or `auto_update = false` in `config.toml`; a release
already prepared is then not applied. Choosing a version by hand in the picker
pauses auto-update the same way (written to `config.toml`), so a rollback
sticks; turn it back on from the picker's `auto-update` row or in settings.

Preparation state lives beside the plugin in
`<plugins dir>/.agenmux-state/<plugin dir name>/`. Deleting that directory
while agenmux is closed discards a prepared release; an interrupted switch is
recovered from it on the next start.

### Choosing a version

Press `U` to open the version picker, choose a release, and press `Enter`.
The plugin switches its source *and* its native engine to that release and
reopens itself — so the same key rolls **back** to an older release just as
easily. A manual switch first closes this tmux server's view; if another tmux
server or popup still runs agenmux from the same install, it reopens the view
and asks you to close the other one first.

Details worth knowing:

- `Cargo.toml` is the only version source. The engine installed is always the
  one matching the checked-out source, so the two can never drift apart.
- On a git install (TPM or a manual clone) a switch is `git checkout <tag>`,
  leaving the checkout detached at that tag — the normal pinned-plugin state.
  It **refuses to run against a dirty working tree**; commit or stash first.
- On a tarball install the verified release archive is extracted in place.
- TPM's `prefix + U` still works and moves you to the tip of the default branch.
- From a shell: `target/release/agenmux update v0.6.2` (or `latest`).
  Rollbacks to older releases re-enter that release's own entrypoint, including
  its legacy toggle script when the target predates the Rust-only runtime.

## Release archives

GitHub Actions builds ready-to-use plugin archives for x86_64 and ARM64 on
Linux and macOS. The Linux binaries are statically linked for portability.
Download the archive for your platform from the
[latest GitHub Release](https://github.com/snirt/agenmux/releases/latest)
and extract it; its native engine is already installed at
`target/release/agenmux`.
Each release includes `SHA256SUMS` for verification. Builds from untagged commits
remain available as temporary artifacts on their **Build and Release** workflow
run.

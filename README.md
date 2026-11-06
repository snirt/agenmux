# agenmux

[![agenmux logo](site/logo.png)](https://snirt.github.io/agenmux/)

Website: <https://snirt.github.io/agenmux/>

A terminal multiplexer manager in a sidebar that also follows your AI coding
agents. Every session, window, and pane appears in order; jump, create, rename,
and delete from the keyboard or mouse. It currently supports tmux and
[Rex](https://www.superlogical.com/rex/) (beta).

https://github.com/user-attachments/assets/3e27f143-3fb3-474a-8cfd-3568112fe2af

Video not playing? [Watch it in the agenmux YouTube playlist](https://www.youtube.com/playlist?list=PLHITZpg1gd8c).

Each agent shows its state inline:

<img src="site/states.svg" width="680" alt="Agent states: red blinking ⣿ blocked, waiting for your input (permission prompt, menu); yellow spinner working, actively running; green blinking ⣿ done, finished while you were elsewhere, clears when you view it; green ⣿ idle, waiting at the prompt">

Supported out of the box: **Claude Code, Codex, Hermes, Oh My Pi, OpenCode, and
Pi**. Adding an agent is one small config file — no code. Detection reads each
pane's process tree, screen, and title; no hooks to install, nothing runs
inside your agents. In Rex, agents that report their own state are read from
that report. A status-line segment and desktop notifications tell you
when an agent needs you.

## Quick start

```sh
curl -fsSL https://snirt.github.io/agenmux/install.sh | sh
```

The installer clones agenmux once to `~/.local/share/agenmux` for every
supported multiplexer, runs a Nerd Font check, lets you pick the multiplexers,
asks for launcher keys, adds agenmux to each one's config once you confirm, and
reloads it. Run it again to update, or to add or remove a multiplexer.

For tmux alone, [TPM](https://github.com/tmux-plugins/tpm) works too:

```tmux
set -g @plugin 'snirt/agenmux'
```

Press `prefix + I` to install, then `prefix + A` to open the sidebar. The native
engine for your platform downloads and verifies in the background.

### Rex

agenmux also runs in [Rex](https://www.superlogical.com/rex/) (beta), on macOS:
Rex has no Linux client yet. With Rex installed, the installer asks for the Rex
keys (`alt+s` and `alt+p` by default), adds the `init.lua` line that loads
agenmux once you confirm, and reloads Rex. Rex alone needs no tmux;
`AGENMUX_MUX=tmux`, `rex` or `both` picks the setup. See [docs/rex.md](docs/rex.md)
for setup by hand and what differs from tmux.

### Manual install

Clone the repo, add `run-shell /path/to/agenmux/agenmux.tmux` to `~/.tmux.conf`,
and reload tmux. For Rex, add the `init.lua` line from
[docs/rex.md](docs/rex.md#setup). Requirements, release archives, and updating
are in [docs/installation.md](docs/installation.md).

### Upgrading from agents-mon

`agents-mon` is now **agenmux**. Legacy options, formats, environment
variables, and config paths are still accepted; see
[docs/installation.md](docs/installation.md#upgrading-from-agents-mon).

## Usage

Press `prefix + A` in tmux, or `alt+s` in Rex, to open the sidebar. Move with
`j`/`k`, jump with `Enter`, and press `?` for every key. In tmux, add
`#{agenmux}` to `status-right` for a compact count of agents by state.

[docs/usage.md](docs/usage.md) has the full key table, search, tmux management,
quick launchers, popup mode, the status line, and the agent-only view.

## Configuration

tmux options (`@agenmux-*`) set launcher keys and overrides; in Rex, keys live
in the `init.lua` line. `config.toml` holds display, behavior, theme, and keymap
settings for both. Press `s` in the sidebar to
edit settings in place, or edit the file and run `agenmux config reload`.
`agenmux config --help` lists every key; [`examples/config.toml`](examples/config.toml)
is a complete annotated example. See [docs/configuration.md](docs/configuration.md).

## Adding / overriding agents

Drop a `.conf` in `~/.config/agenmux/agents/`. A file named after a built-in in
`agents/` overrides only the keys it sets; a new name adds an agent. See
[docs/custom-agents.md](docs/custom-agents.md).

## Troubleshooting

Start with the daemon log at `~/.local/state/agenmux/daemon.log`
(`$XDG_STATE_HOME/agenmux/daemon.log` when set). Detection tracing and known
limits are in [docs/troubleshooting.md](docs/troubleshooting.md).

## Documentation

- [Installation and updating](docs/installation.md)
- [Usage](docs/usage.md)
- [Configuration](docs/configuration.md)
- [Desktop notifications](docs/notifications.md)
- [Rex](docs/rex.md)
- [Adding / overriding agents](docs/custom-agents.md)
- [Troubleshooting](docs/troubleshooting.md)
- [Development](docs/development.md): tests, dev harnesses, runtime architecture
- [Contributing](CONTRIBUTING.md)

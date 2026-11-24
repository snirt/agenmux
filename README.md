# agenmux

**A clean, ready-to-use multiplexer manager for tmux and Rex.**

Search and jump to any session, window, or pane. Keep the sidebar open while you work, or bring up a floating popup only when you need it.

Create and rename sessions and windows, close several items at once, or restore recently closed layouts and working directories. If you run coding agents, their status appears alongside your panes, with no hooks to install.

https://github.com/user-attachments/assets/3e27f143-3fb3-474a-8cfd-3568112fe2af

[More demos](https://www.youtube.com/playlist?list=PLHITZpg1gd8c) · [Project website](https://snirt.github.io/agenmux/)

## Quick start

```sh
curl -fsSL https://snirt.github.io/agenmux/install.sh | sh
```

Open the sidebar with `prefix + A` in tmux or `Alt+S` in Rex. Press `/` to search, `Enter` to jump, and `u` to see what you recently closed. Rex support is in beta on macOS.

See [installation options](docs/installation.md) for TPM, manual setup, and upgrading from agents-mon.

## Make it yours

Launch your own commands from the selected pane’s directory, change the keybindings, or switch to an agents-only view.

- [Usage and popup shortcuts](docs/usage.md)
- [Configuration](docs/configuration.md) and [Rex setup](docs/rex.md)
- [Desktop notifications](docs/notifications.md) and [custom agents](docs/custom-agents.md)
- [Troubleshooting](docs/troubleshooting.md), [development](docs/development.md), and [contributing](CONTRIBUTING.md)

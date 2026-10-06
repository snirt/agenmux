# agenmux v0.7.1

## What's changed

v0.7.1 smooths sidebar navigation, keeps wide characters from breaking rows,
makes the command-line help readable, lists every pane in a session, and
offers to put `agenmux` on your PATH.

### Sidebar

- Holding `j`/`k` no longer makes the cursor jump: split sidebars hide the terminal cursor, frames use synchronized output so tmux never draws one half-finished, and scans wait while a key is held ([#154](https://github.com/snirt/agenmux/pull/154)).
- Rows containing East Asian wide characters or emoji are clipped by terminal cells, so they no longer wrap and push the rows below down ([#159](https://github.com/snirt/agenmux/pull/159)).

### Command line

- `agenmux -h`, `--help` and `help` print grouped help with a description for each command; internal commands are listed separately ([#166](https://github.com/snirt/agenmux/pull/166)).
- `agenmux config` lines every key up in key / values / default columns and lists each key action with its default bindings ([#166](https://github.com/snirt/agenmux/pull/166)).
- `agenmux list` filters by session (`-s`), agent (`-a`) or running command (`-c`); a session filter lists every pane in it, not just agents ([#168](https://github.com/snirt/agenmux/pull/168)).

### Installer

- The installer offers to link `~/.local/bin/agenmux` to the engine, and if `~/.local/bin` is not on PATH it prints the line to add ([#153](https://github.com/snirt/agenmux/pull/153)).

### Documentation

- The README is now a short landing page; the full reference moved to `docs/` ([#156](https://github.com/snirt/agenmux/pull/156), [#163](https://github.com/snirt/agenmux/pull/163)).
- The website and README show the [agenmux intro video](https://youtu.be/iRlmKR6Y9aE) in place of the animated demo ([#171](https://github.com/snirt/agenmux/pull/171), [#172](https://github.com/snirt/agenmux/pull/172)).

### Assets

- Linux x86_64
- Linux aarch64
- macOS x86_64
- macOS aarch64
- SHA-256 checksums

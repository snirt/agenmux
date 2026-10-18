# agenmux v0.8.4

## What's changed

v0.8.4 adds settings to stop the animation and use plain-text glyphs, fixes layout restore on tmux 3.8, hardens the terminal against panics and control keys, and stops pasted text and modified keys from leaking into commands.

### Motion and glyphs

- `display.animate = false` stops blinking and the spinner: blocked and done stay lit, working shows a steady glyph, and the 250 ms redraw stops ([#219](https://github.com/snirt/agenmux/pull/219), [#213](https://github.com/snirt/agenmux/issues/213)).
- `display.glyphs = "plain"` replaces font-dependent glyphs with plain text in the sidebar, the help legend and the `#{agenmux}` status segment: `!` blocked, `>` working, `*` done, `.` idle. Agent icons that are not plain ASCII are hidden and Nerd Font window icons become `▢` ([#219](https://github.com/snirt/agenmux/pull/219)).
- Both settings are editable from Settings ([#219](https://github.com/snirt/agenmux/pull/219)).

### tmux 3.8

- Closing the sidebar restores the window's original layout again. tmux 3.8 reports layouts as JSON, and the size check before restoring did not understand that format, so the restore was silently skipped ([#221](https://github.com/snirt/agenmux/pull/221), [#220](https://github.com/snirt/agenmux/issues/220)).

### Terminal

- A panic now restores the terminal settings, turns off bracketed paste and shows the cursor before exiting, so the shell is usable afterwards ([#217](https://github.com/snirt/agenmux/pull/217), [#206](https://github.com/snirt/agenmux/issues/206)).
- While the sidebar is open, Ctrl-Z, Ctrl-\, Ctrl-S and Ctrl-O no longer suspend, kill or freeze it. Ctrl-C and Ctrl-D still cancel or exit as before ([#217](https://github.com/snirt/agenmux/pull/217)).

### Input

- Modified keys such as Ctrl-Up no longer leak characters like `5A` into search, and unknown escape sequences do nothing ([#215](https://github.com/snirt/agenmux/pull/215), [#205](https://github.com/snirt/agenmux/issues/205)).
- Bracketed paste is ignored in normal mode instead of running as commands, and is inserted as one edit in text fields ([#215](https://github.com/snirt/agenmux/pull/215)).

### Overlays

- Recently closed (`u`) and settings (`s`) footers show your configured keys, and hide actions that are unbound ([#218](https://github.com/snirt/agenmux/pull/218), [#212](https://github.com/snirt/agenmux/issues/212)).
- `d` in recently closed now asks `forget …? y/N` before removing an entry from restore history; only `y` forgets ([#218](https://github.com/snirt/agenmux/pull/218)).

### Assets

- Linux x86_64
- Linux aarch64
- macOS x86_64
- macOS aarch64
- SHA-256 checksums

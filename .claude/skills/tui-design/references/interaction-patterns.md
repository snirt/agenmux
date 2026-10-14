# Interaction patterns — keybindings, focus, navigation, modal vs modeless

A deep dive into how users interact with TUIs — keybinding philosophy, focus management, navigation patterns, mouse support, and discoverability. The top-level `SKILL.md` covers the principles; this file goes deeper into the *why* and the trade-offs.

**Contents:**
- [Keybinding philosophies](#keybinding-philosophies)
- [Cross-app keybinding conventions](#cross-app-keybinding-conventions)
- [Reserved keys — preserve what users expect](#reserved-keys--preserve-what-users-expect)
- [Discoverability — the four-layer pattern](#discoverability--the-four-layer-pattern)
- [Modal vs modeless — the deeper trade-off](#modal-vs-modeless--the-deeper-trade-off)
- [Focus management](#focus-management)
- [Search and filter](#search-and-filter)
- [Multi-select](#multi-select)
- [Mouse support — the real trade-off](#mouse-support--the-real-trade-off)
- [Undo / redo](#undo--redo) · [Confirmation patterns](#confirmation-patterns)
- [Forms and settings screens](#forms-and-settings-screens)
- [Terminal modes and protocols](#terminal-modes-and-protocols) (paste, keyboard, mouse, focus, sync output, cursor, title, progress, color queries, terminal environments)
- [Talking to the terminal emulator — OSC 8, 52, 9](#talking-to-the-terminal-emulator--osc-8-52-9)
- Named patterns: [fzf](#the-fzf-pattern-in-detail) · [lazygit](#the-lazygit-pattern--multi-pane-with-numeric-tabs) · [k9s](#the-k9s-pattern--command-driven) · [helix](#the-helix-pattern--selection-first-modal-editing)
- [Common interaction pitfalls](#common-interaction-pitfalls)

---

## Keybinding philosophies

There are four major schools. Most apps blend them.

### 1. Vim-style (modal)

**Identity:** modes (NORMAL, INSERT, VISUAL, COMMAND), single-letter motions and operators in NORMAL mode, leader keys for namespaces.

**Examples:** vim, neovim, helix, kakoune, k9s (ex-mode), aerc.

**Strengths:**
- Densely expressive — `5dw` ("delete five words") is an operator plus a motion with a count.
- Hands stay on home row; minimal modifier-key gymnastics.
- Power users get fast.

**Weaknesses:**
- Steep learning curve.
- "What mode am I in?" confusion if mode indicator is weak.
- Modal text input fights conventional editing (Ctrl+A, Ctrl+E, etc.).

**When to use:** editor-like tools where keystrokes are the primary interaction, users are likely to be vim-familiar, you want dense expressivity. Not appropriate for general-purpose tools targeting non-vim users.

### 2. Emacs-style (chord)

**Identity:** modifier-prefixed bindings (`C-x C-s` to save, `M-x` for command), no modes, deeply nested key tables.

**Examples:** emacs (the originator), readline-based shells, weechat (default), some bash bindings.

**Strengths:**
- No modes; what you press is what you get.
- Rich modifier space (`C-`, `M-`, `S-`, combinations).
- Discoverable with which-key.

**Weaknesses:**
- Modifier strain ("Emacs pinky").
- Conflicts with terminal-reserved keys.
- Less efficient than modal for repetitive editing.

**When to use:** rarely from scratch in 2026. Most Emacs-style apps inherit the convention from a long history. New apps usually go modal or modeless.

### 3. Arrow-key / GUI-like (modeless)

**Identity:** arrow keys + Enter + Tab + Esc + single-letter shortcuts. Mouse-first or mouse-friendly.

**Examples:** btop, htop (somewhat), fzf, gum, Posting, Harlequin, most modern Textual apps.

**Strengths:**
- Zero learning curve.
- Inclusive — works for users who've never touched vim.
- Pairs naturally with mouse support.

**Weaknesses:**
- Slower for power users.
- Less expressive — fewer keys means fewer commands.
- Modifier keys (Ctrl/Alt) for less-common actions.

**When to use:** general-purpose tools, dashboards, monitors, anything where you want broad usability.

### 4. Hybrid (modeless + vim motions)

**Identity:** on navigational surfaces, support arrows and compatible `hjkl` aliases simultaneously. Vim shortcuts are power-user paths, never requirements, and printable aliases yield while a text field owns input.

**Examples:** lazygit, k9s, yazi, gh dash, most modern TUIs.

**Strengths:**
- Best of both worlds — onboarding via arrows, speed via hjkl.
- Inclusive without sacrificing power.
- Familiar to users from many backgrounds.

**Weaknesses:**
- Slightly more code (handle both).
- Risk of inconsistency between which keys do what.

**When to use:** a strong default for new navigational, full-screen TUIs. A bounded prompt or form can keep a smaller conventional keymap.

---

## Cross-app keybinding conventions

These have crystallized across the ecosystem. Use them unless you have a strong reason not to:

| Key | Action |
|---|---|
| `q` | quit |
| `?` | help |
| `/` | search / filter |
| `n` / `N` | next / prev match |
| `Esc` | cancel / back / dismiss |
| `Enter` / `Return` | confirm / drill in |
| `Space` | toggle / mark for multi-select |
| `:` | command mode |
| `gg` | top |
| `G` | bottom |
| `Tab` / `Shift+Tab` | switch focus |
| `r` | refresh |
| `1`–`9` | jump to panel / numbered tab |
| `hjkl` *and* arrows | move in navigational views when text entry does not conflict |
| `Ctrl+P` | command palette, when the app includes one |
| `y` | yank / copy |
| `p` | paste / push |
| `d` | delete (often confirms first) |
| `e` | edit |
| `o` | open |

**Pick a paradigm and stay consistent.** Don't mix `Ctrl+S` and `:w` for save unless both clearly map to "write file."

---

## Reserved keys — preserve what users expect

In cooked (line) mode these keys belong to the terminal driver and shell. In raw mode — which every full-screen framework enables — they arrive as ordinary keypresses: no SIGINT, SIGTSTP, or XOFF is generated unless your app produces that behavior itself.

| Key | Expected behavior |
|---|---|
| **Ctrl+C** | Interrupt — cancel the current operation or quit cleanly |
| **Ctrl+Z** | Suspend — restore the terminal, stop, and redraw on `fg` |
| **Ctrl+\\** | Abort — SIGQUIT (core dump) in cooked mode |
| **Ctrl+S** / **Ctrl+Q** | XON/XOFF flow control, for line-mode programs where the tty driver still sees them |

A full-screen app *may* handle these keys, and real ones do: helix binds `Ctrl-c` to toggle comments and `Ctrl-s` to `save_selection`; nano and micro save on `Ctrl+S`. What you owe users is the expected behavior, not a hands-off policy: keep `Ctrl+C` as cancel/quit unless there's a strong reason not to (and then offer an obvious exit), and implement `Ctrl+Z` suspend through the framework's support so the terminal is restored. Flow-control freezes (and the `stty -ixon` workaround) only matter for cooked-mode programs; raw mode as set by `cfmakeraw` also clears `IXON`.

Ctrl+H is sometimes Backspace, sometimes a free key — it depends on terminal config. Test before binding.

---

## Discoverability — the four-layer pattern

Every action a user can take should be discoverable through *at least one* of these layers, in order of immediacy:

### Layer 1: Always-visible footer hints

For a full-screen app, keep the 3–5 most-useful shortcuts visible at the bottom and update them by context. A one-step inline prompt can show its controls beside the prompt instead of reserving a footer row.

For a complex full-screen app, this is usually the strongest discoverability tool. Most users will not begin in the docs; they read what is visible in context.

Examples:
- htop's F1–F10 strip.
- lazygit's per-pane hints (`space stage  c commit  P push  p pull  ?`).
- helix's status line.

Auto-generation is a force multiplier:
- **Bubble Tea**: `bubbles/help` reads from `key.KeyMap`.
- **Textual**: `Footer` widget renders `BINDINGS`.
- **Ratatui**: not auto-generated; build a helper that reads from your keybinding map.
- **Ink**: not auto-generated; build with `<Box>` + the same keymap source.

Define keys *once*, derive the footer.

### Layer 2: `?` help screen

Pressing `?` opens a modal or full-screen help with **all** keybindings, grouped by context.

Every full-screen app with more than a handful of actions should provide this or an equivalent discoverable help view. A tiny picker whose complete keymap is already visible does not need a separate modal. lazygit's `?` shows a categorized list of keys, with sections per panel. Textual's `?` (when implemented) typically shows a key table.

Format: `key  action  context`. Group by mode/panel/category.

### Layer 3: Leader-key / which-key

After pressing a leader (Space, `,`, `\`), show a popup with available follow-up keys. The user can:
- Read the list and pick.
- Type the key to dismiss the popup and trigger the action.
- Press Esc to cancel.

**The gold standard:** helix's `Space-` menu. Press Space, see `f` (file picker), `b` (buffer picker), `s` (symbols), `a` (LSP actions), etc. `which-key.nvim` is near-universal in neovim.

When to add: when your keymap exceeds ~20 distinct actions. Below that, `?` help is enough.

### Layer 4: Command palette

`Ctrl+P` (or similar) opens a fuzzy-matched action list. The user types to filter.

Add a palette when the action set is large, spans multiple contexts, or benefits from search; a small app can stop at footer hints plus `?`. **Once a palette exists, every action that has a keybinding should also be a palette command.** The keybinding is the shortcut; the palette is the long-form name + searchable description.

Examples:
- Textual's built-in command palette (`Ctrl+P` by default since v0.77; it originally shipped on `Ctrl+\`, which collides with SIGQUIT).
- VS Code's Ctrl+Shift+P (the conceptual ancestor for many TUIs).
- helix's `:` (ex-style command, not strictly a palette but similar role).
- k9s's `:` (resource-typing, but adopts the same "type to filter actions" model).

Format each command:
```
Action name              keybinding
Description / context
```

Show the keybinding next to the command name — the palette doubles as keybinding documentation.

---

## Modal vs modeless — the deeper trade-off

### Modal (vim-style)

You're in a mode (NORMAL, INSERT, etc.). Keystrokes interpreted differently per mode.

**Requires:**
- Persistent mode indicator (status bar color or label).
- Distinct cursor shapes per mode (block in NORMAL, bar in INSERT, underline in REPLACE), reset on exit (see *Terminal modes and protocols* → *Output*).
- A way to learn modes — usually a tutorial or readme.

**Trade-offs:**
- Pro: dense keybindings (no Ctrl/Alt needed for most operations).
- Pro: composable operators (`d` + motion = delete-by-motion).
- Con: confusion if mode is weakly indicated.
- Con: muscle memory required.

### Modeless

What you press is what you get. State changes happen via widget focus.

**Requires:**
- Clear focus indication (border color, reverse video on selection).
- Modifier-key bindings for less-common actions.

**Trade-offs:**
- Pro: zero learning curve.
- Pro: no mode confusion.
- Con: less expressive — limited by Ctrl/Alt combinations.
- Con: more visual real estate spent on focus indicators.

### The third option: contextual

Bindings change based on which panel is focused. Not "modes" in the vim sense — more like "the current panel determines what `space` means."

Examples: lazygit (`space` stages in Files, checks out in Branches, applies in Stash), k9s (panel-specific actions in addition to global ones).

This is **context-sensitive modeless** — works well if you have rich, panel-specific actions and a footer hint bar that shows what's available right now.

---

## Focus management

In a multi-panel TUI, **which panel has focus** determines:
- Which keystrokes the panel responds to.
- Which footer hints show.
- The visual highlight (border color, reverse video on selection).

### Focus indicators (in order of strength)

1. **Border color change** — strongest. The focused panel's border becomes the accent color.
2. **Border weight** — thin → thick. Subtle but works.
3. **Title color/weight** — bold + accent on focused panel title.
4. **Background tint** — subtle but be careful (background-leak issues with borders).
5. **Selection visible** — only the focused panel shows reverse-video selection; unfocused panels show muted selection.

Combine 2–3 of these for unambiguous indication.

### Focus navigation

| Method | Best for |
|---|---|
| **Tab / Shift+Tab** cycle | Linear panel order; small number of panels |
| **Numeric keys** (`1`–`9`) | Direct jumps; many panels or tabs (lazygit `1`–`5` panels, yazi `1`–`9` tabs) |
| **Directional** (`Ctrl+w h/j/k/l` like vim) | Spatial navigation; arbitrary panel arrangement |
| **Mouse click** | Augmentation; not the primary path |

**Numeric jumps are fastest** when you have 5+ panels. Tab is fine for 2–3.

### Focus traps

Some panels should "trap" focus — Tab inside them cycles through internal widgets, not back to the global panel cycle. Modal dialogs and forms always trap focus. Use Esc to escape.

---

## Search and filter

Two distinct interactions:

### Search (forward through content)

Bound to `/`. Opens a search prompt; `Enter` jumps to first match; `n` / `N` cycles. Examples: vim's `/`, less's `/`, lazygit's `/`.

- Show match count: `(2/15)`.
- Highlight all matches; focus the current one with reverse video or accent.
- `Esc` cancels and returns to original position.

### Filter (narrow what's shown)

Bound to `/` (when search isn't needed) or another key. Filters the list to matching items. Examples: fzf's whole identity, k9s's `/`, Textual's `Input` widget on a list.

- Sub-100ms response — anything slower feels broken.
- Show count: `123/45678`.
- Highlight matched substring within results.
- `Esc` clears filter.

**Smart-case** is a kind courtesy: lowercase query is case-insensitive; mixed-case is case-sensitive. fzf and fd default to this; ripgrep offers it as `-S`/`--smart-case`.

---

## Multi-select

Universal pattern: **Space toggles** the row's marked state. Marked rows show a `*` prefix or accent background.

Variants:
- **Visual mode** (`v`): yazi and vim — extend selection with motion keys.
- **Range select** (Shift+Click or Shift+arrow): from current to clicked.
- **Select all** (`Ctrl+A`): mark everything — with the caveat that `Ctrl+A` is GNU screen's prefix (and a common tmux rebinding) and readline's start-of-line, so keep an alternative binding.
- **Invert** (`*` or another printable key): swap marked/unmarked. Avoid `Ctrl+I` — in the legacy encoding it's indistinguishable from Tab (only the Kitty keyboard protocol tells them apart), and Tab is your focus-cycle key.

After multi-selecting, an action key applies to all marked items: `d` deletes all marked, `y` yanks all, etc.

---

## Mouse support — the real trade-off

The fight between camps:

### Pro-mouse

- Textual treats mouse as first-class with `:hover`, click, scroll wheel.
- btop makes every highlighted key clickable and scrolls lists with the wheel.
- lazygit selects on click, range-selects by dragging, and reorders commits by dragging (v0.64+).
- Modern users expect to be able to click things.

### Anti-mouse

- vim purists, and keyboard-first users of any tool (helix and fzf turn the mouse on by default but never require it).
- Mouse capture takes over click-and-drag text selection; users must hold a bypass modifier (Shift in most terminals, Option in iTerm2, Fn in Terminal.app).
- Adds complexity without speed benefit for power users.

### The pragmatic compromise

**Mouse augments keyboard, never replaces required actions.** Every mouse-reachable target needs a keyboard equivalent.

Implement mouse for:
- Clicking a panel to focus it.
- Clicking a tab to switch.
- Scrolling lists / viewports.
- Clicking buttons in forms.

Don't require mouse for:
- Anything in the critical path.
- Power-user actions.
- Anything that should work over a serial console or restricted SSH.

**Document the selection bypass** for your users' terminals, since the modifier isn't always Shift, and offer a way to turn mouse capture off. Inside tmux 3.8+, the bypass is needed even for apps that never enable the mouse (*Terminal environments*). When mouse capture is on, regular click-drag selects via the app, not via the terminal. Users learn this pattern from one app and transfer it.

---

## Undo / redo

Hard for non-editor apps, but lazygit notably offers `z` (undo) / `Z` (redo) for git actions recorded in the reflog — commits, checkouts, resets, even a whole botched rebase. It is honest about the limits: working-tree and stash changes, pushes, and branch creation can't be undone, and undo is unavailable mid-rebase. Even bounded, this is a *huge* user comfort feature for destructive tools.

**Two implementation strategies:**

1. **Action stack** — model every user action as a reversible operation. On undo, pop and reverse.
2. **State snapshots** — periodically snapshot full state; undo restores a snapshot. Cheaper to implement but coarser.

For destructive actions without undo, use modal confirmation. **Never silently destroy.**

---

## Confirmation patterns

Three levels of friction:

### Light: y/n prompt

```
Delete 3 files? [y/N]
```

Default to **No** — pressing Enter shouldn't delete.

### Medium: typed name

```
This will delete the production database.
Type the database name to confirm: prod-main
```

Used by Heroku for app deletion. Forces the user to *think* about what they're deleting.

### Heavy: dual confirmation

```
Step 1: Type "delete" to confirm
> delete

Step 2: Are you sure? [y/N]
```

For nuclear actions. Used rarely.

**Match friction to consequence.** Y/n for "stage all files" is fine. Typed name for "delete production." Don't make every confirmation typed-name — users learn to just type fast and lose the safety.

---

## Forms and settings screens

### Validation timing — avoid premature errors

Default to validation on **blur** or **submit** so an incomplete value is not treated as an error while the user is still typing. huh's `Input` validates when a field loses focus and again on Next/Submit — there's no per-keystroke validation path in its implementation. Clack and Inquirer validate only on Enter (there's no blur concept in a single-prompt-at-a-time flow). After an error has been shown, live revalidation can help it disappear as soon as the value becomes valid; expensive or remote checks should be debounced or reserved for submit.

Textual's `Input.validate_on` accepts `"blur"`, `"changed"`, and `"submitted"`, and **defaults to all three**. Narrow it to `validate_on=["blur"]` or `["submitted"]` when errors should wait; keep `"changed"` only when immediate feedback is useful and non-disruptive.

### Required-field marking — there is no framework convention

None of huh, Textual, or Clack/Inquirer ship a built-in "required field" visual marker (no asterisk-for-required the way GUI toolkits do). **Don't assume one exists — this is an easy claim to get wrong by GUI pattern-matching.** In particular, huh's `" *"` suffix is its **error indicator**, shown after a validation failure, not a required-field marker; there's no `Required()` API. If a design needs required fields marked, do it manually with label text such as `(required)` or an explained `*`; color may reinforce the marker but must not carry the meaning alone.

### Dynamic/conditional fields

Show or hide dependent fields as earlier answers change, and recompute choices from those answers. Check what your framework ships before designing around it: Huh hides whole groups, not single fields (`ecosystem-go.md` → *Huh*), and Textual hides a dependent widget from a reactive watcher (`ecosystem-python.md` → *Reactive state*).

### Settings-screen behavior — be honest about what's settled

There's no single widely adopted TUI convention for settings persistence. Choose from the behavior of the underlying operation rather than presenting one pattern as universal:

- **Live-apply** (htop's F2 setup screen): changes take effect immediately as you make them; persistence to disk happens implicitly, not via an explicit Save action. No dirty-state indicator, no discard.
- **Explicit Apply / Save / Cancel**: appropriate when changes must be validated together, committed atomically, trigger an expensive reload, or update a remote system. Show dirty state, make `Esc` behavior explicit, and confirm before discarding meaningful edits.
- **Defer to `$EDITOR`** (lazygit): no in-app settings form at all — press `e` in the Status panel to open the raw config file in the user's editor. Since v0.44, lazygit reloads the changed file when its terminal regains focus; only a few settings (refresh and fetch intervals, update checks) still need a restart, and lazygit says which. If you take this route, reload on focus or file change rather than demanding a restart.

There is no settled convention for Esc-to-discard on a dirty form. Design and document it explicitly, and match confirmation friction to the cost of losing the edits.

---

## Terminal modes and protocols

Full-screen apps switch terminal modes on with DECSET (`CSI ? n h`) and must switch each one off (`CSI ? n l`) on every exit path, including errors, panics, and suspend. That is the lifecycle contract in `SKILL.md`. Frameworks restore what they enable, but nothing you enable yourself: Ratatui's `restore()` undoes only raw mode and the alternate screen. Treat every capability query as optional. A terminal or multiplexer may answer late or never, so render a working default first and wait for replies with a timeout. xterm's [control sequences](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html) define the classic modes; newer ones have their own specs, linked below.

### What each framework turns on

| Mode | Bubble Tea v2 | crossterm / Ratatui | Textual | Ink 8 |
|---|---|---|---|---|
| Bracketed paste (2004) | On | Off | On | While a paste hook is mounted |
| Kitty keyboard | On (disambiguate), plus `modifyOtherKeys` | Off | On (disambiguate, all keys, associated text) | Opt-in |
| Mouse reporting | Off | Off | On, all motion | None |
| Focus reports (1004) | Off | Off | On | None |
| Synchronized output (2026) | When the terminal reports it; not queried over SSH unless `TERM` names a known terminal | The app's job | When reported; never inline | Always, on a TTY |
| Grapheme clusters (2027) | Same query and SSH rule as 2026 | None | None | None |

The ecosystem references own the API names.

### Input

- **Bracketed paste (2004).** The terminal wraps a paste in `CSI 200~` … `CSI 201~`, so the app can tell pasted text from typing. Without it, kitty and Ghostty turn a pasted newline into Enter, and the app may run half a paste as commands. Enable it for any free-text input, and decide per field what a multi-line paste means: a single-line field should keep the first line or refuse, never submit. xterm, kitty, Ghostty, and foot neutralize escape characters inside a paste so it cannot end itself early; don't assume every terminal does.
- **Kitty keyboard protocol.** Push enhancement flags with `CSI > flags u` and pop them with `CSI < u` on exit; `CSI ? u` asks whether the terminal supports it ([spec](https://sw.kovidgoyal.net/kitty/keyboard-protocol/)). The first flag, "disambiguate", makes Shift+Enter, Ctrl+Enter, Ctrl+Tab, and Ctrl+I distinct from their legacy twins and sends plain Esc as its own code, so the app no longer guesses whether Esc starts an Alt sequence. Flag 2 adds key-release events. kitty, foot, Alacritty 0.13+, Ghostty, Windows Terminal 1.25+, and WezTerm (with `enable_kitty_keyboard`) support it; xterm's `modifyOtherKeys` (`CSI > 4 ; 2 m`) covers some of the same keys. Treat the extra keys as accelerators. Every action still needs a binding that works in the legacy encoding, which is what reaches you inside tmux, whose `extended-keys` option is off by default.
- **Mouse reporting.** Mode 1000 reports clicks, 1002 adds drags, and 1003 adds every pointer move. Pair any of them with 1006 (SGR encoding), which reports releases and columns past 223. Enable the smallest mode the UI needs, since 1003 sends an event on every pointer move. crossterm's `EnableMouseCapture` turns on all of them at once. Any mouse mode takes over the terminal's own text selection; see *Mouse support* for the bypass and the off switch.
- **Focus reports (1004).** The terminal sends `CSI I` and `CSI O` when its window gains or loses focus. Use them to pause animation and polling in the background and to refresh stale data on return; lazygit dims its panels on focus loss and refreshes on focus. tmux forwards them only with `focus-events on`, which is off by default.

### Output

- **Synchronized output (2026).** Wrap each frame in `CSI ? 2026 h` … `CSI ? 2026 l` so the terminal paints it at once instead of tearing mid-redraw ([spec](https://github.com/contour-terminal/vt-extensions/blob/05050a11e793c8f4362bf4e34a59ed3f7e5105fe/synchronized-output.md)), and detect support with DECRQM (`CSI ? 2026 $ p`). Always send the end sequence, error paths included; a terminal releases a stuck frame only after its own timeout (150 ms in Alacritty, 1 s in Ghostty, 2 s in kitty). kitty, WezTerm, foot, Alacritty, Ghostty, iTerm2, Windows Terminal 1.23.20211+, and tmux 3.7+ support it. Without it, write each frame in one `write`, send only changed cells, and hide the cursor while drawing.
- **Cursor shape (DECSCUSR, `CSI n SP q`).** Pair shapes with modes (block in normal mode, bar in insert; see *Modal vs modeless*), and reset with `CSI 0 SP q` on exit. Windows Terminal, foot, and Ghostty read 0 as the user's configured cursor, but xterm reads it as a blinking block; only xterm patch 411 and later restore the user's choice, with `CSI 7 SP q`. The current shape can't be reliably queried, so it can't be saved. Ratatui's `restore()` doesn't touch the shape, so an app that sets one resets it.
- **Window title (OSC 2).** Set a title only when it helps users find the window, for example the open file or the remote host. Push the old title with `CSI 22 ; 0 t` before setting yours and pop it with `CSI 23 ; 0 t` on exit. xterm, kitty, foot, iTerm2, Alacritty, and tmux keep that stack; WezTerm, Ghostty, and Windows Terminal ignore it, so the user's title is lost there. Bubble Tea clears the title on exit rather than restoring it.
- **Progress (OSC 9;4).** `OSC 9 ; 4 ; state ; percent ST` shows progress on the tab or taskbar; state 0 removes it, 1 is normal, 2 error, 3 indeterminate, and 4 paused ([ConEmu](https://conemu.github.io/en/AnsiEscapeCodes.html#ConEmu_specific_OSC), [Windows Terminal](https://learn.microsoft.com/en-us/windows/terminal/tutorials/progress-bar-sequences)). Windows Terminal, Ghostty 1.2+, kitty, iTerm2, and tmux 3.7+ understand it. A terminal that knows only OSC 9 notifications may pop it up as a notification, so send it where you know it is supported, and clear it with state 0 on every exit. Bubble Tea's view-level progress bar sends it without checking.
- **Shell-integration marks (OSC 133).** Prompt marks (`A` prompt, `B` input, `C` output, `D;exit-code` end) let a terminal jump between prompts, select one command's output, and show its exit status ([proposal](https://gitlab.freedesktop.org/Per_Bothner/specifications/-/blob/6b555699e41ab47ce210610d1a17cfc1662ec8d7/proposals/semantic-prompts.md)). A REPL should mark its own prompts, adding `aid=` when it runs inside a shell that marks them too; a full-screen TUI should not.
- **Grapheme clusters (2027).** With mode 2027 on, the terminal sizes cells by grapheme cluster, so emoji sequences and combining marks take the width the app computed ([spec](https://github.com/contour-terminal/contour/blob/b719d05aed54f29ee7d629f5dd7d760b2f4b4320/docs/vt-extensions/unicode-core.md)). Bubble Tea turns it on when the terminal reports support; the other frameworks don't. Where it is off, app and terminal can still disagree; kitty, for one, segments graphemes without implementing 2027. Test emoji-heavy rows on the terminals you support.

### Color and appearance

- **Color depth.** `COLORTERM=truecolor` (or `24bit`) is the [convention](https://github.com/termstandard/colors/blob/caf0bb949ad64307d938a055f7fbec76ce65d91e/README.md#truecolor-detection) for 24-bit color; `TERM` alone proves little, since most terminals call themselves `xterm-256color`. Libraries read it differently: chalk 6 accepts only `truecolor` (with `24bit` it falls back to whatever `TERM` suggests), and crossterm consults `TERM` only when `COLORTERM` is unset. The variable is also the first thing lost over SSH. OpenSSH forwards no environment variables by default, and only recent Debian and Ubuntu releases send and accept `COLORTERM` at both ends. tmux 3.6+ sets `COLORTERM=truecolor` in its panes and downsamples for the outer terminal. To recover truecolor remotely, ask the terminal (XTGETTCAP `RGB`, which Bubble Tea's capability request sends) or let users set it; otherwise fall back to 256 colors over the 16-color theme from `visual-patterns.md` → *Color in depth*.
- **Background color (OSC 11).** `OSC 11 ; ? ST` returns the background as `rgb:rrrr/gggg/bbbb`. The Linux console, PuTTY, mosh, GNU Screen, and ConEmu never answer, so follow it with a query every terminal answers (DA1, `CSI c`, or the status report `CSI 5 n`). If that reply arrives first, no background reply is coming. Cap the wait anyway (Neovim uses 100 ms, lazygit 500 ms for slow SSH, Lip Gloss 2 s), render with a default palette meanwhile, and switch when the answer arrives. tmux 3.4+ answers OSC 11 itself with the attached client's colors.
- **Dark/light changes (2031).** After `CSI ? 2031 h`, the terminal reports a theme switch as `CSI ? 997 ; 1 n` (dark) or `CSI ? 997 ; 2 n` (light), and `CSI ? 996 n` asks for the current theme ([spec](https://github.com/contour-terminal/contour/blob/ab57ab3288c3dca5ff727311cb3f1401bac30549/docs/vt-extensions/color-palette-update-notifications.md)). kitty 0.38.1+, Ghostty, foot 1.23+, and iTerm2 3.7+ support it; WezTerm does not; tmux 3.8 forwards the outer terminal's theme. Probe with DECRQM before enabling it, re-query OSC 11 when a report arrives instead of trusting the report alone, as lazygit and Neovim do, and turn the mode off before handing the terminal to a child process. Bubble Tea passes the parsed reports through without enabling the mode; Textual, Ink, and Ratatui don't handle it.

### Terminal environments

The same app runs inside multiplexers, over SSH, and on Windows consoles. Each changes which keys arrive and which modes work:

| Environment | What breaks | Detect | Fallback |
|---|---|---|---|
| tmux | The prefix `Ctrl+B` never reaches the app. A lone Esc waits `escape-time` before delivery (500 ms before 3.5, 10 ms since). Since 3.8 the `mouse` option defaults to on: unless the app enables mouse reporting, a plain drag starts tmux's copy-mode selection and the wheel enters copy mode (except on the alternate screen), while Ctrl-drag, Alt-drag, and Alt+right-click always belong to tmux | `$TMUX`, `TERM_PROGRAM=tmux` | Never make `Ctrl+B` the only path to an action; document the `tmux.conf` options below |
| GNU screen | The prefix `Ctrl+A` never reaches the app. Truecolor is dropped unless `truecolor on` (5.0+); OSC sequences other than titles and colors are discarded; the alternate screen is off by default. The default flow control (`auto`) still passes Ctrl+S and Ctrl+Q to raw-mode apps on Linux, but `flow on` swallows them | `$STY`, `TERM=screen*` | 256-color theme; no OSC-dependent feature without a fallback; clear the screen on exit |
| Zellij | Its default mode captures `Ctrl+G/P/T/N/H/S/O/Q/B` and many Alt keys. The Unlock-First preset (0.41+) frees the Ctrl keys but keeps its Alt bindings | `$ZELLIJ` | Keep a non-Ctrl, non-Alt path to every action; point users at Unlock-First |
| SSH | Every capability query costs a network round trip; `COLORTERM` usually doesn't arrive; the remote may lack terminfo for the local terminal (kitty's `xterm-kitty`, Ghostty's entry); OSC 52 is the only clipboard path | `$SSH_TTY`, `$SSH_CONNECTION` | Bounded query timeouts, truecolor from a query or a user setting, `TERM=xterm-256color` when terminfo is missing (kitty's `kitten ssh` and Ghostty's SSH integration copy it over) |
| Windows | No SIGTSTP, SIGWINCH, or SIGCONT: no job-control suspend, and resize arrives as console events. The legacy console has no VT processing before the Windows 10 Anniversary Update. Windows Terminal sets no `TERM` or `COLORTERM` | `WT_SESSION` (Windows Terminal) | A non-suspend path to the shell; target Windows Terminal, the default since Windows 11 22H2. Go, Rust, and Python write Unicode through the wide console API, so no `chcp 65001` is needed |
| CI, pipes | No TTY: no prompts, no full-screen UI | `CI`, stdout not a terminal | `cli-basics.md` → *Interactivity* and *Output design* |

Sources: [tmux.1](https://github.com/tmux/tmux/blob/3.8/tmux.1) and [CHANGES](https://github.com/tmux/tmux/blob/3.8/CHANGES) at 3.8, [screen.1](https://git.savannah.gnu.org/cgit/screen.git/tree/src/doc/screen.1?h=v.5.0.2) at 5.0.2, [Zellij's keybinding presets](https://zellij.dev/documentation/keybinding-presets.html), [Microsoft's console VT notes](https://devblogs.microsoft.com/commandline/new-experimental-console-features/), and the [Windows 11 default-terminal announcement](https://devblogs.microsoft.com/commandline/windows-terminal-is-now-the-default-in-windows-11/).

**tmux options.** tmux answers or forwards most of these itself: synchronized output and progress (3.7+), hyperlinks (3.4+), the clipboard (`set-clipboard`), focus reports (`focus-events on`), extended keys (`extended-keys`), and the dark/light theme (3.8). Notifications and image protocols need DCS passthrough (`ESC P tmux; … ESC \` with every inner ESC doubled) and `allow-passthrough on`, which is off by default ([tmux.1](https://github.com/tmux/tmux/blob/3.8/tmux.1)). Several of these depend on options that are off by default, so document the `tmux.conf` lines your app needs.

---

## Talking to the terminal emulator — OSC 8, 52, 9

Some interactions aren't between the user and your app — they're between your app and the *terminal emulator*: open a link, set the clipboard, pop a desktop notification. OSC escape sequences carry these requests in-band, down the same byte stream as your output. That's their superpower: they traverse SSH, containers, and nested shells, because the emulator on the user's *local* machine interprets them — no X11 forwarding, no remote clipboard daemon.

### OSC 8 — hyperlinks

`ESC ] 8 ; params ; URI ESC \` … link text … `ESC ] 8 ; ; ESC \`. The optional `id=` param stitches one logical link across wrapped lines or split regions so all its cells underline together on hover; simple filters shouldn't set ids, full-screen apps that manage the screen should.

**Support:** iTerm2, kitty, WezTerm, Ghostty, foot, Windows Terminal (1.4+), Alacritty (0.11+), tmux 3.4+. Degradation is graceful — a terminal that doesn't know OSC 8 ignores it and renders the visible text normally — but strip it when stdout isn't a TTY, or the raw bytes land in files and pipes.

**Libraries:** Lipgloss v2 has `Style.Hyperlink(url)`; Rich/Textual support links via the `link` style attribute. **Ratatui has no native support** — escape sequences inside `Text`/`Span` break its per-cell width accounting (open issue #1227); workarounds (the `hyperrat` crate, the `tui-link` repo) exist but work around the buffer model rather than fixing it.

**Over SSH, remember the link opens on the local machine.** A `file://` URI pointing at a remote path won't resolve. For "open this file," suspend and spawn `$EDITOR` on the remote side instead; save OSC 8 for `https://` URLs and the like.

### OSC 52 — write the system clipboard

`ESC ] 52 ; c ; <base64 payload> ESC \` asks the emulator to set the system clipboard — from anywhere, including over SSH. Practical limits: the common ceiling comes from xterm/hterm's 100,000-byte maximum sequence length, ≈74,994 bytes of decoded text; older kitty builds capped far lower. Keep payloads to tens of KB or chunk them.

**tmux:** `set-clipboard` controls forwarding — `on` lets inner apps set the outer clipboard; `external` (the default since 2.6) reserves that for tmux itself. Forwarding needs tmux to know the outer terminal handles OSC 52: an `Ms` terminfo entry, or (tmux 3.6+) the terminal's device-attributes reply. tmux understands OSC 52 natively — it does **not** need `allow-passthrough`.

**Security:** writing is the safe half. Clipboard *reads* are how a malicious remote exfiltrates data, so most terminals disable or prompt on them (kitty prompts; WezTerm ignores queries; Alacritty disabled paste-back by default in 0.13). Alacritty has ignored OSC 52 from unfocused windows since 0.11, and other terminals may apply similar focus gates, so a background job cannot count on the write landing either. Design for write-only, and treat the write as best-effort.

**Libraries:** crossterm 0.29 added OSC 52 copy; Bubble Tea v2 ships `tea.SetClipboard`. Still provide a local fallback (`pbcopy` / `xclip` / `wl-copy`, or Rust's `arboard`) — OS clipboard APIs are more reliable when you're not over SSH, and some terminals disable OSC 52 entirely.

### OSC 9 / 777 — desktop notifications

`ESC ] 9 ; message ESC \` (OSC 9, iTerm2-originated) has the widest support: kitty, WezTerm, Ghostty, iTerm2, foot. Never start the message with digits and a semicolon: `OSC 9;n` is also ConEmu's command namespace (`9;4` is a progress bar), parsed by Windows Terminal, Ghostty, and tmux 3.7+, so `3; tests failed` becomes a command or gets dropped. Prefix the text (`build: 3 tests failed`) or use OSC 777. OSC 777 (`ESC ] 777 ; notify ; title ; body ESC \`) adds a title where supported (WezTerm, Ghostty, foot); a reasonable strategy is 777 where you know it works, else 9. tmux swallows both unless wrapped in DCS passthrough with `allow-passthrough on` (tmux 3.3+).

**Etiquette:** notify only when long-running work finishes (or fails) while the user is likely elsewhere — build done, tests finished, deploy complete. Fire once, keep it short, prefer in-UI status when the app has focus. Never notify for routine events or things the user just did.

### The rest of the family, in one line each

- **OSC 11** (background color) and **mode 2031** (dark/light changes) — see *Terminal modes and protocols* → *Color and appearance*.
- **OSC 9;4** (progress) and **OSC 133** (prompt marks) — see *Terminal modes and protocols* → *Output*.
- **Bracketed paste (mode 2004)** — see *Terminal modes and protocols* → *Input*.

---

## The fzf pattern in detail

The instant-filter interaction has spread far beyond fzf itself: telescope.nvim, atuin, zoxide, helix's `Space+f`, and Textual's command palette match fuzzily; k9s's `/` (regex) and lazygit's `/` (substring by default) keep the instant filter with a stricter matcher.

The pattern:

1. **Open** with a key (often `/` or `Ctrl+P`).
2. **Type to filter** — shows results updating in real time.
3. **Arrow / `Ctrl+J` / `Ctrl+K`** to navigate while the query owns printable input. Reserve bare `hjkl` for an explicit navigation mode or a view with no active text field.
4. **Tab** to mark for multi-select (in `--multi` mode).
5. **Enter** to confirm; **Esc** to cancel.
6. **Optional preview pane** (right side) showing detail of hovered result.

Implementation rules: everything in *Search and filter* → *Filter* (sub-100ms updates, match count, highlighting, smart case), plus **sensible result ranking** — exact matches first, then prefix, then substring.

For Go: `sahilm/fuzzy` or `lithammer/fuzzysearch`. For Rust: `nucleo` (used by helix) or `skim`'s matcher. For Python: `rapidfuzz`. For TS/JS: `fuse.js` or `fzy.js`.

**Case study:** `references/exemplar-apps.md` → *fzf*.

---

## The lazygit pattern — multi-pane with numeric tabs

Used by: lazygit, lazydocker (and inspired by mc / Norton Commander).

**Recipe:**
1. 5+ persistent panels in fixed positions.
2. `1`–`9` keys jump directly to numbered panels.
3. `Tab` cycles in order.
4. **Single letters trigger panel-specific actions** — `c` does something different in each panel.
5. **Footer hint bar updates per panel** — shows which actions `c` and friends do *right now*.

**The bet:** users tolerate context-sensitive single letters in exchange for dense bindings, *because the footer always shows what each key does in the current context.*

**The trade-off:** higher cognitive load — `c` does N different things. The footer hint bar is what makes it work.

**Case study:** `references/exemplar-apps.md` → *lazygit*.

---

## The k9s pattern — command-driven

Used by: k9s, helix (`:` ex-commands), weechat (`/` slash-commands), aerc.

**Recipe:**
1. Press `:` to enter command mode.
2. Type a resource type or command name (`pods`, `nodes`, `svc`, `ingress`).
3. Tab-completion narrows.
4. Enter runs.
5. Aliases for common commands (`po` = `pods`).

**Pros:** fast for power users, scales to many commands.

**Cons:** demands tab-completion or a discoverability layer — k9s suggests completions as you type in `:` mode and lists every alias in its own view (`Ctrl-A`). Without that, users won't know what's possible.

**When to use:** when there are many resource types or actions, and users will repeat them often.

**Case study:** `references/exemplar-apps.md` → *k9s*.

---

## The helix pattern — selection-first modal editing

Helix's bet against vim: select first, then act. `wd` selects-word-then-deletes (vs vim's `dw` "delete-word"). Multi-cursor is core. Tree-sitter integration. `Space` opens which-key.

**Why it matters for general TUI design:**
- **Selection-then-act** has visual feedback before the destructive action — users see what they're about to delete.
- **Multi-cursor** as primary scales beautifully for batch edits.
- **Which-key for leader keys** is a discoverability pattern any modal app should consider.

You probably won't build a modal editor, but the *visual-feedback-before-action* principle is widely applicable: highlight the rows you're about to delete, the field about to be cleared, the URL about to be visited.

**Case study:** `references/exemplar-apps.md` → *helix*.

---

## Common interaction pitfalls

1. **Broken reserved-key expectations** — Ctrl+C that neither cancels nor quits, Ctrl+Z that doesn't suspend cleanly.
2. **No focus indication.** Users tab around guessing which panel is active.
3. **Mode confusion** (modal apps with weak indicators). Use cursor shape + color.
4. **`q` doesn't quit in a modeless full-screen app** (outside text entry), without offering another visible conventional exit.
5. **`Esc` doesn't back up or dismiss** where the UI establishes a navigation stack or modal.
6. **No discoverable key help** in an action-rich app; `?` is conventional, but an equivalent visible route is fine.
7. **Filter that's slow** (>100ms updates).
8. **Destructive single-letter without confirmation or undo.** Either confirm or implement undo.
9. **Inconsistent meanings for the same key across panels** without a footer hint bar to explain.
10. **Mouse-only actions** with no keyboard equivalent.
11. **Tab order that skips around** (not following visual reading order).
12. **No multi-select where users need it** (file managers, lists of items to act on).

When reviewing a TUI's interaction design, walk this list. Most existing apps fail on 4–6 items.

# Visual patterns — borders, color, density, layout

A deep dive into the visual design choices that make TUIs feel professional. The top-level `SKILL.md` covers the principles; this file goes deeper with the *why*, the *when*, and the trade-offs.

**Contents:**
- [The seven canonical layouts in detail](#the-seven-canonical-layouts-in-detail)
- [Inline, alt-screen, or overlay — where the UI lives](#inline-alt-screen-or-overlay--where-the-ui-lives)
- [Borders — when, what, and why](#borders--when-what-and-why)
- [Color in depth](#color-in-depth)
- [Typography in monospace](#typography-in-monospace)
- [Density: pack vs pad](#density-pack-vs-pad) (includes the clutter audit)
- [Responsive design — breakpoints and the floor](#responsive-design--breakpoints-and-the-floor)
- [Visual hierarchy in monospace](#visual-hierarchy-in-monospace)
- [Tables and lists](#tables-and-lists)
- [Localization](#localization) (translation growth, right-to-left text, ambiguous-width characters, locale formatting)
- [Status bars, headers, footers](#status-bars-headers-footers)
- [Progress indicators](#progress-indicators) (includes disconnected/stale-data and timeout/cancel UX)
- [Theming systems](#theming-systems) (includes [icons and Nerd Fonts](#icons-and-nerd-fonts--there-is-no-detection-only-opt-in))
- [Accessibility](#accessibility) (screen readers, the plain-mode contract, framework support, motion)
- [Common visual pitfalls](#common-visual-pitfalls)

---

## The seven canonical layouts in detail

### 1. Persistent multi-panel

All panels visible simultaneously in fixed positions. Focus shifts via Tab or numeric keys. The user builds spatial memory: "files are top-left, branches are below, diff is on the right."

**Examples:** lazygit (5 left panels + 1 right), btop (full-width CPU on top; memory above network on the left, processes on the right), htop (header / process list / F-key footer).

**When to use:**
- Users want to see multiple related views at once without switching.
- The data is observed at a glance (monitors, dashboards).
- Numeric panel jumps make sense (`1`–`5`).

**When to avoid:**
- Each pane needs full attention (use IDE three-panel or drill-down stack instead).
- Total information would overwhelm at smaller terminal sizes.

**Implementation notes:**
- Use border *color* to indicate the focused panel — strongest possible signal.
- Footer hint bar should be **panel-aware**: `c` does different things in lazygit's Files vs Commits panel; show what's relevant.
- Numeric jumps (`1`–`5`) should work from any panel — don't make users return to a "menu" first.

### 2. Miller columns

Three (sometimes more) columns: parent → current → preview. `h`/`l` (or `←`/`→`) ascend/descend.

**Examples:** yazi (default ratio `[1, 4, 3]` for parent/current/preview), ranger (the original), broot (tree-mode variant), nnn, lf.

**When to use:**
- Hierarchical data: filesystems, JSON, K8s resource trees.
- Users need to see context (parent) and outcome (preview) without losing position.

**When to avoid:**
- Narrow terminals (<80 cols) — degrades poorly. Provide a single-pane fallback.
- Non-hierarchical data — the parent column is wasted.

**Implementation notes:**
- The middle column should be widest — that's where the user works.
- Preview column shows file content / nested object / pod logs depending on what's selected.
- Selection in the middle column updates the preview live (debounce if expensive).

### 3. Drill-down stack

Browser-style: navigate into deeper views with a back-stack, `Esc` returns to the previous view.

**Examples:** k9s (`:pods` → Enter on pod → `Esc` back), lazydocker (sidebar → drill into container → `Esc`), gh dash (PR list → PR detail).

**When to use:**
- Many resource types where the user needs to pivot between them.
- Each detail view is full-screen and demands attention.
- Command-mode navigation is natural (`:resource`).

**When to avoid:**
- Users need to compare two things side-by-side (use multi-panel).
- The back-stack would get deep (>3 levels confuses users).

**Implementation notes:**
- Status bar should show the current "address" (breadcrumb): `cluster/namespace/pods/my-pod-7d9f`.
- `Esc` always backs up one level. The user should never feel trapped.
- Implement a stack of view-state objects. On push, save the current view; on pop, restore.

### 4. Widget dashboard

Independent widgets in a grid, each owning its data lifecycle. Layout is configurable.

**Examples:** bottom (btm), btop, glances, gtop. Configurability varies: bottom defines rows and column ratios in TOML; btop toggles boxes and layout presets in `btop.conf`; glances uses an INI file; gtop has no layout config.

**When to use:**
- Monitoring/observability where users want to compose their own view.
- Each widget has its own update cadence.

**When to avoid:**
- Static data layouts — config overhead doesn't earn its keep.
- Users who want a single curated experience.

**Implementation notes:**
- Define a config schema (TOML/YAML) early. Users *will* want to customize.
- Each widget should be independently scrollable/expandable.
- Mouse-resize is rare; btop, for example, offers click-to-focus and scroll but no drag-resize.

### 5. IDE three-panel

Sidebar → main content → detail/output. The main panel often has tabs giving it multiple personalities.

**Examples:** Posting (collection tree → request editor → response), Harlequin (catalog → editor → results). helix is the counterexample: its file explorer is a picker, not a persistent sidebar.

**When to use:**
- Editor-like workflows where users compose then execute then inspect.
- A small set of "current items" needs prominent focus while keeping a navigator.

**When to avoid:**
- Pure browsing (no composition step) — multi-panel is simpler.
- Mobile-style narrow terminals — too many panels to fit.

**Implementation notes:**
- Sidebar collapsible (`Ctrl+B` or similar — but `Ctrl+B` is tmux's default prefix, so keep an alternative). Users will toggle it.
- Main panel tabs cycled with `[`/`]`. (`Ctrl+Tab` isn't reliably delivered without the kitty keyboard protocol or `modifyOtherKeys`.)
- Output panel often docked bottom or right; resizable.

### 6. Overlay / popup

Appears over the shell, does one thing, exits.

**Examples:** fzf (the canonical example), atuin (replaces Ctrl+R), zoxide+fzf for directory selection, gum prompts.

**When to use:**
- "Summon → choose → output → exit" interactions.
- Tools meant to compose with shell pipelines.
- Replacing a built-in like Ctrl+R reverse-search.

**When to avoid:**
- Long-running interactions — users will resent the cramped popup space.
- Anything that needs to persist state between invocations.

**Implementation notes:**
- Buffer choice (inline vs alt screen) and the exit contract are covered in full below — see *Inline, alt-screen, or overlay*.
- Startup time is part of the design, because users summon these dozens of times a day; budgets are in `cli-basics.md` → *Startup time*.

### 7. Tabbed within panel

Tab bars inside a larger layout, cycled with `[`/`]` (`Ctrl+Tab` only where the enhanced keyboard protocol is active).

**Examples:** lazygit's Local/Remotes/Tags tabs in the branches panel; lazydocker's Logs/Stats/Env/Config/Top tabs in the right pane.

**When to use:**
- One panel needs multiple personalities without changing the global layout.
- The tabs are related (different views of the same selected object).

**When to avoid:**
- Tabs are unrelated — that's a sign you actually need different panels.
- More than 5–6 tabs — split into nested tabs or use a different pattern.

**Implementation notes:**
- Active tab visually distinct (bold + underline + accent color).
- Tab key labels (`Logs [1]`, `Stats [2]`, `Env [3]`) so users can jump directly.
- Cycle order matches reading order (left to right).

---

## Inline, alt-screen, or overlay — where the UI lives

Before choosing a layout, choose a screen buffer. The **alternate screen** is a separate buffer with no scrollback; on exit the terminal restores whatever was there before. **Inline** rendering paints in the normal buffer, below the shell prompt, and scrolls with everything else.

**Default heuristic: alt screen for apps you *live in*; inline for tools you *summon*.** Editors, file managers, and dashboards — long-lived, navigational, full-screen — usually belong on the alt screen so they do not pollute scrollback. Prefer inline for bounded pickers, prompts, confirmations, and progress when preserving shell context matters. A picker with a large preview or unusually deep workflow may still earn a full-screen buffer; choose from the working set and exit contract, not the label alone.

### Mechanics per framework

All four frameworks support both modes. How each enters the alternate screen or stays inline, and how it prints permanent lines above an inline UI, is in its ecosystem reference: `ecosystem-go.md`, `ecosystem-rust.md`, and `ecosystem-python.md` → *Lifecycle and terminal handoff*, and `ecosystem-typescript.md` → *Strengths and weaknesses*.

### The fzf model — bounded inline viewport

fzf itself defaults to full-screen mode. Its explicit `--height 40%` mode renders the finder below the cursor so previous commands remain visible; fzf's shell keybindings commonly opt into this bounded mode. The UI paints on `/dev/tty` (stderr as fallback) so stdout stays clean — that's why `vim $(fzf)` works. Treat height-capped inline rendering as a strong option for fzf-like tools, not as a fact about every picker or fzf's default.

### The exit contract — the receipt pattern

Inside `$(...)` or a pipe, stdout is not a TTY; anything the UI paints to stdout becomes part of the captured "result." So: **chrome to stderr or `/dev/tty`, answer to stdout.** gum runs its Bubble Tea UI with `tea.WithOutput(os.Stderr)` and prints the chosen value to stdout, stripping ANSI when stdout isn't a TTY — which is exactly why `CHOICE=$(gum choose a b c)` composes.

On exit, do one of two respectable things: erase the transient UI completely (fzf's default), or replace it with a one-line receipt that stays in scrollback — `✓ deployed api-server in 12s`. Textual's `inline_no_clear` and fzf's `--no-clear` are the deliberate leave-the-last-frame escape hatches. What you must not do is leave a dead, half-drawn UI behind.

### Choose by workflow

| Workflow | Mode |
|---|---|
| One-shot pick / confirm / progress | Prefer inline; erase cleanly or leave a concise receipt |
| Explore / monitor / edit session | Alt screen; restore on exit |
| Hybrid (fzf-like; logs + live status) | Inline with a height cap (`--height 40%`, `Viewport::Inline`) |

---

## Borders — when, what, and why

### The Unicode block

The Unicode box-drawing block is U+2500–U+257F:
- **Single line**: `─ │ ┌ ┐ └ ┘ ├ ┤ ┬ ┴ ┼`
- **Heavy**: `━ ┃ ┏ ┓ ┗ ┛ ┣ ┫ ┳ ┻ ╋`
- **Double**: `═ ║ ╔ ╗ ╚ ╝ ╠ ╣ ╦ ╩ ╬`
- **Rounded**: `╭ ╮ ╰ ╯` (combined with single-line straights)

### The aesthetic landscape

- **Single-line** — default for most modern TUIs (lazygit, k9s, btop, htop). Reads cleanly without dominating.
- **Rounded corners** — the Charm aesthetic (Bubble Tea apps, gum, glow). Slightly softer; modern.
- **Heavy** — emphasis. Use sparingly — for the focused panel border, or a "hot" warning state.
- **Double** — reads as "DOS." Avoid for modern apps unless you're going for retro.

### When to use borders

- The pane has dynamic content needing a visible boundary.
- Focus state must be communicated visually (border color change is the strongest signal).
- Adjacent panels need clear separation.
- The border is doing real work — not just decoration.

### When to skip borders

- Content is static (htop has no internal borders — it's all dense layout).
- Density matters more than structure.
- The pane is implicitly bounded (only thing on screen, or at the edge).

Don't decorate — whitespace is usually separator enough (see *The clutter audit* below).

### The background-leak problem

Each terminal cell has only fg/bg colors. A border cell renders the line in the foreground but the cell's background "leaks" through behind the border characters. If your panel has a contrasting background to its parent, this looks wrong:

```
parent_bg has black background
panel has dark blue background
border characters: render in panel's blue background
        ^^^ creates a visible "step" between border and content
```

**Solutions:**
- Use the same background for panel and borders.
- Or use one-eighth block characters (`▏▕▔▁`) as borders — Textual does this for themed apps.

### ASCII fallback

For legacy SSH, Windows conhost, `TERM=dumb`, or any system where Unicode might be unreliable:
- Single-line `─│┌┐└┘├┤┬┴┼` → `-|+++++++++`
- Heavy → `=|+++` (or just bold the ASCII)
- Rounded `╭╮╰╯` → `+`

Detect it from the first non-empty variable among `$LC_ALL`, `$LC_CTYPE`, and `$LANG` (does it name UTF-8?), and via terminal capability queries. The ASCII set also sidesteps ambiguous-width box drawing; see *Localization*. Provide a config option (`--ascii`, `MYAPP_ASCII=1`) for explicit override.

---

## Color in depth

### The three tiers

Design in three layers: monochrome, 16 ANSI, then 256/truecolor. Use truecolor only when the terminal advertises it (`COLORTERM=truecolor` or `24bit`, or your framework's color-profile detection); `COLORTERM` often doesn't survive ssh or sudo, so let users force a tier (detection details: `interaction-patterns.md` → *Terminal modes and protocols* → *Color and appearance*). SKILL.md → *Meaning and access* sets the token and `NO_COLOR` rules this section builds on. The depth worth adding here: the user's terminal theme is sacred. The 16 ANSI colors are *theme variables* — the user's `red` might be `#ef5350` (Material), `#dc322f` (Solarized), or `#f38ba8` (Catppuccin). You design in terms of "red means error," not "use #ff0000 for errors."

### Semantic tokens

Define a vocabulary by *function*, not appearance:

```
status.success    → green
status.warning    → yellow
status.error      → red
status.info       → blue or cyan

text.primary      → default fg
text.muted        → dim or gray
text.emphasis     → bold + accent

bg.base           → default bg
bg.surface        → slightly elevated
bg.overlay        → modal/popup bg

accent.primary    → brand color
accent.secondary  → complementary

border.default    → muted
border.focus      → accent.primary

git.staged        → green
git.modified      → yellow
git.untracked     → red or cyan
git.added         → green (foreground)
git.deleted       → red (foreground)
```

Then map tokens to palette colors. **Lipgloss v2's `LightDark`** (`AdaptiveColor` in v1/compat), **Textual's CSS variables**, and **Ratatui's palette indirection** all implement this.

This indirection earns its keep when adding themes (Catppuccin Latte + Frappé + Macchiato + Mocha is one config file, not four code changes) or when supporting light/dark modes.

### Conventional meanings

| Color | Meaning |
|---|---|
| Green | Success, added, online, OK |
| Red | Error, deleted, danger, offline |
| Yellow | Warning, modified, pending, in-progress |
| Cyan / Blue | Info, paths, links, hints |
| Magenta | Special, highlights, attention without alarm |
| Dim / gray | Secondary, disabled, metadata, timestamps |
| Bold (any color) | Title, primary content, emphasis |

Stick to conventions. A green error message confuses everyone.

### Color and accessibility

About 8% of males have red-green color vision deficiency. Don't rely on color alone:

- Pair color with **letters**: lazygit shows file status as `M` (modified, yellow), `A` (added, green), `D` (deleted, red), `??` (untracked, cyan). Even if color is invisible, the letters carry meaning.
- Pair color with **symbols**: delta's `+` and `-` line prefixes work in monochrome.
- Pair color with **position**: errors at the end of output, success in a separate column.

**Safe color pairs for CVD:**
- Blue + orange.
- Blue + yellow.
- Black + white (and any high-contrast neutral pair).

Avoid red + green as the only distinction. If you must use them, add letters or symbols.

### Honor `NO_COLOR`

[no-color.org](https://no-color.org) is a 2018 informal standard. Respect it: when `NO_COLOR` is present with a non-empty value, suppress automatic color output. An explicit user choice such as `--color=always` may override that default if the tool documents the precedence.

`ripgrep`, `bat`, `eza`, `fd`, `gh`, `cargo`, and `htop` (3.5+) honor it. Library behavior varies: some detect it automatically, while others require an optional capability feature or an application-level gate. For example, `owo-colors` only consults terminal and environment support when its `supports-colors` feature and `if_supports_color` path are used. Make `NO_COLOR` part of the app's semantic color policy instead of assuming a styling dependency will enforce it.

Provide tool-specific `MYAPP_NO_COLOR` and `MYAPP_FORCE_COLOR` for finer control. Keep precedence consistent with the policy in `references/cli-basics.md` → *Color*.

---

## Typography in monospace

You can't change font size. Typography signals are limited but useful:

| Attribute | Use for | Notes |
|---|---|---|
| **Bold** | Titles, current selection, primary content, focused panel name | Universal support |
| **Dim** | Metadata, timestamps, disabled items, secondary text | Universal support |
| **Italic** | Light emphasis | Poorly supported; never the only signal |
| **Underline** | OSC 8 hyperlinks, shortcut hints | Wide support; meaningful |
| **Reverse video** | Cursor row, current selection | Works on every terminal back to VT100 |
| **Strikethrough** | Cancelled / deleted items | Limited support |
| **Blink** | Don't | Some modern terminals render it (WezTerm, kitty 0.43+); distracting and an accessibility hazard |

**Reverse video is the canonical "current selection" signal.** It's the most reliable and most readable. Use it for the highlighted row in lists, the current cell in tables, the cursor position.

**Bold + accent color** for the focused panel's title. Combined with border color change, that's unmistakable focus indication.

---

## Density: pack vs pad

You have a fixed grid. Two strategies:

### Pack (high density)

Examples: htop, btop, k9s.

Used when:
- Data is scanned at a glance.
- It updates in real time.
- Users read horizontally across rows.

Tactics:
- Tight rows (no extra spacing).
- Compact headers (one line, dim text).
- Abbreviations and units (`GiB`, `1.2k`, `99.9%`).
- Sparklines, mini-bars.
- One-line per record.

### Pad (low density)

Examples: gum/huh forms, glow markdown, Posting.

Used when:
- Users are reading prose.
- Filling forms (fields need breathing room).
- Making single decisions.

Tactics:
- Generous vertical spacing between fields.
- One field per line; labels above for narrow.
- Single column for forms.
- Lots of margin around modals.

For separation in moderate-density layouts, reach for whitespace before borders (see *The clutter audit* below).

### Hybrid: hierarchy through density

Use density to communicate hierarchy: dense data tables in the main panel, padded settings/forms in modals, dense status bars at the bottom.

### The clutter audit — making "feels busy" countable

"It feels noisy" is a real signal but useless until you convert it into specific offenders. When a layout feels cluttered, **count — don't squint** — and report the count, not a vibe:

- **Border-nesting depth.** Trace any corner from the terminal edge inward. More than *one* border between the edge and the content is too many. A border wrapping content that's already inside a bordered panel separates nothing and shows no focus — delete it. Boxes-inside-boxes is the single most common cause of "busy," and an outer full-screen frame is almost always redundant (the terminal edge already frames the app).
- **Signals per piece of state.** Count how many things encode the same fact. `[PASS]` + green + `✅` + a `▶` row prefix is *four* signals for one status. Keep one (color paired with a letter/word for monochrome safety); cut the rest.
- **Always-on markers.** A glyph that appears on 100% of rows (a `▶` on every line) marks nothing — it's texture, not information. Reserve markers for the exception: the selected row, the failed item.
- **Chrome-to-data ratio.** Add up the cells spent on borders, titles, padding, labels, and repeated boilerplate (a full ISO date on every log line when only the time changes) versus cells showing actual data. A high chrome ratio *is* the cluttered feeling.
- **The removal test.** For each decorative element ask: "if I delete this, do I lose information?" If no, delete it. Whitespace is not empty space — it's the cheapest separator you have, and a single blank row routinely out-reads a heavy border.

Run this pass on any layout — yours or someone else's. "Three concentric borders before the content, status encoded four ways, a marker on every row, full datestamps repeated every log line" tells the user exactly what to cut. "Simplify it" tells them nothing.

---

## Responsive design — breakpoints and the floor

A terminal layout is not designed for one size. The same app runs on a 220-column ultrawide, an 80×24 SSH session, a 60-column tmux split, and a 13-inch laptop. **A layout that only works at the author's window size is unfinished** — and the author rarely notices, because they only ever see their own terminal. Pressure-test every layout at the floor, and raise it in any review even when the user only asked about something else: it's the most-missed issue in TUI design.

### The breakpoint ladder

Decide behavior per width band, widest to narrowest. Exact thresholds depend on content, but the shape is universal:

- **Wide (>120 cols)** — the full multi-panel layout; you can afford a side panel or preview alongside the primary view.
- **Standard (80–120)** — the baseline most users see. Often: one primary view full-width, with details/logs on drill-in (Enter / a key) rather than permanently side-by-side.
- **Narrow (60–80)** — collapse to a single column. Stack panels vertically or hide all but the primary. Multi-column layouts (Miller columns, 2×N grids) **must** fold to one pane here. (lazygit's `--screen-mode full` is a related single-panel mode — but it's a user-selectable escape hatch, not automatic responsive behavior.)
- **Below the application-specific minimum** — don't render garbage or panic. Show a clean message naming the dimensions the app actually requires, such as `terminal too small — need 48×12`, until the user resizes. Do not claim an 80-column requirement if the single-pane layout intentionally supports 60 columns.

This is why a **drill-down model degrades better than a fixed grid**: when only one primary thing is ever on screen, narrowing just shrinks it; a fixed 2×2 grid has nowhere to go and turns to mush. If you find yourself unable to make a grid responsive, that's often a sign the layout should have been drill-down in the first place.

### Mechanics

- **Lay out in relative units, never absolute positions:** percentages (Textual `width: 30%`), ratios (Ratatui `Ratio(num, den)`), `Min`/`Max`/`Fill` constraints (Ratatui), `fr` units (Textual `1fr`/`3fr`), flex (Ink/Yoga). Derive geometry from the current frame or latest known window dimensions, and invalidate cached rectangles whenever those dimensions change.
- **Decide what's load-bearing.** When width runs out, what hides *first*? Usually: preview pane → secondary columns → low-priority table columns. Keep the primary view and the controls needed to operate it; a dedicated footer can collapse if those controls remain discoverable elsewhere. **Detail-on-Enter** is the escape hatch — it lets you hide columns/fields at narrow widths without losing access to the data.
- **Truncate, don't wrap, in cells**; reserve a cell for the ellipsis. Truncate paths at the start or middle so the filename survives; tail-truncate prose and IDs.
- **Handle the framework's resize event** and re-layout from the current frame/window size. On POSIX this usually begins with `SIGWINCH`; Windows and higher-level frameworks expose different events. Coalesce rapid events only when layout work is expensive so resizing still feels immediate.
- **Use 80×24 as a compatibility test baseline, not a universal hard minimum.** Define a smaller application-specific minimum from the content that must remain usable and test both sizes. `tmux split-window -h` is a free narrow-terminal test rig.

When reviewing a layout, state the degradation plan concretely: "at 80×24 the preview drops and you get parent│current; at 60 columns it's a single pane; below the tested minimum, show a message naming that minimum." **A review that doesn't name what happens at the floor hasn't finished.**

---

## Visual hierarchy in monospace

Since you can't change font size, hierarchy comes from:

1. **Position** — top/left reads first; status bar at bottom.
2. **Color and weight** — bold + accent for headlines; dim for secondary.
3. **Reverse video** — current selection.
4. **Indentation and connectors** — `├─ └─` for trees; consistent indent units (2 cells is standard).
5. **Whitespace** — blank rows for separation; pads around important content.
6. **Bullets / symbols** — `▶` expandable, `▼` expanded, `●` active, `○` inactive, `•` static bullet.
7. **Borders** — focused panel highlighted via border color.

Combining 2–3 signals creates clear hierarchy. The current selection: reverse video + bold. The focused panel: accent border + bold title. A warning: yellow + `[!]` prefix + slightly lifted from surrounding text via blank line above.

---

## Tables and lists

### Alignment rules

- **Numerics: right-aligned.** Easy to scan magnitudes.
- **Text: left-aligned.** Standard.
- **Dates: fixed-width ISO-8601** (`2026-04-28 14:30`). Sortable; doesn't shift on locale.
- **Status / categorical: centered or left** — be consistent.

### Truncation

When content exceeds column width:

- **Start truncation** (`…/share/file.txt`) — for paths, so the filename you're looking at survives.
- **Middle truncation** (`/usr/…/file.txt`) — for paths when the root is also useful context.
- **Tail truncation** (`Connection refused whi…`) — for prose, names, and IDs, where the beginning carries the meaning.
- **Wrap** — only for prose. Never wrap in cells of dense tables.

Reserve a cell (or three for `...`) for the ellipsis. Don't truncate so aggressively that nothing remains: `... ` is useless.

### Sort indicators

Show which column is currently sorted: `▲` ascending, `▼` descending. Place after the column header: `Size ▼`. Click (or key) to toggle and cycle.

### Filtering

Filter behavior (response time, match count, highlighting, smart case) is defined in `interaction-patterns.md` → *Search and filter*.

### Wide tables on narrow terminals

Three approaches:

1. **Hide low-priority columns.** Define column priority; drop them as width shrinks.
2. **Horizontal scroll.** htop scrolls the process table left and right with the arrow keys when columns overflow.
3. **Detail-on-Enter.** Pressing Enter on a row opens a side panel or modal showing all fields. The universal escape hatch.

Detail-on-Enter is the highest-leverage pattern: you can show fewer columns, and the user is one keypress away from full detail.

### Virtualization

**Always virtualize** lists that might exceed a few hundred items.

- **k9s** virtualizes for thousands of pods.
- **Toolong** tails multi-GB log files.
- **Textual `DataTable`** virtualizes by default.
- **Ratatui `List` + `ListState`** handles offsetting efficiently.
- **Bubbles `list`** virtualizes.
- **Ink** needs an explicitly windowed slice or viewport for a changing collection. `<Static>` is only for finalized append-only history; it does not virtualize selectable, filtered, or otherwise mutable rows.

The naive "render all rows, scroll viewport" approach degrades horribly at 10k+ items.

---

## Localization

Terminal layouts assume a fixed number of cells per string, so translated text breaks them in ways a GUI would absorb.

- **Budget for growth.** Translations run longer than English, and short strings grow most. IBM's globalization guidelines allow 100–200% extra space for strings up to 10 characters and 30% past 70 ([IBM](https://public.dhe.ibm.com/systems/power/docs/systemi/v5r2/en_US/rbagsmstp3.pdf), p. 16), and Microsoft's pseudo-localization lengthens text by 40% ([Microsoft](https://learn.microsoft.com/en-us/globalization/methodology/pseudolocalization)). Footer hints, column headers, and button labels are exactly those short strings. Size columns from the rendered text rather than the English labels, and let labels truncate before data does, with the full text in help.
- **Test with a pseudo-locale.** Before any translation exists, run the UI with every string lengthened by about half and wrapped in markers, as Windows' `qps-ploc`, Android's `en-XA`, and ICU's `en_XA` do, then pressure-test the floor from *Responsive design*. A 60-column split with long labels is where layouts fail. General catalog tools such as gettext's `en@boldquot` and Translate Toolkit's `podebug` produce the strings.
- **Right-to-left text.** Most terminals don't implement bidi. kitty, Alacritty, Ghostty, and Windows Terminal show Arabic and Hebrew in logical order, left to right; VTE (GNOME Terminal) and Konsole reorder lines by default, and WezTerm and iTerm2 only experimentally. The [terminal-wg recommendation](https://terminal-wg.pages.freedesktop.org/bidi/recommendation/basic-modes.html) has full-screen apps turn off the terminal's reordering (`CSI 8 l` on entry, `CSI 8 h` on exit), which leaves visual order to the app. Either way, keep right-to-left strings in their own field or line, never mix directions inside one cell, and don't promise correct shaping.
- **Ambiguous-width characters.** Unicode's East Asian Ambiguous class ([UAX #11](https://www.unicode.org/reports/tr11/tr11-46.html)) is narrow by default and wide only in East Asian legacy contexts, and terminals expose the choice as a setting: iTerm2, VTE, and Windows Terminal 1.25+. Nearly all box drawing, including the rounded corners, is ambiguous, and so are `…`, `·`, `•`, `●`, `★`, and the arrows `←↑→↓`. When the app and the terminal disagree, borders and markers misalign. go-runewidth switches to wide under any `ja`, `ko`, or `zh` locale, while most other width libraries default to narrow. Test with the terminal's ambiguous-wide setting on, prefer narrow-class markers where they fit (`▸ ▾ ✓ ✗ ⋯` and the Braille spinner frames), and keep the ASCII border set from *ASCII fallback* one switch away.
- **Numbers, dates, and units.** Format human output with the user's locale, because separators, grouping, and even digits differ: 1234567.891 is `1 234 567,891` in French and `12,34,567.891` in Indian English. Use the platform's locale library (`Intl`, Babel, `golang.org/x/text/message`, ICU4X) rather than hand-built strings. Keep machine output (`--json`, `--plain`) locale-independent: under a comma-decimal locale, POSIX `printf` both prints and parses numbers with commas, so pin `LC_NUMERIC=C` for anything a script will read. Fixed-width ISO-8601 dates remain the right choice inside tables.
- **Locale detection.** Read `LC_ALL`, then the category variable (`LC_CTYPE` for encoding, `LC_NUMERIC` and `LC_TIME` for formats), then `LANG`, skipping empty values ([POSIX](https://pubs.opengroup.org/onlinepubs/9799919799/basedefs/V1_chap08.html)). `C.UTF-8` is a glibc name rather than a POSIX one, and CI may run in either the C locale or a full one (GitHub's macOS runners set `LC_ALL=en_US.UTF-8`), so tests should pin the locale they expect.

---

## Status bars, headers, footers

### A common full-screen four-section layout

```
┌─────────────────────────────────────────┐
│ Header (top)                            │ ← persistent context: app, dataset, mode
├─────────────────────────────────────────┤
│                                         │
│   Main area (middle)                    │ ← the panels
│                                         │
├─────────────────────────────────────────┤
│ Status / mode line                      │ ← ephemeral feedback
│ Footer hint bar                         │ ← contextual shortcuts
└─────────────────────────────────────────┘
```

Some apps merge status and footer into one line. Some skip the header. Complex full-screen apps benefit from persistent context around the work area; bounded inline tools often need neither a header nor a dedicated footer.

### Header content

- App name (often with logo char or color block).
- Current dataset / context: branch name (lazygit), cluster + namespace (k9s), file path (bat/glow).
- Mode indicator (NORMAL / INSERT in editors).
- Right-aligned: time, host, version (sparingly — only if meaningful).

### Status / mode line

- Ephemeral feedback: "Saved", "3 files changed", "Connecting...".
- Auto-fade after a few seconds (or replace with idle state).
- The current mode, for modal apps (label and cursor shape: `interaction-patterns.md` → *Modal (vim-style)*).
- Color-coded: green = OK / saved, yellow = pending, red = error.

### Footer hint bar

Lay hints out as `key action · key action · key action`, with `·` (middle dot) or `|` as the separator; htop's `F1Help F2Setup F3Search …` strip is the classic form. Which shortcuts to show, how many, and how to generate them from your keymap: `interaction-patterns.md` → *Layer 1: Always-visible footer hints*.

---

## Progress indicators

### Spinners

The de facto modern default for indeterminate work: **Braille spinners**.

```
⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏
```

Ten frames at ~80ms per frame (cli-spinners' `dots`) = smooth rotation. `cli-spinners` (the npm package, vendored everywhere) ships ~70+ named styles.

**Rules:**
- Show only after ~100–200ms — instant work shouldn't flash a spinner.
- Stop with a final state symbol: `✓` (success), `✗` (failure), or just disappear.
- Suppress entirely on non-TTY. CI logs shouldn't have spinners.
- In plain, screen-reader, or reduced-motion mode, print one static line instead (see *Accessibility*).

### Determinate progress bars

Block characters for sub-cell precision:
```
▏▎▍▌▋▊▉█
```

These give 8 sub-positions per cell. A 40-cell-wide bar can represent 40 × 8 = 320 distinct progress states.

```
[████████▎             ] 41%
```

Show: percent, current/total counts where meaningful, ETA for long operations.

### Multi-progress

Multiple parallel tasks each with their own bar:

```
package-a [████████████████████] 100% ✓
package-b [████████▎           ]  41%
package-c [██▎                 ]  10%
```

indicatif `MultiProgress`, rich `Progress` (multiple tasks), and listr2 `concurrent` mode support this out of the box; Bubbles ships only a single `progress` component — compose multiple bars by hand. Used by Docker pulls, npm/cargo builds, parallel installers.

### Pulse / fade

Ambient indication of background work without commanding attention. lazygit's auto-fetch, for example, shows only a small spinner and status message instead of a blocking dialog. Useful when work is happening but doesn't need to be in the user's face.

### Empty states and loading states

**Empty states:** never just "No data." Say what to do next: Posting's empty collection reads "Collection is empty. Press ctrl+s to save the current request."

**Loading states:** skeleton text or "Loading..." with a delayed spinner (don't flash for sub-200ms loads). Animate only when something is happening.

**Error states:** what failed and how to recover. "Connection refused. Press `r` to retry."

**Disconnected, stale, or hung states:**
- Keep the last-known-good view visible, dimmed, with a "stale" or "reconnecting" marker instead of blanking the screen. k9s's retry and backoff settings (`apiServerTimeout`, `maxConnRetry`) treat connection loss as a designed-for case.
- Never let a transient failure rewrite persistent config. btop has an open bug where a temporarily unavailable GPU permanently rewrites the user's saved widget layout instead of hiding the metric until it returns.
- Show elapsed time once a wait passes a second or two, and let `Esc` cancel the in-flight request. There is no TUI convention for an automatic give-up time, so prefer elapsed time and cancel over picking one.

---

## Theming systems

Most production TUIs support themes. The canonical approach:

1. **Define semantic tokens** (`status.error`, `text.muted`, etc.).
2. **Define palette mappings** as TOML/YAML/CSS — one file per theme.
3. **Allow theme selection** via config, env var, or runtime command.

### Configuration formats

- **TOML** — bottom, helix, starship, alacritty (since 0.13); btop's `btop.conf` is a TOML-like `key = value` file.
- **YAML** — lazygit (`config.yml`), lazydocker, k9s (including `plugins.yaml`).
- **Flag files / git config** — bat's config file is a list of CLI flags; fzf reads `FZF_DEFAULT_OPTS` or `FZF_DEFAULT_OPTS_FILE`; delta lives in `.gitconfig` under `[delta]`.
- **TCSS (Textual CSS)** — Textual apps; live-reloads.
- **JSON** — VS Code-style; less common in TUIs.

No format is the TUI standard. Matching the tool your users already configure, as when mirroring lazygit, is a good reason to use its format; otherwise choose the one that suits your own settings.

### Community palettes for themeable, broadly distributed apps

If theming is part of the product, either ship popular palettes or document how to import them:

- **Catppuccin** (Latte, Frappé, Macchiato, Mocha).
- **Dracula**.
- **Nord**.
- **Gruvbox** (light, dark; soft, medium, hard).
- **Tokyo Night**.
- **Rose Pine** (Main, Moon, Dawn).
- **Solarized** (light, dark).
- **base16** umbrella spec — many themes follow this.

The community has built theme repos for most popular tools; users of highly themeable apps expect plug-and-play. Small single-purpose tools still need semantic colors and light/dark safety, but do not need to bundle every palette.

### Icons and Nerd Fonts — there is no detection, only opt-in

No terminal emulator exposes "a Nerd Font is installed and active" as a queryable signal — not kitty, WezTerm, iTerm2, Alacritty, Windows Terminal, or Ghostty. Fonts are a client-side rendering concern the terminal protocol has no capability query for; even reading the configured font name from a config file (the one heuristic tool that tries this, `has-nerd-font`, checks a list of terminals that bundle Nerd Font glyphs, then parses the terminal's config for the font name, and still needs a `NERD_FONT=1` override) doesn't guarantee the font is actually installed, and font-fallback chains mean a declared font can silently substitute per-glyph anyway. **Don't invent a detection scheme — gate icons behind an explicit opt-in instead**, the way real tools do:

- **eza** — `--icons=WHEN` (`always` / `automatic` / `never`); `automatic` gates on stdout being a TTY, not on font presence — it fully trusts the user.
- **lazygit** — `gui.nerdFontsVersion: '2' | '3' | ""`; empty (the default) means no icons at all.
- **yazi** — icons are default-on in the shipped theme rather than gated by a flag; the FAQ's answer for users without a Nerd Font is to override the icon tables in `theme.toml`, not a switch.

**Pin the codepoint generation, not just "Nerd Font on/off."** Nerd Fonts v3 reorganized Material Design Icons' codepoints (`F500–FD46` → `F0001+`) because the old range collided with CJK Unicode — a real, documented breaking change, which is exactly why lazygit's config asks for `'2'` or `'3'` explicitly rather than shipping one hardcoded set.

The fallback ladder — Nerd Font glyphs → plain Unicode symbols → ASCII — is real, but starship is the cleanest evidence of a tool actually shipping all three rungs as named presets (`nerd-font-symbols` / `no-nerd-font` / `plain-text-symbols`). Most tools are binary (icons on with a Nerd Font, or off), not a full three-tier ladder — don't imply every icon-capable tool implements all three.

### Light/dark detection

- OSC `]11;?` query — many terminals respond with their background color; others never answer, so bound the wait. Mode 2031 reports later theme switches where supported. The query, timeout, and sentinel details are in `interaction-patterns.md` → *Terminal modes and protocols* → *Color and appearance*.
- `$COLORFGBG` env var — set by some terminals (rxvt-derivatives).
- Lipgloss v2's `LightDark` (`AdaptiveColor` in v1/compat) and Textual's runtime theme switching abstract this.

If you support both light and dark, default to "auto" and let users override with `--theme dark`.

---

## Accessibility

Be honest about the medium. Screen readers announce what changed on screen. NVDA finds new text in a terminal by diffing its contents, and Windows Terminal reports text as it is written ([NVDA user guide](https://download.nvaccess.org/releases/2026.2/documentation/userGuide.html#DiffAlgo), [Windows Terminal accessibility notes](https://github.com/microsoft/terminal/blob/e90f9a534cae047fccddacc1ee813a82929662d5/doc/terminal-a11y-2023.md)). A full-screen UI repaints constantly, so a spinner frame, a re-highlighted menu row, or a redrawn panel is heard as new text, and the layout itself carries no structure a listener can navigate. No TUI framework fixes that. The dependable answer is a linear mode offered alongside the full-screen UI.

### What works for screen-reader users

These rules come from Sampath, Merrick, and Macvean's study of blind developers using CLIs ([CHI '21](https://doi.org/10.1145/3411764.3445544)) and from the screen-reader modes that current agent CLIs ship:

- **Append; don't repaint.** Report a state change as a new line (`Saved notes.txt`) instead of redrawing a status bar, and keep output in scrollback rather than on the alternate screen.
- **Say it in words.** End every command with a status line, because silent success leaves users guessing. Make progress an occasional line (`Uploaded 3 of 10`), not spinner frames or a bar, which are read out character by character.
- **Flatten structure.** Turn table rows into labelled `Header: value` lines or offer `--format csv|json`; aligned columns force users to memorize column order. Drop borders, box drawing, and ASCII art, which are heard as symbol noise.
- **Number the choices.** An arrow-key menu is re-read whenever the highlight moves; print a numbered list and read a typed answer.
- **Label lines.** Prefix output kinds (`error:`, `warning:`, speaker names in a chat) so a listener knows what a line is before hearing it.
- **Write errors to be spoken.** Say what failed and what to do next, without regexes or symbol-dense jargon.
- **Keep the cursor on the focus.** Screen readers and magnifiers follow the terminal cursor, which full-screen apps usually hide. Leave it on the focused field or row, or return it to the prompt after printing.

[Claude Code's screen-reader mode](https://code.claude.com/docs/en/accessibility) is the most complete published example: flat text without box drawing, `Header: value` tables, numbered menus, labelled lines, no fullscreen, and the terminal bell when it needs input.

### The plain-mode contract

`SKILL.md` requires a `--no-tui`-style mode when automation or accessibility needs linear output. Treat it as part of the product, not a degraded fallback. It must meet these terms:

- It reaches every action the TUI offers; nothing is TUI-only.
- It follows the rules above and the stream contract in `cli-basics.md` → *Stdin / stdout / stderr discipline*. Results go to stdout; prompts and progress go to stderr.
- Meaning never depends on color, column position, or layout.
- Every prompt has a flag or config equivalent, so the mode can run unattended.
- Users turn it on once, through a flag, an environment variable, or a persistent setting, and it starts automatically when stdout is not a terminal. Don't count on detecting a screen reader. Ink, Gemini CLI, and Claude Code all require an explicit opt-in. gh's `GH_ACCESSIBLE_PROMPTER` and `GH_SPINNER_DISABLED`, with matching config keys, are the model.

### Framework support

None of these makes a full-screen app accessible by itself. Check the ecosystem reference before relying on a feature:

| Stack | What exists | Details |
|---|---|---|
| Huh (Go) | Accessible mode: plain line-by-line prompts with numbered choices and no full-screen redraw. The app switches it on, and Huh does so itself only for `TERM=dumb` | `ecosystem-go.md` → *Huh* |
| Bubble Tea, Bubbles | Nothing screen-reader-specific. Build the linear mode yourself and use Huh's accessible mode for prompts, as gh does | `ecosystem-go.md` → *Huh* |
| Ink | `aria-*` props and an opt-in screen-reader renderer that prints linear text without borders or color; the live region is still rewritten on every change | `ecosystem-typescript.md` → *Strengths and weaknesses* |
| Textual | No screen-reader support, and `textual serve` adds none. Built-in animations honor a reduced-motion level | `ecosystem-python.md` → *Dev tools* |
| Ratatui | Nothing in the library; accessibility is an open tracking issue | `ecosystem-rust.md` → *Pitfalls* |

### Motion

- Treat animation as decoration with an off switch, and default it off in plain and screen-reader modes. No cross-tool reduced-motion variable exists, so name yours and document it, as gh (`GH_SPINNER_DISABLED`) and Textual (`TEXTUAL_ANIMATIONS=none`) do.
- With motion off, print one static line (`Working…`) and then the final status, rather than a frozen spinner frame.
- Never blink text (see *Typography in monospace*), and don't animate content the user is reading.

### Keyboard, focus, and color

- Every action is keyboard-reachable (`SKILL.md`), and focus shows through at least one cue besides color (border weight, reverse video, or a marker), combined as `interaction-patterns.md` → *Focus indicators* describes.
- Color rules live in *Color and accessibility* and *Honor `NO_COLOR`* above. Keep a 16-color theme: those colors follow the user's terminal palette, including high-contrast palettes, while truecolor values override it.

---

## Common visual pitfalls

1. **Hardcoded colors** that clash with user themes. Use semantic tokens.
2. **Decorative borders** that don't earn their keep.
3. **Color-only signaling.** Pair with letters or symbols.
4. **Misaligned tables** when text contains CJK or emoji. Use `unicode_width` / `string-width`.
5. **Over-bolding.** Bold loses meaning when half the screen is bold.
6. **Inconsistent capitalization** in headers (mix of "Branch", "branch", "BRANCH").
7. **Emoji as the only signal.** Some terminals render them as boxes.
8. **Light-mode-only colors** (low contrast on dark, or vice versa).
9. **Tiny status bar** that hides important state under the fold.
10. **No visual focus indication** — users tab around guessing where they are.

When reviewing a TUI, walk this list. Most existing apps fail on 3–5 items.

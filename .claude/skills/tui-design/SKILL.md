---
name: tui-design
description: Design, build, refactor, or review terminal interfaces—full-screen TUIs, interactive prompts, and command-line tools. Use for terminal-app layout and UX, CLI behavior, ncurses-style tools, dashboards, REPLs, fzf-like pickers, and libraries such as Bubble Tea, Ratatui, Textual, or Ink, including requests that name a known TUI such as lazygit, k9s, btop, helix, or yazi instead of saying “TUI.” Do not use for browser or web UI, native GUI, editor or font configuration, or backend and shell work that has no terminal interface.
---

# TUI & CLI Design

Design terminal software that is calm, predictable, fast, and honest about the medium. Treat this file as the workflow and cross-cutting contract. Load detailed guidance from the one reference that owns it instead of reconstructing or repeating it here.

## Route before answering

| Need | Authoritative reference |
|---|---|
| Go, Bubble Tea, Lipgloss, Bubbles, tview, gocui | `references/ecosystem-go.md` |
| Rust, Ratatui, crossterm, Cursive | `references/ecosystem-rust.md` |
| Python, Textual, Rich, prompt_toolkit, urwid | `references/ecosystem-python.md` |
| TypeScript/JavaScript, Ink, OpenTUI, Clack, Inquirer | `references/ecosystem-typescript.md` |
| One-shot commands, arguments, streams, exit codes, automation | `references/cli-basics.md` |
| Layouts, buffers, borders, hierarchy, color, density, responsive behavior, tables, themes, accessibility (screen readers, plain mode, motion) | `references/visual-patterns.md` |
| Keys, focus, navigation, modes, forms, mouse, confirmation, undo, OSC features | `references/interaction-patterns.md` |
| Case studies: lazygit, k9s, fzf, btop, helix, yazi, atuin | `references/exemplar-apps.md` |
| Screenshots or demo recordings | The `vhs-cli-demos` skill when installed; otherwise `references/cli-basics.md` → *Demo recordings* |

Load only the references the task needs. When the prompt names a framework or ecosystem, always load its ecosystem reference before making API, lifecycle, implementation, or testing claims. Ecosystem references own those specifics. The visual and interaction references own their domains for every ecosystem. Exemplar apps are evidence and inspiration, not substitutes for the pattern references.

If no language is named, ask only when ecosystem choice would materially change the answer or implementation. Otherwise state a reasonable recommendation and proceed: Go for polished single binaries, Rust for control and reliability, Python for rapid product work, and TypeScript when React or npm distribution is already an advantage.

## Classify the product first

Choose the output contract before choosing a framework or layout:

| Product shape | Default contract |
|---|---|
| One-shot CLI | No live full-screen UI. Stable stdout for results, stderr for diagnostics, meaningful exit codes. Load `cli-basics.md`. |
| Summon–choose–exit tool | Prefer inline when shell context matters. Put interactive chrome on stderr or `/dev/tty` and the selected result on stdout. Use full-screen only when a large preview or working set needs stable space. |
| Full-screen session | Use the alternate screen and a stable spatial model. Treat terminal restoration, resize, suspend, and redraw behavior as product requirements. |

Then name the workflow shape—persistent panels, Miller columns, drill-down stack, dashboard, IDE-style panes, overlay, or tabs—and verify it against `visual-patterns.md`. Sketch the states and layout before writing code: initial, loading, empty, partial, success, error, disconnected, and too-small.

## Work the task

1. Classify the product shape and its stdout/stderr contract.
2. Identify the dominant user loop and the 5–8 most common actions.
3. Select the ecosystem and load its reference plus any relevant pattern reference.
4. Sketch the layout and state transitions at wide, standard, narrow, and minimum sizes.
5. Implement in the ecosystem's native architecture; keep state/update/event work separate from rendering where the framework permits.
6. Verify lifecycle cleanup, input discoverability, output behavior, width handling, and async work.
7. Test the cheapest stable layer first, then rendered frames, then a small PTY smoke path only if its integration risk justifies it.

For design questions, make the recommendation before explaining it. For implementation, inspect the existing architecture and dependencies before introducing a new framework or abstraction. For reviews, cite concrete observations and prioritize changes by user harm.

## Preserve these cross-cutting contracts

### Terminal lifecycle

- Use the alternate screen for full-screen sessions; keep bounded and one-shot workflows inline when possible.
- Prefer framework-managed terminal cleanup. Restore raw mode, screen buffer, cursor, and input modes on every exit path, including errors and panics. Do not invent custom signal handling when the framework already owns it.
- Re-layout from the current frame or window size on resize. Coalesce bursts only when layout work is expensive.
- Treat final shutdown and temporary handoff as different boundaries. For an editor, shell, or supported suspend, prefer the framework's handoff API: pause UI input, restore the shell-facing terminal, wait, re-enter modes, reload externally mutable data, and force a full redraw. Redrawing only repaints the current model; it does not refresh changed data. Do not final-unmount an app that must resume, and do not assume POSIX signals exist on Windows.
- Keep logs and debug output away from the screen the TUI owns. Use a file, framework console, or separate diagnostic stream.

### Rendering, data, and performance

- Never block the UI/event thread on disk, network, or subprocess work. Return results through commands, messages, tasks, channels, or framework events.
- Render on input, data, resize, or intentional ticks; do not redraw unchanged state in an unconditional loop.
- Measure terminal cell width, not bytes, code points, `len()`, or JavaScript string length. Test CJK, combining marks, and emoji.
- Virtualize collections that can grow beyond a few hundred rows. Truncate rather than wrap inside tables; reveal full values in a detail view.
- Keep panel positions stable unless the user explicitly changes the layout. Spatial memory is part of navigation.

### Meaning and access

- Define semantic style tokens rather than scattering color literals. Honor `NO_COLOR` in automatic color mode and preserve meaning in monochrome.
- Never use color alone. Pair it with text, shape, position, or symbols, and provide an ASCII fallback when Unicode support is uncertain.
- Make every action keyboard-reachable. Mouse support may accelerate an action but must not gate it.
- Offer familiar navigation aliases only where they do not conflict with text entry or a complete bounded prompt keymap. Preserve the expected reserved-key behavior: `Ctrl+C` cancels or quits, and `Ctrl+Z` suspends through the framework. Raw mode delivers both as keypresses and disables XON/XOFF, so a full-screen app may bind `Ctrl+S`; flow control only affects line-mode programs.
- Match discoverability to complexity: complete inline controls for bounded prompts; contextual hints, help, and optionally a command palette for action-rich full-screen apps.
- Provide a plain `--no-tui` or equivalent mode when automation or serious accessibility needs require linear output; `visual-patterns.md` → *Accessibility* defines what it must contain.

## Apply two review reflexes unprompted

These catch the failures users rarely name. Apply both to every layout you design or review, even when the question is about something else.

### Run a clutter audit

Make “busy” countable. Report:

- border-nesting depth—more than one border between the terminal edge and content is usually too much;
- how many signals encode the same state—`[PASS]`, green, a checkmark, and a row marker is four;
- markers present on every row, which therefore mark nothing;
- the share of cells spent on chrome, labels, and repeated boilerplate instead of data.

Name the exact borders, markers, labels, or repeated fields to remove. Do not stop at “simplify it.” Use the full method in `visual-patterns.md` → *The clutter audit*.

### Pressure-test the floor

State what happens at 80×24 and in a 60-column tmux split: which pane wins, what hides, what truncates, what becomes drill-down, and when the truthful “terminal too small” state appears. Every multi-column design needs a single-pane fallback. Use the breakpoint ladder and minimum-size method in `visual-patterns.md` → *Responsive design*.

## Build and verification discipline

Keep business state independent enough to test without a terminal. In MVU or immediate-mode systems, feed synthetic events into update logic and assert state. In retained/widget systems, drive the smallest widget or app harness that owns the behavior.

Use a bottom-heavy test pyramid:

1. Unit-test state transitions, parsing, sorting, filtering, and command construction.
2. Snapshot or golden-test rendered frames at pinned terminal sizes and color profiles, including 80×24, 60 columns, and the hard minimum.
3. Use one or two PTY end-to-end flows for lifecycle and real-keyboard integration, not as the primary suite.

Verify at least:

- normal exit, interrupt, error, and panic cleanup;
- resize, too-small behavior, and suspend/resume where supported;
- empty, loading, partial, error, disconnected, and large-data states;
- keyboard reachability, focus visibility, and help accuracy;
- `NO_COLOR`, 16-color or monochrome, ASCII fallback, and non-TTY output;
- wide characters, combining marks, truncation, sorting, and virtualization;
- no stdout corruption and no blocking I/O in the event/render path.

Use the ecosystem reference's exact testing and debugging APIs. Never print diagnostics into an active raw-mode or alternate-screen UI.

## Review existing interfaces

Report evidence in priority order:

1. Is the product shape right, or should a full-screen flow be inline or one-shot?
2. Can any exit path leave raw mode, cursor state, mouse capture, or the screen buffer behind?
3. Does the UI block, redraw wastefully, crash on resize, or mismeasure cell width?
4. Can a first-time user find and reach the important actions without breaking text entry or terminal conventions?
5. What does the clutter audit count, and which exact elements should be cut?
6. What happens at 80×24, 60 columns, and the declared minimum?
7. Do output streams, exit codes, non-TTY behavior, `NO_COLOR`, and plain mode support scripts and accessibility?
8. Are state transitions and pinned frames covered at the right test layers?

Avoid generic verdicts. Tie each recommendation to a user-visible failure, an implementation risk, or a measurable reduction in noise.

## Give useful recommendations

Be decisive where practice has converged: semantic colors, clean terminal restoration, responsive fallback, non-blocking work, width-aware rendering, and honest stream contracts. Explain real tradeoffs for inline versus full-screen, modal versus modeless input, mouse support, and destructive-action confirmation.

Use the chosen ecosystem's idioms rather than translating another framework's architecture literally. When a design choice remains abstract, point to the relevant case study in `exemplar-apps.md` and explain which part of its solution transfers.

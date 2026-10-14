# Exemplar apps — what makes specific TUIs great

Concrete case studies. When the user asks "how should I lay out my dashboard" or "how do I handle drilling into resources" or "what makes lazygit feel so good," point at one of these. Each entry covers: layout pattern, what it does well, specific features worth copying, lessons for builders.

For abstract principles, see `visual-patterns.md` and `interaction-patterns.md`. This file is the case-study companion.

## How to use this file

When the user asks about a design choice and you're unsure of the answer, find the analogous case here:

- "How should I lay out a dashboard?" → btop, bottom, htop.
- "How should drilling-down work?" → k9s, lazydocker.
- "How should I make searching fast?" → fzf, atuin.
- "How do I handle a million-row table?" → Harlequin, Toolong.
- "How should help/discovery work?" → htop's F-keys, helix's which-key, lazygit's footer.
- "How do I keep my AI chat smooth at high token rates?" → Claude Code and Copilot CLI (alternate screen, virtualized transcript) versus Gemini CLI (inline, Ink `<Static>` history).
- "What's the spec for my undo system?" → lazygit's git-action stack.
- "How should mouse work in my TUI?" → btop (clickable keys and boxes), helix (keyboard-first; mouse on by default but never required), lazygit (augmentation).
- "How fast does my prompt picker need to be?" → fzf (<100ms), starship (per-module time budgets).
- "What does a polished setup wizard look like?" → @clack/prompts (create-vite, create-astro) — see `ecosystem-typescript.md`.
- "How do I theme well?" → btop, bottom, helix, Posting (community palette support).

Concrete examples beat abstract principles for design questions. When in doubt, point at the app and the user can study its source.

**Entries, grouped:**
- Git: [lazygit](#lazygit-go-gocui) · [gitui](#gitui-rust-ratatui)
- Kubernetes / Docker: [k9s](#k9s-go-tview) · [lazydocker](#lazydocker-go-gocui)
- Monitors: [btop / btop++](#btop--btop-c) · [bottom / btm](#bottom--btm-rust-ratatui) · [htop](#htop-c-ncurses)
- Pickers and search: [fzf](#fzf-go) · [atuin](#atuin-rust-ratatui)
- Editors: [helix](#helix-rust-custom-renderer) · [neovim](#neovim-c--lua)
- File managers: [yazi](#yazi-rust-ratatui) · [ranger / lf / nnn / broot](#ranger--lf--nnn--broot-mostly-python-and-go)
- IDE-style apps: [Posting](#posting-python-textual) · [Harlequin](#harlequin-python-textual)
- Viewers: [Toolong](#toolong-python-textual) · [Glow](#glow-go-bubble-tea)
- Chat-style AI tools: [Claude Code / Copilot CLI / Gemini CLI](#claude-code-github-copilot-cli-gemini-cli-typescript)
- CLI output discipline: [starship](#starship-rust-no-ui-framework) · [yt-dlp / aria2](#yt-dlp--aria2-cli-but-excellent-terminal-output)

---

## lazygit (Go, gocui)

> Multi-pane git client. The aesthetic that defined "lazy*."

**Layout:** persistent multi-panel — 5 panels on the left (Status, Files, Branches, Commits, Stash), 1 large panel on the right (Diff/details), command log strip at the bottom.

**What it does well:**
- **Per-pane sub-tabs.** The Branches panel has Local / Remotes / Tags as `[`/`]` tabs.
- **Undo/redo for git operations** (`z` / `Z`), driven by the reflog — so it can walk back a rebase, but not working-tree or stash changes, pushes, or branch creation, and not while a rebase is in progress. This is unusually thoughtful for a TUI.
- **Unobtrusive background-fetch status** (a small spinner) — present without commanding attention.
- **Custom commands and aliases** in YAML config (`config.yml`).
- A "command log" pane showing what git commands actually ran (transparency).
- Confirmation popups for destructive actions (Enter confirms, Esc cancels).

**Pattern recipe:** the numeric-panel-jump + context-sensitive-letter interaction lazygit defined is dissected in `references/interaction-patterns.md` → *The lazygit pattern — multi-pane with numeric tabs*.

**Stack:** Go, gocui (jesseduffield's fork, copied into the lazygit repo as `pkg/gocui` in April 2026) on tcell v3, YAML config.

---

## k9s (Go, tview)

> Kubernetes TUI. Drill-down stack + command mode.

**Layout:** drill-down stack with command-mode navigation. Top status header (cluster, context, namespace), main resource list, footer hints. Drill into a resource → full-screen detail view → `Esc` back.

**What it does well:**
- **Resource-aware actions:** `s` shells into a pod, `l` shows logs, `d` describes. Different resources get different actions.
- **Sortable columns** via Shift+letter (`shift-n` name, `shift-a` age, `shift-o` the selected column).
- **Instant filter** (`/`) to narrow huge lists in real time — regex by default; `/-f` switches to fuzzy matching.
- **Live updates** — pods change state; k9s reflects without refresh, via a debounced redraw (don't refresh on every event).
- **Plugin system** for custom actions (`plugins.yaml`).
- **Status-header** showing the current "address" of where you are.

**Pattern recipe:** the ex-command mode (`:pods`, `:svc`, tab-completion, aliases) is dissected in `references/interaction-patterns.md` → *The k9s pattern — command-driven*.

**Stack:** Go, derailed's forks of tview and tcell (`derailed/tview`, `derailed/tcell`), YAML config, K8s client-go.

---

## btop / btop++ (C++)

> System monitor. The pinnacle of widget dashboard layouts.

**Layout:** widget dashboard. CPU graph full-width on top; memory above network on the left; processes table on the right. Boxes can be toggled, swapped to alternate positions, and saved as presets — not drag-resized — all configurable in `btop.conf` (a TOML-like `key = value` file).

**What it does well:**
- **Truecolor gradient meters** that look genuinely beautiful.
- **One tunable refresh rate** (`update_ms`, default 2000) that every box shares; each box keeps its own scroll, focus, and graph history.
- **Mouse support** that doesn't get in your way: highlighted keys and boxes are clickable, lists scroll with the wheel, and there is no drag-resize to fight.
- **Theme system** with dozens of community themes.
- **Help modal (`h`, `?`, or F1)** that's actually informative, organized by widget.

**Specific features worth copying:**
- Widget independence for focus, scroll, and layout (boxes can be shown, hidden, and rearranged individually).
- Theme system as one `.theme` file per theme (a simple `theme[key]="#hex"` format).
- The decision to default to truecolor and degrade — most users have it now.

**Lessons:** in a dashboard, every widget should be independent — its own scroll, its own focus, its own data source. Share a refresh clock when the data sources are cheap and uniform (btop does); give a widget its own cadence only when its source is slow or expensive.

**Stack:** C++ (btop). Related monitors: Rust's bottom (`btm`), inspired by gtop, gotop, and htop rather than by btop. cjbassi/gotop was archived in 2020 — the maintained fork is xxxserxxx/gotop; don't treat the original as a current peer.

---

## fzf (Go)

> The fuzzy finder. Pattern-defining.

**Layout:** full-screen by default; `--height` opts into a bounded picker below the cursor. Either form does one thing and exits.

**What it does well:**
- **Preview pane** with `--preview` — shows file contents, command output, anything, without leaving the picker.
- **Composable** — pipes to anywhere. `git branch | fzf | xargs git checkout` is the canonical pattern; even a full TUI can have a "summon fzf for picking" sub-flow.
- Opens `/dev/tty` for its interactive UI while keeping the selected value on stdout. Use `--height` when shell context must remain visible; see `references/visual-patterns.md` → *Inline, alt-screen, or overlay*.

**Pattern recipe:** the instant-fuzzy-filter interaction (sub-100ms updates, match count, smart-case, ranking) is dissected in `references/interaction-patterns.md` → *The fzf pattern in detail*.

**Lessons:** speed is a feature. Users summon fzf hundreds of times a day; making it 50ms instead of 200ms transforms the experience.

**Stack:** Go, no UI framework — its own renderer on Unix, tcell on Windows.

---

## helix (Rust, custom renderer)

> Modal editor. Selection-first, multi-cursor first.

**Layout:** main editor area (with multiple split views) plus a bottom statusline and diagnostics; everything else arrives as a picker. Even its file explorer (`Space e`, released in 25.07) is a directory-browsing picker, not a persistent sidebar tree — helix's identity is pickers instead of trees.

**What it does well:**
- **Tree-sitter** for syntax-aware text objects.
- **Space-based which-key.** Press `Space`, see a menu of `f` (file picker), `b` (buffer picker), `s` (symbols), etc.
- **`:` command mode** with completion.
- **Built-in LSP** (no plugins needed for most languages).
- Single-binary distribution with sensible defaults — works out of the box.

**Pattern recipe:** selection-first editing and multi-cursor-as-primary are dissected in `references/interaction-patterns.md` → *The helix pattern — selection-first modal editing*.

**Lessons:** modes don't have to be confusing. helix shows the mode in the statusline (`NOR`/`INS`/`SEL`) plus which-key for leaders; by default the cursor is a block in every mode and `color-modes` is off, so per-mode cursor shapes and colors are opt-in config (`[editor.cursor-shape]`, `color-modes = true`) — worth enabling, and worth shipping on by default in your own modal app.

**Stack:** Rust, custom rendering (Ratatui-adjacent).

---

## yazi (Rust, Ratatui)

> File manager with image preview. Miller columns.

**Layout:** miller columns — parent / current / preview, configurable widths (default `[1, 4, 3]`).

**What it does well:**
- **Async I/O everywhere.** Even directory listing is non-blocking. The UI never stalls.
- **Built-in image preview** via Sixel / kitty / iTerm2 (auto-detected).
- **Vim-style keybindings**, with `;` / `:` to run a shell command (`:` blocks until it finishes).
- **Plugins in Lua** — extends without recompile.
- **Tasks pane** for ongoing work (copies, transcodes).

**Specific features worth copying:**
- Async-everything — no operation should block the UI.
- The decision to support all three image protocols.
- Lua plugins for extension — extension without forking.

**Lessons:** in a file manager, *every* I/O can be slow on the wrong filesystem (network mounts, encrypted drives, dead USB). Async-first isn't optional; it's the foundation.

**Stack:** Rust, Ratatui, Crossterm, async via Tokio.

---

## atuin (Rust, Ratatui)

> Shell history with sync. Replaces Ctrl+R.

**Layout:** overlay/popup — replaces the default shell reverse-search with a richer fzf-style picker.

**What it does well:**
- **Replaces a built-in** (Ctrl+R) seamlessly. Users don't have to change muscle memory; they get a better version of what they already use.
- **Fuzzy filter with metadata** — when you ran the command, exit code, working directory, hostname.
- **Optional encrypted sync** to share history across machines.
- **Both CLI and TUI** from the same core. CLI for scripts (`atuin search`), TUI for interactive use.

**Specific features worth copying:**
- The "replace a familiar built-in" strategy. Far easier adoption than a new tool to learn.
- Showing metadata on every result (when, where, exit code) without cluttering.
- Dual CLI + TUI — the CLI handles scripts, the TUI handles exploration.

**Lessons:** the highest-leverage tools replace existing workflows rather than adding new ones. Users will switch to a slightly better Ctrl+R; they won't switch to a wholly new mental model.

Atuin Desktop (a GUI runbook app) launched in 2025; the CLI/TUI remains maintained alongside it.

**Stack:** Rust, Ratatui, optional self-hosted sync server.

---

## htop (C, ncurses)

> Process viewer. Old-school, still excellent.

**Layout:** persistent dense — header (CPU/mem/swap meters), process list (sortable), F-key strip at the bottom.

**What it does well:**
- **F-key strip** — `F1`–`F10` always visible at the bottom, telling you exactly what's available. The single most useful discoverability pattern.
- **Tree mode** (`F5`) — collapsible process tree.
- **Filter** (`F4`) and **search** (`F3`).
- **Color meters** in the header.
- **Mouse support** for clicking column headers to sort.

**Specific features worth copying:**
- The F-key strip. If you have 10 actions, just show them at the bottom — don't make users press `?` to discover them.
- Sortable column headers (click or key).
- Nice/renice via `F7`/`F8` — fast access to less-common but valuable actions.

**Lessons:** there's a reason htop has been the standard for 20 years. Persistent footer hints + sortable tables + mouse augmentation + keyboard primary = a recipe that just works. Sometimes the best design is the obvious one done well.

**Stack:** C, ncurses.

---

## bottom / btm (Rust, Ratatui)

> System monitor in Rust, inspired by gtop, gotop, and htop.

**Layout:** widget dashboard, configurable.

**What it does well:**
- **Tree process view** (`t`).
- **Battery widget** (laptops!).
- **Search and filter** in the process list.
- **Configurable layouts** via TOML — define rows and column ratios.
- **Per-widget basis** — focus a widget for full keyboard access.

**Specific features worth copying:**
- TOML layout config. Users will want to customize.
- Tree view as a single-key toggle.

**Lessons:** if you ship a dashboard, ship a way to rearrange it. Default layouts are never quite right for everyone.

**Stack:** Rust, Ratatui, Crossterm.

---

## Posting (Python, Textual)

> HTTP client. Postman-alternative for the terminal.

**Layout:** IDE three-panel — collection tree (left), request editor (main), response (bottom or right).

**What it does well:**
- **Empty states explain next action** — an empty collection says "Collection is empty. Press ctrl+s to save the current request."
- **Multiple themes** (Catppuccin, Gruvbox, Tokyo Night, Solarized, custom).
- **Vim keys** plus a "jump mode" for reaching any widget in two keystrokes.
- **Import and export** of curl commands, Postman collections, and OpenAPI specs.
- **Variable substitution** (`{{token}}`) with environment switching.
- **Keyboard-first** — every action keyboard-reachable; mouse is augmentation.

**Specific features worth copying:**
- The empty-state pattern (`X is empty. Press <key> to <action>.`) — never just say "No data."
- Theme switching at runtime.
- Jump mode: label every focusable widget with a letter so a keyboard user never tabs through a form.

**Lessons:** Postman-style apps don't have to be GUIs. A well-designed TUI can replace bulky Electron apps and be faster, more keyboard-friendly, and offline-friendly.

**Stack:** Python, Textual, with `httpx` for HTTP. That is the released Posting 2.x line; `main` now holds Posting 3, a Go rewrite, so cite Posting 2 when you need a Textual example.

---

## Harlequin (Python, Textual)

> SQL IDE. Database client + editor.

**Layout:** IDE three-panel — schema/catalog (left), SQL editor (top-right), results table (bottom-right).

**What it does well:**
- **Multi-adapter** — DuckDB, SQLite, Postgres, MySQL/MariaDB, and ODBC from the maintainers, plus community adapters for many more databases, all from the same UI.
- **Tree-sitter SQL** highlighting in the editor.
- **Fast result virtualization** for million-row queries.
- **Query history** (`F8`) as a searchable screen.

**Specific features worth copying:**
- The plugin/adapter pattern (clean abstraction → swap database backend).
- Result virtualization for huge tables.

**Lessons:** when your TUI is a productivity tool, take performance seriously. Million-row results require real virtualization, not "render the first 1000."

**Stack:** Python, Textual, with `textual-fastdatatable` for performance.

---

## ranger / lf / nnn / broot (mostly Python and Go)

> File managers. The miller-columns lineage.

**Layout:** miller columns (parent / current / preview).

**What they do well:**
- **Vim-style navigation** (`hjkl`).
- **Bookmarks** (`m{a-z}`, `'{a-z}`).
- **Image preview** in the right column (terminal-graphics or ASCII).
- **Composable with shell** — `ranger --choosefile=/tmp/path` for "use ranger to pick a file in scripts."
- **Each handles a different niche:**
  - **ranger** — Python, mature, lots of features, slower startup.
  - **lf** — Go, very fast, minimal.
  - **nnn** — C, the fastest, most minimal.
  - **broot** — Rust, tree-mode + fuzzy filter at the same time.

**Lessons:** for browsing tools, startup time matters as much as a fancy UI. nnn opens in single-digit ms; ranger takes a noticeable beat. Both have devoted users; performance is a real axis to compete on.

---

## gitui (Rust, Ratatui)

> Git TUI in Rust.

**Layout:** persistent multi-panel, similar to lazygit but Rust-based.

**What it does well:**
- **Fast, even on huge repos.** Async git operations.
- **Diff viewer** with syntax highlighting.
- **Async fetch with no UI block.**
- **Push/pull progress** rendered in-app.

**Specific features worth copying:**
- The pattern of running git operations on a worker thread and posting progress events back.
- Showing diff with proper syntax highlighting (Tree-sitter or syntect).

**Lessons:** git operations on huge repos can be slow. If your tool wraps git, the user experience hinges on whether you keep the UI responsive. Threading-pool with progress events is the pattern.

**Stack:** Rust, Ratatui, `git2` plus `gix` (gitoxide).

---

## lazydocker (Go, gocui)

> Docker TUI. Same paradigm as lazygit.

**Layout:** persistent multi-panel — sidebar with Project / Containers / Services / Images / Volumes / Networks, main panel showing details (Logs / Stats / Env / Config / Top tabs).

**What it does well:**
- **Tabbed-within-panel** in the main panel — `[`/`]` cycle Logs/Stats/Env/Config/Top.
- **Live log streaming** with auto-scroll toggle.
- **Resource graphs** (CPU / memory / network) in the Stats tab.
- **Custom commands** in YAML config.

**Lessons:** if you're building a "lazy*" tool for some other domain (kubernetes, AWS, your CI system), this template works. Persistent sidebar + main panel with tabs + per-context single letters + footer hint bar.

**Stack:** Go, gocui (jesseduffield's fork).

---

## neovim (C + Lua)

> Editor. The most-customized TUI in existence.

**Why study:** the *plugin* and *config* ecosystem teaches lessons about extensibility. Every successful neovim plugin (telescope.nvim, which-key.nvim, lualine.nvim, nvim-tree.lua, lazy.nvim) demonstrates a UI pattern worth knowing:

- **telescope.nvim** — fuzzy picker for everything (files, buffers, LSP symbols, git refs). The "picker for X" pattern generalizes.
- **which-key.nvim** — popup showing available leader-key follow-ups. Discoverability by default.
- **lualine.nvim** — status line as a configurable component pipeline. Mode + git branch + diagnostics + filename + position.
- **nvim-tree.lua** — file tree sidebar with vim navigation.
- **lazy.nvim** — plugin manager with lazy-loading. Cold-start matters.

**Lessons:** the design choices in vim — modal editing, leader keys, command mode, registers, marks, buffers, splits — have proven robust over decades. Even non-editor TUIs benefit from borrowing these mental models when they fit.

**Stack:** C (core), Lua (config and plugins).

---

## Toolong (Python, Textual)

> Log viewer for multi-GB files.

**Layout:** single panel (the log) with optional filter sidebar.

**What it does well:**
- **Virtualization for multi-GB files.** Doesn't load the whole file.
- **Real-time tailing** with `-f`-style follow.
- **Regex filter** that doesn't slow down on huge files.
- **Merge multiple log files** by timestamp.
- **Pretty syntax highlighting** for common log formats (JSON, syslog, NGINX).

**Lessons:** when working with potentially-huge data, virtualization is the difference between "it works on my 10MB sample" and "it works in production on 50GB rotated logs." Plan for it from day one.

**Stack:** Python, Textual. Toolong has had no release since April 2024; study it for the design, not as a maintained dependency.

---

## Glow (Go, Bubble Tea)

> Markdown reader for the terminal.

**Layout:** single panel (the rendered Markdown) with optional file picker.

**What it does well:**
- **Beautiful Markdown rendering** via Glamour.
- **Multiple themes** (light, dark, custom JSON).
- **Local + remote files** (GitHub URLs work directly).
- Scope discipline: Glow 2.0 removed the cloud stash feature rather than carry a dead backend.

**Specific features worth copying:**
- The decision to ship as both a CLI (`glow README.md` pipes to terminal) and a TUI (`glow` opens the picker).
- Glamour's themes as JSON — define style once, apply everywhere.

**Stack:** Go, Bubble Tea v2 (since Glow 3), Glamour.

---

## Claude Code, GitHub Copilot CLI, Gemini CLI (TypeScript)

> AI coding assistants in the terminal.

**Layout:** chat-like — transcript above, input pinned at the bottom.

**Two render models, both shipping:**
- **Inline, append-only** — Gemini CLI's default and Claude Code's classic renderer. Finalized turns go to native scrollback (Ink's `<Static>` or an equivalent), so the terminal's own scrollback, search, and copy work for free. The cost grows with the transcript: long streaming sessions can flicker, jump the scroll position, and grow memory.
- **Alternate screen with a virtualized transcript** — Claude Code's fullscreen renderer (a research preview; the default for users who first ran Claude Code on or after May 6, 2026; `/tui` switches) and Copilot CLI (default since 1.0.8, no opt-out since 1.0.12). Only visible messages render, so memory stays flat and nothing flickers. The cost: the app must rebuild what the terminal gave for free — search, copy, exporting the transcript to scrollback — and keep an inline fallback. Claude Code forces its classic renderer in screen-reader mode.

**What they do well:**
- **Status indicators** (thinking, tool-use, error states) without overwhelming.
- **Inline diff rendering** for code changes.
- **`/`-prefixed slash commands** for actions (clear, model switch, etc.).
- **Shell integration** — paste files, run commands, read working directory.

**Lessons:** a chat TUI is something users live in, so it faces the same inline-versus-alt-screen choice as any other app (`visual-patterns.md` → *Inline, alt-screen, or overlay*). Inline with `<Static>` for finalized turns is the simpler start. Move to the alternate screen with a virtualized transcript when sessions run long, and ship search, copy, scrollback export, and an inline or accessible fallback with it.

**Stack:** TypeScript. Ink's README lists all three as users, and Gemini CLI renders with a fork of Ink (React, Yoga); the internals of Claude Code's and Copilot CLI's alternate-screen renderers aren't documented.

---

## starship (Rust, no UI framework)

> Cross-shell prompt.

**Why study:** not technically a TUI, but the **performance discipline** is exemplary. Starship runs on every prompt, so startup is a budget users feel.

**What it does well:**
- **Per-module time budgets instead of a single startup number** — `scan_timeout` (30ms) and `command_timeout` (500ms) bound every module; a module that overruns is dropped from that prompt with a warning instead of delaying it.
- **Single TOML config** with semantic blocks (one per "module": `[character]`, `[directory]`, `[git_branch]`).
- **Cross-shell** — bash, zsh, fish, pwsh, ion, nu — all from one binary.

**Lessons:** for any tool that runs frequently (shell prompts, file pickers, fuzzy finders), startup time is a feature. Profile, optimize, never regress. Users feel 100ms even if they can't articulate it.

**Stack:** Rust, no UI framework — direct ANSI emission.

---

## yt-dlp / aria2 (CLI, but excellent terminal output)

> Downloaders.

**Why study:** great example of progress UI in non-TUI tools. They show:
- **Multi-stream progress** (parallel downloads, all visible).
- **Adaptive output** — full progress bars on TTY, simple percentage on CI.
- **Final summary** at the end (success / failed counts, total size).

**Lessons:** even non-interactive CLIs benefit from thoughtful output design. Show progress, show summary, degrade for non-TTY.

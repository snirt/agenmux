# Rust ecosystem — Ratatui, crossterm, Cursive, clap

Ratatui dominates Rust TUI development — thousands of crates build on it. Forked from `tui-rs` in February 2023. Notable production users: **gitui, bottom, yazi, atuin, bandwhich, oha, tokio-console, csvlens, gpg-tui, systemctl-tui, tenere, kdash**. Helix uses its own custom renderer but follows similar patterns.

**Current line: 0.30** (0.30.0 stable since December 2025; 0.30.2 current). 0.30 reorganized Ratatui into a modular workspace of crates (better compile times and API stability) and added `no_std` support, but the main `ratatui` crate still re-exports everything, so most apps import it exactly as before. The version detail that actually bites in practice is the crossterm-compatibility story below.

**Contents:**
- [Quick recommendation](#quick-recommendation)
- [Ratatui](#ratatui-ratatuiratatui) — [Lifecycle and terminal handoff](#lifecycle-and-terminal-handoff) · [Widgets](#widgets) · [Layout](#layout) · [Styling](#styling)
- [Backends](#backends-crossterm-vs-termion-vs-termwiz)
- [State management](#state-management-patterns) · [Async with Tokio](#async-with-tokio)
- [Testing](#testing) · [Debugging](#debugging)
- [Companion crates](#companion-crates)
- [Panic safety](#panic-and-error-safety--the-critical-pattern)
- [Alternatives to Ratatui](#alternatives-to-ratatui)
- [Pitfalls](#pitfalls)
- [Notable Rust TUI apps](#notable-rust-tui-apps-to-study)
- [CLI design in Rust](#cli-design-in-rust)

## Quick recommendation

| If the user wants… | Use |
|---|---|
| Modern TUI in Rust | **Ratatui + Crossterm** |
| Form-heavy app with dialogs/menus | **Cursive** (callback-driven, retained-mode) |
| React-like declarative TUI | **iocraft** (newer, hooks + JSX-style + taffy flexbox) |
| Argument parsing for CLIs | **clap** (derive API) |
| Pretty terminal colors | **owo-colors** (zero-allocation, recommended) |
| Non-TUI progress bars | **indicatif** |
| Interactive prompts (one-shot) | **inquire** (modern) or **dialoguer** (stable) |
| Rich panic/error reports | **color-eyre** |
| Fancy modern wizards (Clack-style) | **cliclack** |

**Default: Ratatui + Crossterm + clap + color-eyre.** Start from `cargo generate ratatui/templates` and pick one of its six templates by app size: `simple` or `event-driven` for sync apps, `simple-async` or `event-driven-async` when you need Tokio, and `component` (Tokio, action channel, config, logging) only for large multi-pane apps.

---

## Ratatui (ratatui/ratatui)

**Architectural model: immediate-mode rendering.** Every frame, the application redraws the entire UI from current state. The library handles diffing between intermediate buffers and emits only changed cells — "a video codec for text."

The mental model is **"UI = f(state)"**. You manage app state, the event loop, and timing yourself. Ratatui handles the rendering math.

**Canonical app structure:**

```rust check=rust/canonical
use ratatui::{prelude::*, widgets::*, DefaultTerminal};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use color_eyre::Result;

fn main() -> Result<()> {
    color_eyre::install()?;            // install reporting before Ratatui wraps the hook
    let mut terminal = ratatui::init();
    let result = App::default().run(&mut terminal);
    ratatui::restore();
    result
}

#[derive(Default)]
struct App {
    counter: i32,
    should_quit: bool,
}

impl App {
    fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.should_quit {
            terminal.draw(|frame| self.draw(frame))?;
            self.handle_events()?;
        }
        Ok(())
    }

    fn draw(&self, frame: &mut Frame) {
        frame.render_widget(
            Paragraph::new(format!("Counter: {}", self.counter))
                .block(Block::bordered().title("Demo")),
            frame.area(),
        );
    }

    fn handle_events(&mut self) -> Result<()> {
        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Press {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
                    // raw mode delivers Ctrl+C as a key event, not SIGINT
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.should_quit = true
                    }
                    KeyCode::Char('+') | KeyCode::Right => self.counter += 1,
                    KeyCode::Char('-') | KeyCode::Left => self.counter -= 1,
                    _ => {}
                }
            }
        }
        Ok(())
    }
}
```

`ratatui::init()` enables raw mode, enters the alt screen, constructs a `Terminal<CrosstermBackend>`, and installs a panic hook that restores the terminal before delegating to the hook already installed. `ratatui::restore()` handles normal shutdown. Install reporting hooks such as `color_eyre` **before** `ratatui::init()` so Ratatui can wrap them with terminal restoration.

### Lifecycle and terminal handoff

Ratatui owns rendering and terminal setup, but not the application event loop, subprocesses, or general process-signal policy. Prefer the managed API and make every external lifecycle event converge on the loop boundary.

| Boundary | Ratatui 0.30 contract |
|---|---|
| Normal exit | Prefer `ratatui::run(...)`; it initializes, runs the closure, and restores afterward. With `init()` / `restore()`, retain the loop result, restore, then return the result so errors cannot skip cleanup. |
| SIGTERM | Ratatui does not turn process signals into application events. Use the owning runtime or signal integration to send a quit event or cancellation into the loop, then let the closure return through managed cleanup. Default SIGTERM and SIGKILL do not unwind Rust cleanup code. |
| Interactive child | Stop the input reader first (a still-running reader steals the child's keystrokes and terminal capability responses), restore, run and wait for the child, reinitialize, clear, and redraw. Keep the child's result; if child and reentry both fail, report both instead of letting `?` discard one. See the snippet below. |
| Foreground suspend | On Unix, use the same temporary-handoff sequence, send SIGTSTP only after restoration, then reinitialize and redraw after SIGCONT. Offer another path on Windows rather than assuming job-control signals exist. |

Following Ratatui's official [external-editor recipe](https://ratatui.rs/recipes/apps/spawn-vim/):

```rust check=rust/editor-handoff
use std::{io::stdout, process::{Command, ExitStatus}};
use ratatui::crossterm::{execute, terminal::{enable_raw_mode, EnterAlternateScreen}};
use color_eyre::eyre::eyre;

fn run_editor(terminal: &mut DefaultTerminal, app: &mut App) -> Result<ExitStatus> {
    app.input.stop();                                    // async apps: cancel the EventStream task
    ratatui::restore();
    let child = Command::new("vim").arg(&app.path).status(); // keep it; no `?` yet
    let reentry = enable_raw_mode()
        .and_then(|()| execute!(stdout(), EnterAlternateScreen))
        .and_then(|()| terminal.clear());                // next draw repaints every cell
    app.input.start();
    app.reload();                                        // the child may have changed state
    match (child, reentry) {
        (Ok(status), Ok(())) => { terminal.draw(|f| app.draw(f))?; Ok(status) }
        (child, reentry) => Err(eyre!("child: {child:?}; reentry: {reentry:?}")),
    }
}
```

Reentry steps are not transactional (see the fallible-setup warning below), and a successful redraw proves only that the renderer recovered.

**Inline viewport:** `Terminal::with_options` with `Viewport::Inline(height)` renders below the prompt instead of on the alternate screen. To emit permanent log or output lines above the live UI, use `Terminal::insert_before`: inserted lines push the viewport down, then scroll into scrollback above it.

### Widgets

**Built-in:**
- **`Block`** — borders, title, padding. The container for almost everything.
- **`Paragraph`** — text with wrap, alignment, scroll offset.
- **`List`** + **`ListState`** — selectable list with virtualized rendering.
- **`Table`** + **`TableState`** — selectable table, virtualized.
- **`Tabs`** — tab bar.
- **`Chart`** — line/scatter chart with X/Y axes.
- **`BarChart`** — horizontal or vertical bars.
- **`Gauge`** / **`LineGauge`** — progress indicator.
- **`Sparkline`** — compact trend visualization.
- **`Canvas`** — sub-cell drawing using Braille markers; great for maps, plots, custom shapes.
- **`Scrollbar`** — pair with any scrollable widget.
- **`Clear`** — paints over an area; use for popup hole-punching.
- **`Calendar`** — monthly calendar.
- **`Fill`** — paints an area with one repeated symbol and style (0.30.1+).

**Custom widgets** implement the `Widget` trait (one-shot) or `StatefulWidget` (with associated state). Both are zero-cost — they're just `(area, buf)` consumers.

**Third-party widgets worth knowing:**
- **`ratatui-textarea`** — multi-line editor with vim/emacs-style bindings. It is the Ratatui org's fork of `tui-textarea` and supports Ratatui 0.30; `tui-textarea` itself is unmaintained and its last release (0.7) requires Ratatui 0.29. Install it from crates.io or `github.com/ratatui/ratatui-textarea`: a malicious GitHub fork with the same name was removed in March 2026.
- **`tui-input`** — single-line input.
- **`tui-tree-widget`** — hierarchical tree.
- **`tui-big-text`** (banner text), **`tui-popup`** (modal popups), **`tui-scrollview`** (scrollable viewport) — maintained together in `ratatui/tui-widgets`, also available as the `tui-widgets` bundle crate.
- **`tui-logger`** — in-app log pane.
- **`throbber-widgets-tui`** — spinners.
- **`tachyonfx`** — shader-like effects and transitions, from the Ratatui org.

### Layout

Constraint-based using Cassowary (the same algorithm as iOS Auto Layout):

```rust check=rust/layout
use ratatui::layout::{Layout, Constraint};

let [header, body, status] = Layout::vertical([
    Constraint::Length(3),    // 3 rows for header
    Constraint::Min(0),       // body fills remaining
    Constraint::Length(1),    // 1 row for status
]).areas(frame.area());

let [sidebar, main] = Layout::horizontal([
    Constraint::Percentage(30),
    Constraint::Percentage(70),
]).areas(body);
```

Constraint variants:
- `Length(n)` — exactly n cells.
- `Min(n)` — at least n cells (grows to fill).
- `Max(n)` — at most n cells.
- `Percentage(p)` — p% of available.
- `Ratio(num, den)` — fraction.
- `Fill(weight)` — proportional to weight (prefer over `Percentage` when ratios are simple).

Ratatui caches split results by default (the `layout-cache` feature keeps an LRU keyed by area and layout), so re-splitting the same area is cheap; splitting once per frame and passing the `Rect`s down is still the simpler structure.

### Styling

```rust check=rust/styling
use ratatui::style::{Color, Modifier, Style, Stylize};

// Direct API
let style = Style::default()
    .fg(Color::Yellow)
    .bg(Color::Black)
    .add_modifier(Modifier::BOLD);

// Stylize extension trait (preferred for brevity)
let span = "Hello".bold().yellow().on_black();
```

**Colors:** 16 ANSI named colors (`Color::Red`, `Color::LightRed`, …), `Color::Indexed(u8)` for 256-color, `Color::Rgb(r, g, b)` for truecolor. Detect terminal capability via `ratatui::crossterm::style::available_color_count()` if you want to gate features.

**Modifiers:** `BOLD`, `DIM`, `ITALIC`, `UNDERLINED`, `SLOW_BLINK`, `RAPID_BLINK`, `REVERSED`, `HIDDEN`, `CROSSED_OUT`. Avoid blink and crossed-out — poorly supported.

## Backends: Crossterm vs Termion vs Termwiz

- **Crossterm** — default, cross-platform (Linux/macOS/Windows), pure Rust, MIT. Use this unless you have a specific reason not to.
- **Termion** — Unix-only, older, smaller. Choose only if you want Unix-only and minimal dependencies.
- **Termwiz** — cross-platform terminal library from the WezTerm project. The library can encode Sixel, iTerm2, and kitty image sequences, but Ratatui's termwiz backend doesn't expose them; for images in a Ratatui app, use `ratatui-image`, which works with Crossterm by default.
- **Termina** — `termina` feature (`TerminaBackend`, Ratatui 0.30.2+), built on the Helix editor's terminal library, which sits between Crossterm and Termwiz and targets newer VT extensions such as the kitty keyboard protocol. `ratatui::init()` is Crossterm-only, so set up raw mode and cleanup through Termina yourself. Newer and less proven than Crossterm; choose it only when you need what it exposes.
- **mousefood** — `embedded-graphics` backend over `ratatui-core`, taking Ratatui's `no_std` support to embedded hardware displays.

**Crossterm version conflicts** are a foot-gun: pulling two semver-incompatible Crossterm majors causes separate event queues and broken raw-mode tracking. Always run `cargo tree -p crossterm` and verify only one version. Crossterm 0.29 (April 2025) is the current stable; 0.28 is the legacy pin. Ratatui 0.30 exposes per-version feature flags (`crossterm_0_28`, `crossterm_0_29`) so widget-library authors can pin a specific Crossterm without forcing it on downstream apps; the default is 0.29, and if both flags end up enabled (feature unification) it is not an error — `crossterm_0_29` takes precedence. In app code, import Crossterm through the re-export (`use ratatui::crossterm`) rather than a separate `crossterm` dependency, so you always get the exact version Ratatui uses.

## State management patterns

**1. Monolithic App struct** (simplest, used in most examples):

```rust check=rust/app-struct
struct App {
    items: Vec<Item>,
    list_state: ListState,
    input: String,
    mode: Mode,
    // ...
}
```

**2. Elm Architecture** (Model + Message + update + view):

```rust check=rust/elm
enum Msg { Increment, Decrement, Quit }

fn update(model: &mut Model, msg: Msg) -> Option<Cmd> { todo!() }
fn view(model: &Model, frame: &mut Frame) { /* ... */ }
```

Useful when state is complex and you want testable update logic.

**3. Component pattern** — each component implements an `init`/`handle_events`/`update`/`draw` interface and communicates via `mpsc::UnboundedSender<Action>`. The official `component` template in `ratatui/templates` uses this with Tokio. Best for larger apps. If you'd rather adopt a framework than roll your own, **tui-realm** (crate `tuirealm`) formalizes this pattern on top of Ratatui (components, subscriptions, message-based events).

## Async with Tokio

The standard pattern: a cancellable task reads Crossterm's `EventStream` into an `mpsc` channel; the main loop `select!`s over that channel and a tick interval, and calls `terminal.draw` each pass. `EventStream` needs Crossterm's `event-stream` feature, which Ratatui does not enable: add `crossterm = { version = "0.29", features = ["event-stream"] }` matching Ratatui's Crossterm version so Cargo unifies them, and keep importing through `ratatui::crossterm`.

```rust check=rust/async-input
use std::time::Duration;
use futures::StreamExt;
use ratatui::crossterm::event::{Event, EventStream};
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_util::sync::CancellationToken;

fn spawn_input(tx: mpsc::UnboundedSender<Event>, stop: CancellationToken) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut events = EventStream::new();
        loop {
            tokio::select! {
                _ = stop.cancelled() => break,
                next = events.next() => match next {
                    Some(Ok(event)) => { let _ = tx.send(event); }
                    _ => break,
                },
            }
        }
    })
}

// Main loop
let (tx, mut rx) = mpsc::unbounded_channel();
let stop = CancellationToken::new();
let input = spawn_input(tx, stop.clone());
let mut tick = tokio::time::interval(Duration::from_millis(250));
while !app.should_quit {
    terminal.draw(|f| app.draw(f))?;
    tokio::select! {
        event = rx.recv() => match event {
            Some(event) => app.handle(event),
            None => break, // reader ended (EOF or error); stop instead of ticking with no input
        },
        _ = tick.tick() => app.on_tick(),
    }
}
stop.cancel();
input.await?;
```

Cancel and await the reader before `ratatui::restore()` or before handing the terminal to a child (see [Lifecycle and terminal handoff](#lifecycle-and-terminal-handoff)); after reentry, spawn a fresh reader with a new token.

For sync-only apps, `event::poll(Duration::from_millis(250))` then `event::read()` works without Tokio.

## Testing

`TestBackend` lets you assert against rendered output:

```rust check=rust/test-backend
use ratatui::{backend::TestBackend, Terminal};

let backend = TestBackend::new(20, 5);
let mut terminal = Terminal::new(backend)?;
terminal.draw(|f| app.draw(f))?;
terminal.backend().assert_buffer_lines([
    "┌Demo──────────────┐",
    "│Counter: 0        │",
    "│                  │",
    "│                  │",
    "└──────────────────┘",
]);
```

Pair with **`insta`** for snapshot testing — `insta::assert_snapshot!(terminal.backend())`. Snapshots are stored as text files and reviewed via `cargo insta review`. This is the officially documented recipe (ratatui.rs → Recipes → Testing), with one caveat straight from that page: **snapshots capture text only — color and style are not asserted.** When color matters (selected-row highlight, error styling), compare `Buffer`s instead: build the expected buffer with `Buffer::with_lines(...)`, apply `set_style` to the regions you care about, and `assert_eq!` against the backend's buffer — the official counter-app tutorial demonstrates exactly this.

For the lifecycle layer TestBackend can't reach (event loop, key handling, raw-mode setup and teardown, exit code), the same official recipe now shows a PTY end-to-end test with `termlens`: spawn `env!("CARGO_BIN_EXE_<app>")` at a fixed size, wait for visible text, send keys, and snapshot `terminal.screen()` with insta. Keep it to one or two flows, and pin termlens's version because the crate is new (2026).

**Test at multiple sizes.** Resize bugs live at the edges, so run the same render across the contract sizes — 80×24, 60 columns (e.g. 60×24), and your declared hard minimum — snapshotting each under a size-suffixed name (`app_60x24`). Parameterizing per-size with `rstest` (which ratatui itself uses for its own tests) is a natural fit, though that combination is community practice rather than an official recipe. Extracting layout math into a pure `fn compute_layout(area: Rect) -> ...` makes per-size assertions cheap — no terminal needed at all.

Real-world anchors: **gitui**'s first insta + TestBackend snapshots (PR #2411) were reverted because the refactor that made the main loop testable dropped the initial notify event, so the app opened blank for one tick interval; the re-landed version (PR #2813, merged April 2026) restores that event. The caution: when you restructure an event loop so tests can drive it, the app's own startup path is what regresses, so cover first-draw behavior in the same tests. **openai/codex** keeps its Ratatui TUI (`codex-rs/tui`) under broad insta coverage, about 200 snapshot files of rendered output, so any change to visible TUI output surfaces as a snapshot diff to review (`cargo insta pending-snapshots`, `cargo insta accept`).

## Debugging

`println!` and `dbg!` are broken inside a running TUI: raw mode stops newline processing — crossterm's docs say it directly, "`println!` can't be used, use `write!` instead" — and anything printed is stomped by the next draw or hidden entirely under the alt screen. The working options:

- **Log to a file and tail it** (official recipe): `tracing` + `tracing-subscriber` (env-filter) writing to a plain file with ANSI disabled, then `tail -f app.log` in a second terminal.
- **In-app debug pane** (official recipe): keep `show_debug: bool` in app state, split off a column when toggled, and render `format!("{state:#?}")` into it.
- **tui-logger** (0.18.x, actively maintained) — the ready-made in-app log widget, built on `log`, with `slog` and `tracing` support behind the `slog-support` and `tracing-support` features; the official debug recipe points to it as an alternative.
- **Debuggers:** attach from a *second* terminal (`lldb -p <pid>` / `gdb -p`) or use an IDE debugger so the debug console is separate from the app's terminal — stopping the process in its own terminal leaves you at a prompt that's still in raw mode + alt screen. (Convention rather than official doctrine, but it follows directly from the raw-mode behavior.)

**Profiling:** use `cargo flamegraph` (perf on Linux, xctrace on macOS) or `samply`; ratatui.rs has no profiling recipe. In raw mode Ctrl+C is a key event, not SIGINT, so it won't end a recording. Quit through your app's own key, or attach to a running app with `flamegraph --pid <pid>`. In Tokio apps, `tokio-console` (via `console-subscriber`) shows per-task poll and busy time. The usual slow path isn't widget construction, which Ratatui expects every frame, but O(n) work inside the render closure, such as collecting a large `Vec` or recomputing string widths on every draw.

## Companion crates

- **clap** — argument parsing. Derive API:
  ```rust check=rust/clap-derive
  use clap::{Parser, Subcommand};

  #[derive(Parser)]
  struct Cli {
      #[arg(short, long)]
      verbose: bool,

      #[command(subcommand)]
      command: Commands,
  }

  #[derive(Subcommand)]
  enum Commands {
      /// List items
      List,
  }
  ```
  Best-in-class argument parsing with auto-generated help and validation; shell completions come from the companion `clap_complete` crate.

- **color-eyre** — installs a panic hook that prints rich error reports with span traces and backtraces. Install it before `ratatui::init()` or `ratatui::run()`; Ratatui then wraps the reporting hook and restores terminal state before delegating to it.

- **owo-colors** — zero-allocation color formatting. Direct styling emits color; opt into its `supports-colors` feature and use `if_supports_color`, or add your own policy, when output must account for TTY capability and `NO_COLOR`. Recommended over `colored` (older, allocates) and `ansi_term` (unmaintained).

- **indicatif** — progress bars for non-TUI CLIs. Auto-hides on non-TTY.

- **inquire** — modern interactive prompts (text, select, multi-select, confirm). The Rust answer to Inquirer.js.

- **dialoguer** — older, stable alternative to inquire.

- **cliclack** — port of the JS Clack library; modern wizard-style prompts with Unicode connectors.

- **ratatui-image** — image display in Ratatui apps via Sixel/kitty/iTerm2 protocols.

## Panic and error safety — the critical pattern

A Ratatui app that exits badly can leave the user in raw mode + alt screen + no cursor. On current Ratatui, prefer `ratatui::run()` for the complete managed lifecycle or `ratatui::init()` when the application needs to own the loop. Install any reporting hook first, then let Ratatui wrap it:

```rust check=rust/run
fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    ratatui::run(|terminal| App::default().run(terminal))
}
```

If you construct `Terminal` and configure Crossterm raw/alternate-screen state manually, you own both normal and panic cleanup. In that case, wrap the existing reporting hook yourself:

```rust check=rust/panic-hook
color_eyre::install()?;
let report_hook = std::panic::take_hook();
std::panic::set_hook(Box::new(move |info| {
    ratatui::restore();
    report_hook(info);
}));
```

Do not add that custom wrapper on top of `ratatui::run()` or `ratatui::init()`; those managed APIs already install it. The fallible helpers are not transactional: `try_init()` can return after an earlier setup step changed terminal state, and `try_restore()` stops at its first teardown error. If the caller requires fallible setup or teardown, handle an error with independent best-effort cleanup attempts (disable raw mode, leave the alternate screen, disable mouse/paste modes, show the cursor) rather than assuming one returned `Err` rolled everything back. `ratatui::restore()` performs the default Crossterm teardown—custom backends must run their matching backend-specific teardown instead. Whichever path you choose, inject setup/teardown failures and verify normal exit, returned errors, and panics in a PTY.

## Alternatives to Ratatui

**Cursive** (`cursive`) — callback-driven, retained-mode, ncurses-like. Widget-rich (Dialog, EditView, SelectView, TextArea, LinearLayout, StackView, etc.). Best for form-heavy apps with dialogs and menus where you want a higher-level API. Less popular than Ratatui but well-documented. The `cursive` crate has had no release since 0.21.1 (August 2024) even though its repo is active, and it depends on crossterm 0.28, so mixing it with Ratatui-ecosystem crates puts two crossterm versions in one build.

```rust check=rust/cursive
use cursive::views::TextView;

let mut siv = cursive::default(); // a runnable Cursive on the default backend
siv.add_layer(TextView::new("Hello, world!"));
siv.add_global_callback('q', |s| s.quit());
siv.run();
```

**iocraft** — newer, declarative React-like with hooks and JSX-style macros. Uses **taffy** (the same flexbox engine used by Bevy and Servo). Choose if you want a modern declarative API or are coming from React/Ink.

**Dioxus TUI / Plasmo** — abandoned React-like renderer for Dioxus. Unmaintained; don't start new work on it.

**Ratzilla** (`ratatui/ratzilla`) — run Ratatui apps in the browser via WASM; maintained under the ratatui org. Handy for demos and web playgrounds of terminal apps.

**Original `tui-rs`** — the predecessor to Ratatui, archived. Migrate to Ratatui.

## Pitfalls

1. **Panic without terminal restore.** Use the panic hook pattern above.
2. **Crossterm version skew.** Run `cargo tree -p crossterm`; ensure one version.
3. **Naive redraw loop.** Don't busy-loop calling `draw`. Use `event::poll(timeout)` for sync, `tokio::select!` for async.
4. **On Windows, both Press and Release events fire.** Filter with `key.kind == KeyEventKind::Press`.
5. **Stateful widgets need state ownership.** `List`, `Table`, `Scrollbar` are `StatefulWidget`s — you pass `&mut ListState` / `TableState` / `ScrollbarState` at render time; the state lives in your App struct, not the widget.
6. **Mouse capture disables terminal text selection.** Most emulators bypass with Shift; document this.
7. **`String::len()` is bytes, not cells.** Use `Line::width()` / `Span::width()` for Ratatui text, or `unicode_width::UnicodeWidthStr::width(s)` for raw strings.
8. **No accessibility layer.** Ratatui has no screen-reader support; [ratatui#2610](https://github.com/ratatui/ratatui/issues/2610) tracks it, and the experimental `tui-a11y` crate is Linux-only. Give screen-reader users a linear mode; see `visual-patterns.md` → *Accessibility*.

---

## Notable Rust TUI apps to study

- **gitui** — git client; Ratatui's flagship demo.
- **bottom** (btm) — system monitor; widget dashboard pattern.
- **yazi** — file manager with image preview; miller columns.
- **atuin** — shell history; fzf pattern + sync backend.
- **csvlens** — CSV viewer.
- **bandwhich** — network monitor.
- **oha** — HTTP load tester.
- **tokio-console** — async runtime debugger; powered by Tokio's tracing.
- **systemctl-tui** — systemd manager.
- **gpg-tui** — GPG key manager.
- **kdash** — Kubernetes TUI.
- **tenere** — chat TUI for LLMs; uses `ratatui-textarea` for input.
- **helix** — modal editor; custom renderer but Ratatui-adjacent patterns.
- **zellij** — terminal multiplexer.

When the user is building something similar, point them at the relevant repo. Ratatui's own `examples/` directory is also gold.

---

## CLI design in Rust

For non-TUI CLIs:

- **clap** for argument parsing.
- **indicatif** for progress bars and spinners (auto-hides on non-TTY).
- **owo-colors** for terminal colors; use its `supports-colors` feature plus `if_supports_color`, or an explicit app policy, to honor terminal capability and `NO_COLOR`.
- **anyhow** or **color-eyre** for error reporting.
- **env_logger** or **tracing** + **tracing-subscriber** for logging.

Pair with the principles in `references/cli-basics.md` for argument design, exit codes, and stream handling.

---

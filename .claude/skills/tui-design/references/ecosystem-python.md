# Python ecosystem — Textual, Rich, prompt_toolkit

Three tools occupy distinct niches: **Textual** (the modern reactive TUI framework — async-first, CSS-styled, web-deployable), **Rich** (output formatting — the rendering engine inside Textual, excellent standalone), and **prompt_toolkit** (input-focused REPLs and shells — powers IPython, ptpython, mycli/pgcli/litecli). They solve different problems. Pick by what the program *is*, not just by Python familiarity.

**Contents:**
- [Quick recommendation](#quick-recommendation)
- [Textual](#textual-textualizetextual) — [Lifecycle and terminal handoff](#lifecycle-and-terminal-handoff) · [Widgets](#widgets) · [Layout (TCSS)](#layout--tcss) · [Events and messages](#events-and-messages) · [Reactive state](#reactive-state) · [Async and workers](#async-and-workers) · [Modal screens](#modal-screens)
- [Testing](#testing--pilot--pytest-textual-snapshot) · [Debugging](#debugging) · [Dev tools](#dev-tools)
- [Notable Textual apps](#notable-textual-apps) · [Pitfalls](#pitfalls)
- [Rich](#rich-textualizerich) · [prompt_toolkit](#prompt_toolkit) · [Other libraries](#other-libraries)
- [Argument parsing: argparse vs Click vs Typer](#argument-parsing-argparse-vs-click-vs-typer)

## Quick recommendation

| If the user wants… | Use |
|---|---|
| Full-screen TUI app | **Textual** |
| CLI tool with pretty output (tables, panels, syntax) | **Rich** |
| Interactive REPL or shell-like tool | **prompt_toolkit** |
| One or two prompts inside a CLI | **questionary** (built on prompt_toolkit); InquirerPy is unmaintained |
| Argument parsing with type hints | **Typer** (decorator API; bundles its own copy of Click since 0.26) |
| Argument parsing, decorator-style (no type hints) | **Click** |
| Simple progress bar | **tqdm** (max performance) or **alive-progress** (polished, redirect-safe) |

**Default for full TUI: Textual.** It's the most complete actively developed Python TUI framework and is genuinely well-designed. **Default for fancy CLI output: Rich.**

**Python floor:** target 3.11+ for new tools. Python 3.9 and 3.10 reached end of life in October 2025 and October 2026. Current Typer, Click 8.2+, prompt_toolkit 3.0.53 and pytest 9 need 3.10+, and cyclopts 5 needs 3.11+. Textual 8.2.8 and Rich 15 still install on 3.9.

**Distribution:** ship a package with a `[project.scripts]` entry point. Users install it isolated with `uv tool install <pkg>` (or `pipx install <pkg>`) and can run it once with `uvx <pkg>`; Harlequin and Toad document this route.

---

## Textual (Textualize/textual)

**Architectural model: reactive, async-first, message-passing.** Strongly inspired by web frameworks. App subclass + Widgets in a DOM-like tree (App → Screen → Widgets) + TCSS for layout/style + reactive attributes for state + Messages/Events for communication, all on asyncio.

**Status:** Textualize the company wound down in mid-2025; Will McGugan maintains Textual and Rich as open source. If the user asks "isn't Textual dead?" — no. It shipped five major versions (4.0 through 8.0) since the May 2025 announcement, with 8.x current as of mid-2026, but releases have slowed: the latest are Textual 8.2.8 (June 2026) and Rich 15.0.0 (April 2026). In October 2026 McGugan said he will keep maintaining both, though issue responses may be slow. Treat them as stable and maintained, not fast-moving. Those majors did carry breaking renames (`Static.renderable` → `Static.content` in 6.0, `Select.BLANK` → `Select.NULL` in 8.0), so pin your major and read the changelog when upgrading.

**Canonical structure:**

```python check=python/hello-app
from textual.app import App, ComposeResult
from textual.widgets import Header, Footer, Button, Label

class HelloApp(App):
    CSS = "#greeting { padding: 1 2; }"  # or CSS_PATH = "hello.tcss"; that file must exist
    BINDINGS = [
        ("q", "quit", "Quit"),
        ("d", "toggle_dark", "Toggle dark mode"),
    ]

    def compose(self) -> ComposeResult:
        yield Header()
        yield Label("Hello, world!", id="greeting")
        yield Button("Click me", id="go", variant="success")
        yield Footer()

    def on_button_pressed(self, event: Button.Pressed) -> None:
        self.query_one("#greeting", Label).update("Button pressed!")

    def action_toggle_dark(self) -> None:
        self.theme = "textual-light" if self.theme == "textual-dark" else "textual-dark"

if __name__ == "__main__":
    HelloApp().run()
```

`compose()` defines the widget tree (called once on mount). `BINDINGS` declares key bindings declaratively — they auto-render in the `Footer` widget. `action_*` methods are invoked by binding actions. `on_*` methods handle events from child widgets.

### Lifecycle and terminal handoff

Textual's driver restores application mode from `App.run()` / `run_async()` cleanup. Use App actions and messages for exit or handoff so widgets do not manipulate raw mode themselves.

| Boundary | Textual 8 contract |
|---|---|
| Normal exit | Call `self.exit(result, return_code=...)`; the run lifecycle shuts the driver down in a `finally` path. `return_code` is application metadata, so call `sys.exit(app.return_code)` after `app.run()` when the process must expose it. |
| SIGTERM | Desktop terminal drivers do not provide a general SIGTERM-to-`App.exit` contract. If a service requires graceful termination, let its platform-appropriate event-loop integration schedule `self.exit(...)`; do not restore the terminal directly in a low-level signal handler. SIGKILL cannot be cleaned up. |
| Interactive child | Run an argv-based subprocess inside `with self.suspend():`. Textual pauses input/output, restores the child-facing terminal, and resumes plus refreshes afterward. Reload any data the child could have changed; layout refresh does not reread it. Handle `SuspendNotSupported` with a non-terminal alternative because Textual Web cannot hand a local terminal to a child. |
| Foreground suspend | Bind or invoke `suspend_process`; Textual sends SIGTSTP and resumes application mode on Unix. It is intentionally a no-op on Windows and Textual Web. Subscribe to `app_resume_signal` to reload externally mutable data when needed. |

These are two different APIs: [`App.suspend()`](https://textual.textualize.io/guide/app/#suspend-context-manager) temporarily lends the terminal to code inside a context manager, while `suspend_process` performs Unix job control. Neither is a final-exit substitute.

**Inline mode:** `app.run(inline=True)` (since 0.55) renders below the prompt instead of on the alternate screen. Set its height with CSS on `Screen`, as in `Screen { &:inline { height: 50vh; } }`. Inline mode is not supported on Windows.

## Widgets

Textual ships dozens. Categories:

**Display:** `Label`, `Static`, `Digits`, `Pretty`, `Markdown`, `MarkdownViewer`, `RichLog`, `Log`, `Sparkline`, `Rule`, `Placeholder`, `LoadingIndicator`, `ProgressBar`.

**Input:** `Input`, `MaskedInput`, `TextArea` (multi-line, optional tree-sitter syntax highlighting — the `syntax` extras require Python 3.10+ since Textual 5.0), `Button`, `Switch`, `Checkbox`, `RadioButton`, `RadioSet`, `Select`, `OptionList`.

**Container/structure:** `Header`, `Footer`, `TabbedContent`, `TabPane`, `Tabs`, `Collapsible`.

**Data:** `DataTable` (cell/row/column cursors, sortable, virtualized — handles thousands of rows), `Tree`, `DirectoryTree`, `ListView` + `ListItem`.

`DataTable` is excellent — it's what you should use for any tabular display. Add columns with `add_columns`, rows with `add_rows`, configure cursor type (`cell`/`row`/`column`/`none`), enable `zebra_stripes`, hook into `on_data_table_row_selected`.

## Layout — TCSS

Layout is CSS:

```css check=python/textual-layout
Screen {
    layout: vertical;
}

#sidebar {
    width: 30;
    border: tall $accent;
}

#main {
    width: 1fr;
    layout: vertical;
}

DataTable {
    height: 1fr;
}

.error {
    color: $error;
    text-style: bold;
}
```

Concepts:
- **`layout`** — `vertical | horizontal | grid`.
- **Containers**: `Vertical`, `Horizontal`, `Grid`, `VerticalScroll`, `Center`, `Middle`.
- **Sizing**: cells (`width: 30`), percentages (`width: 50%`), `auto`, fractional units (`width: 1fr`).
- **Grid**: `grid-size: 3 4`, `grid-columns`, `grid-rows`, `grid-gutter`, `column-span`, `row-span`.
- **Docking**: `dock: top | right | bottom | left` for sticky edges. Used for `Header` and `Footer`.
- **Overflow**: `overflow-x: auto` adds scrollbars.

**Selectors**: type (`Button { ... }` matches subclasses too — different from web CSS), id (`#dialog`), class (`.error`), pseudo (`:focus`, `:hover`, `:disabled`, `:dark`, `:light`). Nesting works. **Theme variables** (`$primary`, `$panel`, `$text`, `$accent`, `$error`, etc.) — semantic tokens that change with the active theme.

## Events and messages

Two ways to handle events:

**1. Name convention:**
```python check=python/on-handler
def on_button_pressed(self, event: Button.Pressed) -> None:
    if event.button.id == "submit":
        ...
```

**2. `@on` decorator with CSS selector** (preferred when you have many of the same widget type):
```python check=python/on-decorator
from textual import on

@on(Button.Pressed, "#submit")
def handle_submit(self) -> None:
    ...

@on(Button.Pressed, ".danger")
def handle_danger(self, event: Button.Pressed) -> None:
    ...
```

Messages bubble up the DOM. Call `event.stop()` to halt propagation. The idiom: **"attributes down, messages up"** — parents set state on children via attributes; children notify parents via messages.

## Reactive state

```python check=python/reactive
from textual.reactive import reactive
from textual.widget import Widget

class Counter(Widget):
    count: reactive[int] = reactive(0)
    label: reactive[str] = reactive("")

    def watch_count(self, old: int, new: int) -> None:
        # called automatically when count changes; repaint is already automatic
        self.log(f"count {old} -> {new}")

    def validate_count(self, value: int) -> int:
        # called before assignment; can clamp or transform
        return max(0, min(10, value))

    def compute_label(self) -> str:
        # derived, read-only reactive; recomputed when count changes
        return f"Count: {self.count}"

    def render(self) -> str:
        return self.label
```

`compute_<name>` only runs for a declared reactive of that name (`label` here), and that reactive becomes read-only. Don't name one `display` — that's Widget's built-in visibility property. Execution order on assignment: **validate → assign → watch → compute**. Modifiers on `reactive(...)`:
- `init=False` — don't fire watcher on initial assignment.
- `always_update=True` — fire even when value didn't change.
- `layout=True` — trigger a re-layout, not just a re-render.
- `bindings=True` — re-evaluate `BINDINGS` (useful for context-sensitive bindings).
- `recompose=True` — re-run `compose()` (rebuild children).

This is the heart of Textual's "reactive" claim — it's genuinely declarative state-driven UI.

**Hiding dependent widgets:** when one value controls another widget, react in `watch_<name>(old, new)` on the `reactive()` attribute, and hide the dependent widget with `widget.display = False` (removes it from layout) or `.visible = False` (hides it without reflowing).

## Async and workers

Handlers can be `async def`. The `@work` decorator turns methods into background workers:

```python check=python/workers
import httpx
from textual import work
from textual.widgets import Static

@work(exclusive=True)
async def fetch_data(self, url: str) -> None:
    async with httpx.AsyncClient() as client:
        response = await client.get(url)
        response.raise_for_status()
    self.query_one("#result", Static).update(response.text)

# For blocking code:
@work(thread=True)
def compute_heavy(self) -> None:
    result = expensive_sync_thing()
    self.call_from_thread(self.update_result, result)
```

`call_from_thread(fn, *args)` is required when modifying UI from a thread (vs an asyncio task). `set_interval(secs, callable)` schedules periodic UI updates without blocking.

## Modal screens

Push a screen on top for dialogs/modals:

```python check=python/modal
from textual import on, work
from textual.app import ComposeResult
from textual.screen import ModalScreen
from textual.widgets import Button, Label

class ConfirmDialog(ModalScreen[bool]):
    def compose(self) -> ComposeResult:
        yield Label("Are you sure?")
        yield Button("Yes", id="yes", variant="error")
        yield Button("No", id="no")

    @on(Button.Pressed, "#yes")
    def confirm(self) -> None:
        self.dismiss(True)

    @on(Button.Pressed, "#no")
    def cancel(self) -> None:
        self.dismiss(False)

# In your app — must run in a worker:
@work
async def action_delete(self) -> None:
    if await self.push_screen_wait(ConfirmDialog()):
        await self.do_delete()
```

`push_screen_wait` is the await-able way; results come back via `dismiss(value)`. It only works inside a worker — the `@work` decorator is mandatory, so waiting for the screen doesn't block the app. Calling it from a plain handler raises `NoActiveWorker`.

## Testing — Pilot + pytest-textual-snapshot

Pilot drives the app headlessly from pytest:

```python check=python/pilot-test
import pytest
from textual.widgets import Label

@pytest.mark.asyncio  # or asyncio_mode = "auto"
async def test_button_click():
    app = HelloApp()  # the example app above
    async with app.run_test(size=(80, 24)) as pilot:
        await pilot.click("#go")
        await pilot.pause()
        assert app.query_one("#greeting", Label).content == "Button pressed!"
```

For SVG snapshots:
```python
def test_homepage(snap_compare):
    assert snap_compare("path/to/app.py", press=["tab", "enter", "a"])
```

Snapshots stored under `__snapshots__/`. Failures generate an HTML diff page. This is genuinely production-grade testing — in our view still the most complete story of any TUI framework, though Ratatui's `TestBackend` and Bubble Tea's `teatest` have matured.

Gotchas worth knowing:

- **Async tests need `pytest-asyncio`.** Its default strict mode fails an unmarked `async def` test ("async def functions are not natively supported"). Set `asyncio_mode = "auto"` under `[tool.pytest.ini_options]`, as Textual's own suite does, or mark each test `@pytest.mark.asyncio`.
- **Pin `pytest<9` for snapshot suites.** pytest-textual-snapshot's last release, 1.1.0 (January 2025), pins `syrupy==4.8.0`, which requires `pytest<9`. On pytest 9, pip silently picks 1.0.0, which writes `.raw` snapshots instead of `.svg`. The fix PR has been open since November 2025.
- **`run_test()` runs headless at `size=(80, 24)` and disables notifications and tooltips by default.** A test asserting on a `notify()` toast will silently see nothing — pass `notifications=True` (and `tooltips=True` if relevant) to `run_test`.
- **There's no `App.messages` capture API.** Assert posted messages via a recorder handler on a test App subclass (an `@on(SomeWidget.Changed)` handler appending events to a list — Textual's own test suite does this), or via the `message_hook` parameter of `run_test`, which sees every message in the app. Messages bubble asynchronously, so always `await pilot.pause()` before asserting.
- **Form validation is testable end-to-end**: give `Input` `validators=[...]` and `validate_on`, drive it with the pilot, then assert on the recorded `Input.Submitted`/`Changed` message's `event.validation_result` (`is_valid`, `failure_descriptions`) — or on `input.is_valid` / the auto-applied `-invalid`/`-valid` CSS classes.

## Debugging

The `textual` CLI lives in the separate **`textual-dev`** package — `pip install textual-dev`; installing `textual` alone gives you no `textual` command.

Never `print()` inside a running Textual app — raw mode + the live display corrupt on any direct stdout write. Instead:

- **`textual console`** — a separate process that receives logs and `print()`/`self.log(...)` output. Run it in another terminal, then `textual run --dev myapp.py` in the app's terminal.
- **`textual run --dev`** — hot-reloads TCSS on save and routes logs to `textual console`.
- **`textual keys`** — interactive key inspector; press keys to see exactly what events Textual receives, useful for tracking down "why doesn't my binding fire" bugs.

**Profiling:** neither `textual run --dev`/`console` nor `textual diagnose` (from the `textual-dev` package) measure performance — `diagnose` is an environment report (Python/terminal/Textual versions), not a profiler, and there's no FPS or frame-timing overlay built in. For an actual "what's slow" answer, attach `py-spy` to the running process (`py-spy record -o profile.svg --pid <pid>`) — it needs no code changes, though attaching to a PID usually needs `sudo`; on Python 3.15+ the stdlib `python -m profiling.sampling attach <pid>` needs no install. One asyncio-specific gotcha: `py-spy` excludes idle/sleeping threads by default, and Textual's main loop spends most of its time awaiting the event loop, so a default run over a quiet window can misleadingly show almost nothing; pass `--idle` if you're not seeing the work you expect. Heavy synchronous work inside an `async def` handler with no `await` still blocks the whole UI exactly like it would in any event loop — offload it to `@work(thread=True)`, not just `@work`.

## Dev tools

- **`textual serve app.py`** — serves the app over HTTP/WebSocket; runs in a browser tab. Same Python code, no changes.

`textual serve` is genuinely unusual: the same codebase runs in a terminal, over SSH, or in a browser. Prefer it (self-hosted; works with Textual 8.2) over **textual-web**, the hosted service, which has had no release since 0.8.0 in August 2024.

**Accessibility:** `textual serve` is not an accessibility path. Its browser client paints the terminal grid with xterm.js on a canvas, with xterm's screen-reader mode off, so a screen reader gets no more than it would in a terminal. Textual itself has no screen-reader support; the roadmap item is still open. For reduced motion, `TEXTUAL_ANIMATIONS=none` (or setting `App.animation_level` to `"none"`) makes built-in animations jump to their end state and the loading indicator static; custom timers and spinners must check `app.animation_level` themselves. Design guidance: `visual-patterns.md` → *Accessibility*.

## Notable Textual apps

- **Toad** — Will McGugan's universal terminal UI for agentic coding, with ACP support for Claude Code, Gemini CLI, and others. The flagship Textual app.
- **Posting** — HTTP client (Postman alternative). The released 2.x line is Textual; `main` is now Posting 3, a Go rewrite.
- **Harlequin** — SQL IDE.
- **Memray** — Bloomberg's memory profiler.
- **Dolphie** — MySQL/MariaDB monitor.
- **Toolong** (log viewer), **elia** (LLM chat), **frogmouth** (markdown browser) — dormant and pinned to pre-1.0 Textual (0.43–0.79). Fine as design references; copy APIs from Toad, Harlequin or Dolphie, which track Textual 8.2.
- **Trogon** — generates a form UI from a Click CLI (unmaintained; see the CLI section).

## Pitfalls

1. **Reactives in `__init__` fire watchers before mount** → `NoMatches` errors when watchers try to query DOM. Init in `on_mount` or use `set_reactive(MyClass.attr, value)` to bypass watchers.
2. **`compose()` runs once.** To rebuild children, use `reactive(..., recompose=True)` or `await self.remove_children()` + `await self.mount(...)`.
3. **Don't `print()` inside Textual.** Corrupts the screen. Use `self.log(...)` and `textual console` to view.
4. **TCSS type selectors match subclasses** (unlike web CSS). `Button { ... }` matches your custom `MyButton(Button)`. Use class selectors (`.my-button`) when you want exact targeting.
5. **Inside Textual, use `ProgressBar` widget**, not Rich's `Live`/`Progress` — they fight each other for the screen.
6. **For periodic updates from sync code**, use `self.call_from_thread`, not direct attribute assignment.
7. **`can_focus = True`** on custom widgets that should receive key events. Without this, key events bypass them.
8. **`DataTable.add_rows()` is not a bulk-insert optimization.** It is a plain loop over `add_row()`, paying the same per-row cost, so switching to it won't speed up a slow bulk insert.

---

## Rich (Textualize/rich)

Immediate-mode output formatting — no input handling, no event loop. The de facto Python library for pretty CLI output. Used internally by Textual for rendering.

```python check=python/rich
from rich.console import Console
from rich.table import Table
from rich.panel import Panel
from rich.progress import track

console = Console()
err = Console(stderr=True)  # diagnostics and progress stay off stdout

# Markup syntax (like BBCode)
err.print("[bold red]Error:[/] file not found")

# Tables
table = Table(title="Users")
table.add_column("Name", style="cyan")
table.add_column("Email", style="green")
table.add_row("Alice", "alice@example.com")
console.print(table)

# Panels
console.print(Panel.fit("Hello, world!", title="Greeting", border_style="blue"))

# Progress
for item in track(items, description="Processing...", console=err):
    do_work(item)
```

**Components:** `Console`, markup (`[bold red]…[/]`), `Table`, `Panel`, `Columns`, `Tree`, `Syntax` (Pygments), `Markdown`, `Progress` (with multiple tasks), `Live` (animated regions), `RichHandler` for colorized logs, `install()` for pretty tracebacks.

**Diagnostics belong on stderr:** use a separate `Console(stderr=True)` for errors and progress so stdout stays pipeable. Rich honors `NO_COLOR`.

**`from rich.traceback import install; install()`** replaces the default Python traceback with a much better one — beautiful syntax highlighting and source context. Drop into any script for a free upgrade. (There is no `rich.install()`; the sibling `rich.pretty.install()` only pretty-prints REPL results.)

**Use Rich vs Textual:** Rich is for tools that *print and exit*. Textual is for apps the user *lives inside*. Rich + Click/Typer is the standard for modern Python CLI tools; pip vendors Rich for its own output, and Textual, Harlequin, and most new Typer-based tools build on it. (Poetry uses cleo and Sphinx does not depend on Rich; do not cite them as Rich users.)

---

## prompt_toolkit

Two modes:

**1. Prompt-style** — single-line input with completion, history, syntax highlighting:

```python check=python/prompt-toolkit
from prompt_toolkit import prompt
from prompt_toolkit.completion import WordCompleter

cmd_completer = WordCompleter(["help", "list", "quit"])
text = prompt("> ", completer=cmd_completer)
```

**2. Full-screen Application** — HSplit/VSplit/FloatContainer + UIControl + KeyBindings. Powers IPython, ptpython, all the DBCLI tools (mycli, pgcli, litecli), pyvim, pymux.

**When to choose prompt_toolkit:** you're building a REPL/shell where the central interaction is *typing commands*, with completion, history, multi-line editing, syntax highlighting. Textual can do this but prompt_toolkit is more focused.

For one-off prompts inside CLI tools, use **questionary** (built on prompt_toolkit) — much simpler API:

```python check=python/questionary
import sys
import questionary

answer = questionary.text("What's your name?").ask()
if answer is None:  # .ask() turns Ctrl-C into None
    sys.exit(130)
choice = questionary.select(
    "Pick a color",
    choices=["red", "green", "blue"],
).unsafe_ask()  # raises KeyboardInterrupt instead
```

`.ask()` catches Ctrl-C, prints "Cancelled by user" and returns `None`; check for `None` and exit 130, or call `.unsafe_ask()` to get the `KeyboardInterrupt`. prompt_toolkit draws prompts on stdout by default, even when stdout is piped, so when stdout carries the result pass `output=create_output(stdout=sys.stderr)` (from `prompt_toolkit.output`). For a single select without questionary, prompt_toolkit 3.0.52+ has a built-in `choice()`.

---

## Other libraries

- **Urwid** — pre-Textual TUI framework; mature and actively maintained again (3.0 in May 2025, 4.0 in March 2026, 4.2.x as of October 2026) after years of dormancy. New projects should still use Textual.
- **Blessed** — modernized curses wrapper, cross-platform via `jinxed`. For bespoke games/animations where you want fine control. Not recommended as a first choice.
- **curses** — stdlib, Unix-only, lowest level. Avoid for new code unless you have a strict zero-dep requirement.
- **InquirerPy** — Python port of Inquirer.js. Unmaintained (last release 0.3.4, June 2022); prefer questionary.
- **tqdm** — battle-tested progress bars, max performance, the default for ML/data scripts. `with logging_redirect_tqdm():` routes `logging` output around the bar; it does not capture `print()` — use `tqdm.write()` for those.
- **alive-progress** — animated, polished progress bars; better visual but slightly slower than tqdm.
- **asciimatics** — animations + form-style TUIs. Niche; Textual has taken over its mindshare.

---

## Argument parsing: argparse vs Click vs Typer

**argparse** — stdlib, no dependency. Verbose but capable. Since Python 3.14 it colorizes help and can suggest fixes for mistyped choices and subcommands (`suggest_on_error`, on by default in 3.15).

**Click** — decorator-based, very popular:
```python check=python/click
import click

@click.command()
@click.option("--name", default="world", help="Who to greet")
@click.option("--count", default=1, type=int)
def hello(name: str, count: int) -> None:
    """Say hello."""
    for _ in range(count):
        click.echo(f"Hello, {name}!")
```

**Typer** — type-hint-driven (FastAPI's author). The most ergonomic:
```python check=python/typer
import typer

app = typer.Typer()

@app.command()
def hello(name: str = "world", count: int = 1) -> None:
    """Say hello."""
    for _ in range(count):
        typer.echo(f"Hello, {name}!")

if __name__ == "__main__":
    app()
```

**Recommendation: use Typer for new projects.** Type hints give you free validation, documentation, and shell completion. Since 0.26 (May 2026) Typer vendors Click instead of depending on it, so Click plug-ins and extracting the underlying Click app are no longer supported; use Click directly if you need them. **Cyclopts** (Python 3.11+) is the Click-free alternative with a similar type-hint API.

**Trogon** (last release 0.6.0, October 2024; unmaintained) generates a Textual form UI for a **Click** CLI: `from trogon import tui`, then `@tui()` above your Click group. On Click 8.3+ it pre-fills options that have no default with the literal text `Sentinel.UNSET`, and its Typer integration fails on Typer 0.26+; pin `typer<0.26` or write that CLI in Click.

---

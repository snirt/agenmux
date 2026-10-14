# Go ecosystem — Bubble Tea, tview, gocui, Cobra

The Go TUI landscape consolidated around two camps: the **Charm stack** (Bubble Tea + Lipgloss + Bubbles + Huh) for new projects, and **tcell + tview** for traditional widget-rich apps. **gocui** is a third, legacy lineage behind lazygit (now an in-tree fork) and lazydocker. CLI framing comes from **Cobra** (kubectl, gh, hugo, helm, docker), `urfave/cli` or Kong.

**Contents:**
- [Quick recommendation](#quick-recommendation)
- [Bubble Tea](#bubble-tea-charmbraceletbubbletea) — [Lifecycle and terminal handoff](#lifecycle-and-terminal-handoff) · [Lipgloss](#lipgloss-charmbraceletlipgloss) · [Bubbles](#bubbles-charmbraceletbubbles) · [Huh](#huh-charmbracelethuh)
- [Other Charm libraries](#other-charm-libraries-worth-knowing)
- [tview](#tview-rivotview) · [gocui](#gocui-awesome-gocuigocui) · [tcell](#lower-level-tcell)
- [CLI framing: Cobra and urfave/cli](#cli-framing-cobra-and-urfavecli)
- [Output formatting](#output-formatting-non-tui)
- [Testing](#testing) · [Debugging](#debugging)
- [Notable Go TUI apps](#notable-go-tui-apps-to-study)
- [Cross-platform notes](#cross-platform-notes)

## Quick recommendation

| If the user wants… | Use |
|---|---|
| Modern TUI with clean architecture and Charm aesthetic | **Bubble Tea + Lipgloss + Bubbles** |
| Multi-pane vim-keybinding "lazy*"-style app | **Bubble Tea + Lipgloss** (compose the panes yourself); **gocui** (`awesome-gocui/gocui`) only for its exact views-as-buffers model |
| Heavy data exploration with tables, trees, forms | **tview** (callback-style, retained-mode) |
| One-shot fancy CLI prompts only | **Huh** (Go form library) or **gum** (shell wrapper) |
| Markdown rendering in terminal | **Glamour** |
| Pretty CLI output, no full-screen | **pterm** |
| Subcommand framing for any of the above | **Cobra** or **urfave/cli** |
| TUI served over SSH | **Wish** (wraps Bubble Tea apps as SSH server) |

**Default choice for new projects: Bubble Tea + Lipgloss + Bubbles + Cobra.** This is what `charm.land` apps and most new Go TUIs use. (`gh` is a Cobra CLI, not a full-screen TUI; it uses Lipgloss and Glamour for output and the Charm v2 stack — Bubble Tea, Bubbles, Huh — for interactive prompts.)

---

## Bubble Tea (charmbracelet/bubbletea)

The Elm Architecture in Go, and the most widely used Go TUI framework. Pure functional reactive — `Model → Update(Msg) → (Model, Cmd) → View() tea.View`.

**v2 is stable** (v2.0.0 shipped February 2026 after betas/RCs through 2025; current is v2.1.x, and Go 1.26+ is required since v2.0.10). v2.1.0 (October 2026) is additive: `View.ProgramStatus` reports idle/working/blocked/done/error to terminals that support OSC 7501 program-status reporting. What changed and why it matters:
- **New "Cursed Renderer"** — rewritten from scratch on the ncurses diffing algorithm; faster and more accurate redraws. Bubble Tea now owns terminal I/O and Lipgloss became pure (no more I/O fights between the two).
- **Progressive keyboard enhancements** — with the Kitty protocol you can finally bind `shift+enter`, `ctrl+i` distinct from `tab`, `super+space`, etc. Always keep a legacy fallback.
- **Import path moved** to `charm.land/bubbletea/v2` (vanity domain over `github.com/charmbracelet/bubbletea/v2`). All Charm v2 libraries import from `charm.land/<name>/v2` — bubbletea, bubbles, lipgloss, huh, wish, glamour, log, fang — keep them on the same major. Lower-level pieces (`colorprofile`, `ultraviolet`, `x/*`) stay on `github.com/charmbracelet/...` paths.

**Migrating v1 → v2:** the MVU shape survives, but every model gets touched: `View()` now returns `tea.View` instead of `string`, key handling moves to `tea.KeyPressMsg`, and the alt-screen/mouse program options moved onto the view. If you're on v1 and it works, there's no urgency; if you're starting fresh, start on v2.

**The mental model:**
- **Model**: a single struct holding all state.
- **Init**: returns the initial Cmd (or `nil`).
- **Update(msg)**: pure function returning `(newModel, Cmd)`. The only place state changes.
- **View()**: pure function returning the entire frame as a `tea.View` — content plus frame-level declarations (alt screen, mouse mode, cursor). Bubble Tea diffs against the previous frame and emits minimal ANSI.
- **Cmds and Msgs**: side effects live in `tea.Cmd` (a `func() tea.Msg`); messages flow back through Update.

**Canonical structure:**

```go check=go/counter
type model struct {
    count int
}

func (m model) Init() tea.Cmd { return nil }

func (m model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
    switch msg := msg.(type) {
    case tea.KeyPressMsg:  // KeyMsg is an interface in v2; msg.Code/msg.Text replace Type/Runes
        switch msg.String() {
        case "q", "ctrl+c":
            return m, tea.Quit
        case "+", "right", "l":
            m.count++
        case "-", "left", "h":
            m.count--
        }
    }
    return m, nil
}

func (m model) View() tea.View {
    v := tea.NewView(fmt.Sprintf("Count: %d\n\nq quit · ←/→ change", m.count))
    v.AltScreen = true  // don't pollute scrollback; required for full-screen TUIs
    return v
}

func main() {
    p := tea.NewProgram(model{})
    if _, err := p.Run(); err != nil {
        log.Fatal(err)
    }
}
```

`Program.Run()` owns Bubble Tea's normal terminal cleanup and default panic recovery. Do not add `defer p.RestoreTerminal()` as a final-cleanup guard: `RestoreTerminal` is the counterpart to `ReleaseTerminal` and resumes Bubble Tea's terminal modes rather than restoring the user's shell. Add an outer custom cleanup boundary only when deliberately bypassing or disabling the framework-managed path.

### Lifecycle and terminal handoff

Bubble Tea v2 owns the terminal for the whole `Program.Run()` call. Keep resumable work inside that lifecycle rather than tearing the program down and trying to reconstruct it.

| Boundary | Bubble Tea v2 contract |
|---|---|
| Normal exit | Return `tea.Quit`. `Program.Run()` performs final restoration on normal, error, and recovered-panic paths. |
| Interrupt or termination | Raw-mode Ctrl+C arrives as `tea.KeyPressMsg`; return `tea.Interrupt` when the caller should receive `tea.ErrInterrupted`, or `tea.Quit` for an intentional zero-status quit. Bubble Tea's default signal handler converts external SIGINT/SIGTERM into managed loop termination. Do not call `os.Exit` from `Update`. |
| Interactive child | Return `tea.ExecProcess(exec.Command(...), callback)`. It pauses the program, releases the terminal, attaches the child's standard streams, waits, restores and repaints, then reports the child error or, after child success, a restoration error through the callback message. If both fail, the callback exposes the child error. In the callback-message branch, reload data the child could have changed; repainting alone uses the old model. Use an ordinary `tea.Cmd` for non-interactive I/O. |
| Foreground suspend | Return `tea.Suspend`; after the Unix job resumes, handle `tea.ResumeMsg` to reload state that may have changed while away. Suspend is unsupported on Windows and should not be a required path there. |

The low-level `Program.ReleaseTerminal()` / `RestoreTerminal()` pair is a temporary handoff escape hatch, not final cleanup. Prefer [`ExecProcess`](https://github.com/charmbracelet/bubbletea/blob/v2.1.0/exec.go) for editors and shells; Bubble Tea's [tagged lifecycle source](https://github.com/charmbracelet/bubbletea/blob/v2.1.0/tea.go) defines signal, interrupt, suspend, and restoration semantics together.

**Frame-level declarations live on the view, not the program.** `tea.WithAltScreen()`, `tea.WithMouseCellMotion()`, and `tea.WithMouseAllMotion()` were removed in v2 — set `v.AltScreen = true` and `v.MouseMode = tea.MouseModeCellMotion` (or `tea.MouseModeAllMotion` for hover) in `View()`. Inline is the default, and because these fields are read every frame, an app can switch between inline and the alternate screen at runtime.

**Important options:**
- `tea.WithInput(reader)` / `tea.WithOutput(writer)` — useful for testing or Wish.
- `tea.WithColorProfile(p)` — force a specific color profile; the testing hook for golden-file output.

**Async via Cmds:**
```go check=go/fetch-cmd
func fetchData() tea.Msg {
    resp, err := http.Get(url)
    if err != nil { return errMsg{err} }
    defer resp.Body.Close()
    body, err := io.ReadAll(resp.Body) // *http.Response is not the data
    if err != nil { return errMsg{err} }
    return dataMsg{body}
}

// In Update, when you want to fetch:
return m, fetchData
```

`tea.Cmd` is `func() tea.Msg`. The runtime calls them in goroutines and routes the returned `Msg` back through `Update`. **Never block in `Update`** — anything I/O goes in a Cmd.

**`tea.Batch(cmds...)`** for parallel commands; **`tea.Sequence(cmds...)`** for ordered.

**Pitfalls:**
- `len(string)` ≠ display width. Use `lipgloss.Width()`.
- Goroutines never call `View()` directly — send via `program.Send(msg)`.
- CJK/emoji width in the v2 stack is computed by `charmbracelet/x/ansi` with grapheme clustering — trust `lipgloss.Width()`, never `len()`.
- Don't `fmt.Println` while the program runs — it corrupts the frame. For permanent lines above an inline UI, return `tea.Println(...)`/`tea.Printf(...)` (no-ops on the alt screen). For diagnostics, use `tea.LogToFile("debug.log", "DEBUG")` (see **Debugging** below).

---

## Lipgloss (charmbracelet/lipgloss)

CSS-in-Go declarative styling — immutable `Style` values rendered to ANSI strings. The companion to Bubble Tea, but usable standalone.

**Canonical use:**

```go check=go/lipgloss-style
var titleStyle = lipgloss.NewStyle().
    Bold(true).
    Foreground(lipgloss.Color("#FAFAFA")).
    Background(lipgloss.Color("#7D56F4")).
    Padding(0, 1).
    BorderStyle(lipgloss.RoundedBorder()).
    BorderForeground(lipgloss.Color("63"))

lipgloss.Println(titleStyle.Render("Hello, world")) // downsamples for the terminal; fmt.Println would not
```

**Layout helpers:**
- `lipgloss.JoinHorizontal(lipgloss.Top, left, right)` — side-by-side.
- `lipgloss.JoinVertical(lipgloss.Left, top, middle, bottom)` — stacked.
- `lipgloss.Place(width, height, hPos, vPos, content)` — center/anchor in a box.
- `lipgloss.Width(s)` / `lipgloss.Height(s)` — measure rendered output (skips ANSI escapes, counts wide graphemes correctly).

**No flexbox.** You compute widths from the cached `tea.WindowSizeMsg`:

```go check=go/window-size
case tea.WindowSizeMsg:
    m.width, m.height = msg.Width, msg.Height
    m.leftPaneWidth = msg.Width / 3
    m.rightPaneWidth = msg.Width - m.leftPaneWidth
```

**Light/dark theming:** Lipgloss v2 is pure — it never touches the terminal, so *your app* queries the background once and picks variants. Standalone (no Bubble Tea running):
```go check=go/light-dark
hasDark := lipgloss.HasDarkBackground(os.Stdin, os.Stdout)
lightDark := lipgloss.LightDark(hasDark)
fg := lightDark(lipgloss.Color("#236"), lipgloss.Color("#cef"))
```
Inside a running Bubble Tea program, don't call `HasDarkBackground` — its terminal query competes with Bubble Tea for input. Ask the runtime instead: return `tea.RequestBackgroundColor` from `Init`, then handle `case tea.BackgroundColorMsg:` with `lipgloss.LightDark(msg.IsDark())` and rebuild your styles. Always render a default palette first and treat the reply as an upgrade: it may never arrive (since v2.0.10, `tea.WithInput(nil)` skips all terminal queries, and some terminals don't answer OSC 11).
The old `AdaptiveColor` survives only in `charm.land/lipgloss/v2/compat` for migrations; prefer `LightDark` in new code.

**Sub-packages:**
- `lipgloss/table` — bordered, styled tables with column alignment.
- `lipgloss/list` — bullet/numbered/tree lists with custom enumerators.
- `lipgloss/tree` — hierarchical trees with custom indenters.

**Testing tip:** `SetColorProfile` is gone in v2. Downsampling now happens at write time — `lipgloss.Print`/`Println`/`Sprint` detect the output's profile via the `colorprofile` package. In Bubble Tea programs, force ASCII for CI/golden-file tests with `tea.WithColorProfile(colorprofile.ASCII)`.

---

## Bubbles (charmbracelet/bubbles)

The component library for Bubble Tea. Each Bubble is itself a `tea.Model` you embed and forward messages to.

**Components shipped:**
- **`textinput`** — single-line input with placeholder, suggestions, validation.
- **`textarea`** — multi-line input with optional line numbers; v2.2+ adds text selection, copy/cut and word motion.
- **`list`** — virtualized list with filtering, pagination, custom delegate for row rendering.
- **`table`** — virtualized table with row selection (no built-in sorting; sort the rows yourself).
- **`tree`** — navigable hierarchical tree, rendered with `lipgloss/tree`.
- **`viewport`** — scrollable region for long content.
- **`spinner`** — prebuilt `Line`, `Dot`/`MiniDot` (braille), `Jump`, `Pulse`, `Points`, `Globe`, `Moon`, `Monkey`, `Meter`, `Hamburger`, `Ellipsis`, or custom frames.
- **`progress`** — gradient-filled progress bar with percent.
- **`paginator`** — page indicators.
- **`filepicker`** — file system browser.
- **`help`** — auto-rendered footer hint bar from a `key.KeyMap`.
- **`key`** — declarative key bindings.
- **`cursor`**, **`stopwatch`**, **`timer`** — utilities.

**Embedding pattern:**

```go check=go/bubbles-embedding
type model struct {
    list list.Model
    keys keyMap
    help help.Model
}

func (m model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
    var cmd tea.Cmd
    m.list, cmd = m.list.Update(msg)  // forward to child
    return m, cmd
}

func (m model) View() tea.View {
    // Bubbles still return strings; the parent wraps them in a tea.View.
    return tea.NewView(m.list.View() + "\n" + m.help.View(m.keys))
}
```

**Declarative keys with `key.Binding`:**

```go check=go/key-bindings
type keyMap struct {
    Up   key.Binding
    Down key.Binding
    Quit key.Binding
}

var keys = keyMap{
    Up:   key.NewBinding(key.WithKeys("up", "k"), key.WithHelp("↑/k", "up")),
    Down: key.NewBinding(key.WithKeys("down", "j"), key.WithHelp("↓/j", "down")),
    Quit: key.NewBinding(key.WithKeys("q", "ctrl+c"), key.WithHelp("q", "quit")),
}

// In Update (tea.KeyMsg is an interface in v2 that also matches releases; switch on presses):
case tea.KeyPressMsg:
    switch {
    case key.Matches(msg, keys.Quit):
        return m, tea.Quit
    case key.Matches(msg, keys.Up):
        // ...
    }
```

The `help` Bubble auto-renders the footer from these bindings — define once, get the hint bar for free. `help.View(m.keys)` needs `keyMap` to implement `help.KeyMap`:

```go check=go/help-keymap
func (k keyMap) ShortHelp() []key.Binding  { return []key.Binding{k.Up, k.Down, k.Quit} }
func (k keyMap) FullHelp() [][]key.Binding { return [][]key.Binding{{k.Up, k.Down}, {k.Quit}} }
```

---

## Huh (charmbracelet/huh)

Forms library on top of Bubble Tea. Best for one-shot interactive prompts (multi-step wizards) and embeddable form panes inside larger TUIs. Huh v2 (`charm.land/huh/v2`, released March 2026) is built on Bubble Tea v2 — built-in themes are now `func(isDark bool) *Styles`; wrap one as a `huh.Theme` with `form.WithTheme(huh.ThemeFunc(huh.ThemeCharm))`. Huh supplies `isDark` itself from the `tea.BackgroundColorMsg` it receives, but v2.0.3 never requests one, so an embedding parent must return `tea.RequestBackgroundColor` from its own `Init` and forward the reply to the form. A standalone `form.Run()` has no parent to ask, so it always renders the light variant ([huh#834](https://github.com/charmbracelet/huh/issues/834)). Detect the background before running it (`lipgloss.HasDarkBackground(os.Stdin, os.Stdout)` is safe here because no Bubble Tea program is running yet) and pin the variant with `huh.ThemeFunc(func(bool) *huh.Styles { return huh.ThemeCharm(isDark) })`. Huh's own upgrade guide shows `.WithTheme(huh.ThemeCharm(isDark))`, which does not compile against v2.0.3, because `*Styles` is not a `Theme`.

```go check=go/huh-form
form := huh.NewForm(
    huh.NewGroup(
        huh.NewSelect[string]().
            Title("Choose your country").
            Options(huh.NewOptions("USA", "Germany", "Japan")...).
            Value(&country),
        huh.NewInput().
            Title("What's your name?").
            Value(&name).
            Validate(func(s string) error {
                if len(s) == 0 { return errors.New("required") }
                return nil
            }),
        huh.NewConfirm().
            Title("Submit?").
            Value(&confirm),
    ),
)
err := form.Run()
```

Field types: `NewInput`, `NewText` (multi-line), `NewSelect[T]`, `NewMultiSelect[T]`, `NewConfirm`, `NewFilePicker`, `NewNote` (display-only).

**Dynamic forms:** huh's real mechanism is **`Group.WithHideFunc(func() bool)`** — it hides/shows an entire group (a step's worth of fields) based on a predicate re-evaluated as form state changes. This operates at *group* granularity; per-field hiding is not shipped (open feature requests only) — don't present it as available. For recomputing one field from another's value, huh has `OptionsFunc`/`TitleFunc`/`DescriptionFunc`, which take a callback plus the fields they depend on and re-run automatically:

```go check=go/huh-options-func
huh.NewSelect[string]().
    OptionsFunc(func() []huh.Option[string] {
        return huh.NewOptions(statesForCountry(country)...)
    }, &country) // recomputed whenever `country` changes
```

**Accessibility:** `form.WithAccessible(true)` bypasses Bubble Tea and asks each field as a plain line-by-line prompt: a numbered list for `Select` (the user types the number), numbers toggled one at a time and `0` to finish for `MultiSelect`, `y`/`n` for `Confirm`, and validation errors printed before the prompt repeats. Huh turns it on by itself only for `TERM=dumb`, so expose your own switch. The README reads an `ACCESSIBLE` environment variable, and gh's `GH_ACCESSIBLE_PROMPTER` drives exactly this mode. Bubble Tea and Bubbles have nothing comparable, so the rest of the app needs its own linear mode; see `visual-patterns.md` → *Accessibility*.

**Full-screen:** `tea.WithAltScreen()` is gone in v2 — declare it on the view instead: `form.WithViewHook(func(v tea.View) tea.View { v.AltScreen = true; return v })`.

**Embedding in Bubble Tea:** `(*huh.Form).Update` returns `(huh.Model, tea.Cmd)`, so assert back: `f, cmd := m.form.Update(msg); if ff, ok := f.(*huh.Form); ok { m.form = ff }`. `Form.View()` returns a string that the parent wraps in its own `tea.NewView`; check `m.form.State` (`huh.StateCompleted`/`huh.StateAborted`) to leave the form.

---

## Other Charm libraries worth knowing

- **Glamour** (`charm.land/glamour/v2`) — render Markdown to ANSI. Themable via JSON stylesheets. v2 is pure: `WithAutoStyle` and `WithColorProfile` are gone, the default style is `"dark"` (pass `"light"` yourself after detecting the background), and output is downsampled only when printed via `lipgloss.Print`. Used by the `glow` markdown viewer and for `gh` PR/issue rendering (`gh` is still on Glamour v1).
- **Wish** (`charmbracelet/wish`, v2 at `charm.land/wish/v2`) — serve Bubble Tea apps over SSH with HTTP-style middleware. Build each program with `bubbletea.MakeOptions(sess)` (or the `bubbletea.Middleware` handler, which applies it for you) so input, output, and color-profile detection follow the *client's* PTY, not the server's. The v1-era per-session `MakeRenderer` helper is gone in v2 because Lipgloss v2 no longer owns terminal I/O. `MakeOptions` also installs its own `tea.WithFilter` (ctrl+z suspend becomes resume), and the default `Middleware` appends it after your options, overwriting any filter you set; to keep yours, use `MiddlewareWithProgramHandler` and chain them.
- **Soft Serve** — git server with a Bubble Tea TUI; built on Wish.
- **gum** — shell-script wrapper around Bubble Tea components. `gum choose`, `gum input`, `gum confirm`, `gum spin`. Use when scripting Bash and you want huh-quality prompts without writing Go.
- **VHS** — record terminal sessions to GIFs from a `.tape` script. Gold for documentation.

---

## tview (rivo/tview)

Retained-mode, callback-based — the traditional ncurses style. Built on **tcell v2** (gdamore's terminal library, replacement for termbox). Mature and maintained slowly but steadily; tags are rare (last v0.42.0, August 2025) and the README installs with `go get github.com/rivo/tview@master`. Powers **k9s** (via derailed's `tview`/`tcell` forks).

**Widgets:** `Box`, `TextView`, `TextArea`, `InputField`, `Table`, `TreeView`, `List`, `Form`, `Image`, `Modal`, `Pages`, `Flex`, `Grid`, `Frame`, `DropDown`, `Button`, `Checkbox`.

**Layout:** `Flex` (one-dimensional), `Grid` (two-dimensional), `Pages` (modal/swap views).

**Hello world:**

```go check=go/tview-hello
app := tview.NewApplication()
list := tview.NewList().
    AddItem("First", "First item", 'a', nil).
    AddItem("Quit", "Quit app", 'q', func() { app.Stop() })
if err := app.SetRoot(list, true).Run(); err != nil {
    panic(err)
}
```

**Threading rule:** any modification to a primitive from a goroutine must go through `app.QueueUpdate(func)` or `app.QueueUpdateDraw(func)`, which run `func` on the main loop. A bare `app.Draw()` from a goroutine is safe (it only schedules a repaint) but does not synchronize your data. The deadlock trap is the opposite direction: calling `Draw()`, `QueueUpdate()`, or `QueueUpdateDraw()` from *inside* an event handler that already runs on the main goroutine (a key handler, `SetSelectedFunc`, and so on) blocks forever, per the tview Concurrency wiki. This is the #1 tview pitfall.

**When to choose tview over Bubble Tea:** you have many widgets that need to coexist (a real "form with 12 fields, table, sidebar"), the team prefers callbacks over message-passing, or you're already on tcell. Bubble Tea is more ergonomic for state-heavy apps but you assemble layout manually; tview gives you a richer widget set with less code.

---

## gocui (awesome-gocui/gocui)

Minimalist views-as-buffers — each `View` implements `io.Writer`. The aesthetic of **lazygit** and **lazydocker**.

**Mental model:**
- Define a `Manager` function that's called on every redraw to lay out views.
- Each view has a name, position, and a buffer you write to.
- Per-view keybindings.
- Manual layout (you compute coordinates from `g.Size()`).

**Hello world:**

```go check=go/gocui-hello
g, _ := gocui.NewGui(gocui.OutputNormal, true)
defer g.Close()

g.SetManagerFunc(func(g *gocui.Gui) error {
    maxX, maxY := g.Size()
    if v, err := g.SetView("hello", maxX/2-7, maxY/2, maxX/2+7, maxY/2+2, 0); err != nil {
        if !errors.Is(err, gocui.ErrUnknownView) { return err }
        fmt.Fprintln(v, "Hello, World!")
    }
    return nil
})

g.SetKeybinding("", 'q', gocui.ModNone, func(g *gocui.Gui, v *gocui.View) error {
    return gocui.ErrQuit
})

g.MainLoop()
```

**When to choose gocui:** rarely, for maintaining an existing gocui app or when you want its views-as-buffers API specifically. Use `awesome-gocui/gocui` (last tag v1.1.0, 2022; still on tcell v2). `jesseduffield/gocui` is effectively retired: in April 2026 lazygit copied its fork into `lazygit/pkg/gocui` and moved to tcell v3. For a new lazy-style app (persistent panes, numeric keys to switch panes, single-letter context actions), use Bubble Tea and borrow lazygit's interaction design rather than its library.

---

## Lower-level: tcell

`gdamore/tcell` is the long-standing low-level Go terminal library: pure Go, cross-platform, mouse and SGR support. v3 (December 2025) removed terminfo and the termbox compatibility layer and changed the key and cell APIs; tview still uses `tcell/v2`, while lazygit has moved to v3. Use directly only if you're building a framework or have very specific needs (custom rendering pipeline, embedded use). Most apps should use Bubble Tea or tview on top.

**Ultraviolet** (`charmbracelet/ultraviolet`) is the standalone rendering engine underneath Bubble Tea v2's renderer — the same tier as tcell if you're building your own framework. It is untagged, and its README warns the API may change.

---

## CLI framing: Cobra and urfave/cli

**Cobra** (`spf13/cobra`) — the dominant subcommand framework for Go. Powers kubectl, gh, hugo, helm, docker. Struct-literal command tree:

```go check=go/cobra-commands
var rootCmd = &cobra.Command{
    Use:   "myapp",
    Short: "A short description",
}

var listCmd = &cobra.Command{
    Use:   "list",
    Short: "List items",
    Run:   func(cmd *cobra.Command, args []string) { /* ... */ },
}

func init() {
    rootCmd.AddCommand(listCmd)
}
```

**Cobra + Bubble Tea integration**: the Cobra `Run` function does `tea.NewProgram(...).Run()`. Output from `PreRun` is hidden behind the alt screen while the TUI runs and reappears at exit; what corrupts the frame is writing to stdout while `Run` is active. Gate the TUI on **stdout** being a terminal (`isatty.IsTerminal(os.Stdout.Fd())`): under `$(myapp)` or `myapp | less` the frame would become data, so print plain output instead, or draw the UI on stderr with `tea.WithOutput(os.Stderr)` and print only the answer to stdout, as gum does. Piped stdin doesn't stop the TUI (Bubble Tea v2 opens `/dev/tty` for keys); decide separately whether piped input means non-interactive mode.

Cobra supports generated shell completions and, since v1.2, adds a working `completion` subcommand to every root command automatically (hidden when there are no other subcommands). Do not scaffold your own `completion` command; it would shadow the built-in. Users generate a script with `myapp completion bash|zsh|fish|powershell`; disable or customize the default via `rootCmd.CompletionOptions`. Note that `cobra-cli completion ...` completes the generator tool itself, not your application.

**Fang** (`charm.land/fang/v2`) — a small, self-described experimental starter kit that wraps Cobra: styled help and error output, automatic `--version`, manpage generation. The idiomatic Cobra companion in a Charm-stack app.

**urfave/cli** (`github.com/urfave/cli/v3`; v3 has been the stable line since 2025, v2 is maintenance-only) — alternative subcommand framework, simpler API, less popular but used by Gitea and others. **Kong** (`alecthomas/kong`, struct-tag driven, used by Syncthing) is the third common choice.

For one-shot `flag` parsing only, the stdlib `flag` package is fine.

---

## Output formatting (non-TUI)

When the user wants pretty CLI output (no full-screen UI):

- **lipgloss** — yes, you can use it standalone: `lipgloss.Println(style.Render(...))`, not `fmt.Println`. In v2, `Render` always emits truecolor; only `lipgloss.Print*`/`Sprint*`/`Fprint*` downsample for the real output and honor `NO_COLOR`. This is what most Charm CLI tools do.
- **pterm** — color, tables, spinners, progress bars, charts, prompts. All-in-one. Less elegant API than Lipgloss but has more built-in.
- **fatih/color** — simple ANSI color wrapper. Used by countless tools. `color.Red("hello")`, `color.New(color.FgYellow, color.Bold).Println(...)`.

---

## Testing

**Test in layers, bottom-heavy.** Even Charm's flagship v2 app (crush, over 200 test files) uses zero teatest — it unit-tests handlers with `tea.KeyPressMsg` literals and golden-tests render output via `github.com/charmbracelet/x/exp/golden`. Unit tests on `Update` are the base of the pyramid; the harness is the thin top.

**Layer 1 — `Update` is a pure function.** Construct the model, send a message, assert on state. No harness needed:

```go check=go/update-test
func TestQuitKey(t *testing.T) {
    m := newModel()
    updated, cmd := m.Update(tea.KeyPressMsg{Code: 'q', Text: "q"}) // v2 rune key
    m = updated.(model)
    if cmd == nil { t.Fatal("expected quit cmd") }
}
```

v2 key literals: rune keys are `tea.KeyPressMsg{Code: 'a', Text: "a"}`, specials are `tea.KeyPressMsg{Code: tea.KeyEnter}`, modifiers use `Mod: tea.ModCtrl`. To test a returned Cmd, just call it — `tea.Cmd` is `func() Msg` — and assert on the message (`tea.QuitMsg` for quit; `tea.BatchMsg` is an exported `[]Cmd` you can unpack and run).

**Layer 2 — teatest** runs the real program against in-memory buffers (no PTY, works in bare CI containers). For Bubble Tea v2 import `github.com/charmbracelet/x/exp/teatest/v2` — teatest did **not** move to charm.land; plain `x/exp/teatest` is the v1 line. Both are pseudo-versioned experiments, but teatest/v2 is what maintainers point to as the official harness.

```go check=go/teatest
tm := teatest.NewTestModel(t, newModel(),
    teatest.WithInitialTermSize(80, 24), // default is 80×24 — pin it anyway
    teatest.WithProgramOptions(tea.WithColorProfile(colorprofile.ASCII)), // v2 only
)
teatest.WaitFor(t, tm.Output(), func(b []byte) bool {
    return bytes.Contains(b, []byte("ready"))
}, teatest.WithDuration(5*time.Second)) // defaults (1s timeout, 50ms poll) are tight for CI
tm.Type("hello")                        // v2: emits one KeyPressMsg per rune
tm.Send(tea.KeyPressMsg{Code: tea.KeyEnter})
tm.WaitFinished(t, teatest.WithFinalTimeout(2*time.Second))
out, _ := io.ReadAll(tm.FinalOutput(t))
teatest.RequireEqualOutput(t, out) // golden file: testdata/<TestName>.golden
```

Always pass `WithFinalTimeout` — `FinalModel`/`FinalOutput`/`WaitFinished` block forever without it. Goldens hold the raw bytes, escape sequences included: `golden.RequireEqual` escapes both sides only when comparing, so failure diffs stay readable. Refresh with `go test ./... -update`, and add `*.golden -text` to `.gitattributes` so git never rewrites them.

**Test at the contract sizes:** golden-test 80×24, 60 columns, and your declared minimum (one golden per size, e.g. `TestView/60x24`). Exercise resize too — send `tea.WindowSizeMsg{Width: 60, Height: 24}` to `Update` in unit tests, or via `tm.Send(...)` mid-run in teatest — and assert the layout reflows rather than keeping the startup width.

**Determinism in CI:** the #1 golden-file flake is color-profile detection — v2 wraps output in a `colorprofile.Writer`, and the emitted escapes vary with `TERM`/`COLORTERM`/`NO_COLOR`. Pin the profile (`tea.WithColorProfile(colorprofile.ASCII)` — v2-only; on v1 use `lipgloss.SetColorProfile(termenv.Ascii)`) and the term size, or goldens will flap. Bubble Tea's own teatest example is currently skipped over exactly this.

**E2E:** PTY/expect practice is thin in Go — teatest deliberately avoids PTYs. For real-terminal visual regression, VHS golden output (`Output golden.ascii` in a tape) is the blessed heavyweight option; needs `ttyd` + `ffmpeg`, with `charmbracelet/vhs-action` for CI.

---

## Debugging

**stdout is off-limits while the program runs** — Bubble Tea holds the terminal in raw mode on the alternate screen, so `fmt.Println` output gets swallowed or corrupts the frame. Log to a file instead (unchanged in v2):

```go check=go/log-to-file
if len(os.Getenv("DEBUG")) > 0 {
    f, err := tea.LogToFile("debug.log", "debug")
    if err != nil {
        fmt.Fprintln(os.Stderr, "fatal:", err) // otherwise logs land in the TUI
        os.Exit(1)
    }
    defer f.Close()
}
```

Run `DEBUG=1 go run .` in one terminal and `tail -f debug.log` in another — the documented convention. `log.Println` from anywhere in Update or Cmds lands in the file; `tea.LogToFileWith` (present in both v1 and v2) takes a custom logger.

**Breakpoints:** the TUI and delve fight over stdin/stdout, so run delve headless — `dlv debug --headless --api-version=2 --listen=127.0.0.1:43000 .`, then `dlv connect 127.0.0.1:43000` from a second terminal.

**Profiling a sluggish TUI:** Bubble Tea's README documents no profiling recipe of its own, so this is standard Go tooling — but the shape that actually works for a TUI is `net/http/pprof` on a loopback port, not file-based `pprof.StartCPUProfile`. A background goroutine serving on `localhost:6060` never touches stdin/stdout, so it can't collide with raw-mode rendering, and there's no SIGINT/flush ceremony to get right. This is what Charm's own `crush` does, gated behind an env var:

```go check=go/pprof
import (
    "net/http"
    _ "net/http/pprof"
)
if os.Getenv("MYAPP_PROFILE") != "" {
    go func() { http.ListenAndServe("localhost:6060", nil) }()
}
```

Then from a second terminal: `go tool pprof -http=:8080 http://localhost:6060/debug/pprof/profile`. Common cost sink to look for: work smuggled into `View()` that should be cached (recomputed `lipgloss.Width()` calls, string rebuilds) or an unbatched Bubbles `list`/`table` render — [bubbles#810](https://github.com/charmbracelet/bubbles/issues/810) is a real case of exactly this (per-frame string concatenation instead of `strings.Builder`), diagnosed with this same `net/http/pprof` technique.

---

## Notable Go TUI apps to study

- **lazygit** (in-tree gocui fork on tcell v3) — multi-pane git TUI; the canonical "lazy*" aesthetic.
- **lazydocker** (gocui) — same paradigm for Docker.
- **k9s** (derailed's `tview`/`tcell` forks) — Kubernetes TUI; command-mode navigation, drill-down stack.
- **gh** (Cobra + Lipgloss, Huh for prompts) — GitHub CLI; not a full TUI, but excellent CLI design.
- **Crush** (Bubble Tea v2) — Charm's AI coding agent; the flagship production v2 app.
- **glow** (Bubble Tea v2 stack since glow v3) — markdown reader.
- **soft serve** (Wish + Bubble Tea) — SSH-served git UI.
- **circumflex** — Hacker News reader (Bubble Tea).
- **gh dash** (Bubble Tea) — GitHub dashboard.
- **superfile** (Bubble Tea) — file manager.
- **wishlist** (Wish v1 / Bubble Tea v1; study the patterns, not the imports) — SSH directory.

When the user is building something similar to one of these, point them at it as a reference. The repos are open and idiomatic.

---

## Cross-platform notes

Bubble Tea, tview, and gocui all work on Linux, macOS, and Windows (Terminal, conhost, ConEmu). Windows quirks:
- Older `cmd.exe` doesn't support all ANSI; target Windows Terminal (Windows 10+).
- `Ctrl+Z` (SIGTSTP) doesn't exist on Windows; that's fine, just don't rely on suspend behavior universally.
- Mouse events differ slightly; libraries abstract this but test on Windows if it's a target.
- Environment differences (signals, `WT_SESSION`, Unicode output, multiplexer keys): `interaction-patterns.md` → *Terminal environments*.

For SSH-served TUIs, **Wish** is the right answer — it handles per-session terminal capabilities (the client's truecolor support, not the server's) and gives you middleware for auth, logging, rate limiting.

---

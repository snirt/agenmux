# TypeScript / JavaScript ecosystem — Ink, Clack, Inquirer

The Node.js ecosystem splits along two axes:
- **Pretty CLI** (argument parsing + color + prompts/spinners) vs **full TUI**.
- **Imperative/retained-mode** vs **declarative/React-style**.

**Ink has effectively won the high-end TUI space.** It powers Claude Code, GitHub Copilot CLI, Gemini CLI, Cloudflare Wrangler, Gatsby CLI, Prisma, Shopify CLI, Linear's internal CLI, Canva CLI, and tap. Anthropic's Claude Code is publicly TypeScript + React + Ink + Yoga + Bun.

**Contents:**
- [Quick recommendation](#quick-recommendation)
- [Ink](#ink-vadimdemedesink) — [Lifecycle and terminal handoff](#lifecycle-and-terminal-handoff) · [Primitives](#primitives) · [Layout](#layout) · [Hooks](#hooks) · [ink-ui](#ink-ui-vadimdemedesink-ui)
- [Testing](#testing--ink-testing-library) · [Debugging](#debugging)
- [Pastel](#pastel--nextjs-style-filesystem-routing) · [Strengths and weaknesses](#strengths-and-weaknesses) · [Pitfalls](#pitfalls)
- [Modern prompts: @clack/prompts](#modern-prompts-clackprompts) · [@inquirer/prompts](#inquirerprompts)
- [Color libraries](#color-libraries) · [Other utilities](#other-utilities) · [Argument parsers](#argument-parsers)
- [OpenTUI](#opentui-anomalycoopentui) · [blessed / neo-blessed / terminal-kit](#blessed--neo-blessed--terminal-kit)
- [Notable JS/TS TUI apps](#notable-jsts-tui-apps-to-study)
- [Pitfalls common to JS/TS](#pitfalls-common-to-jsts-terminal-apps)
- [Stack recommendations by project shape](#stack-recommendations-by-project-shape)

## Quick recommendation

| If the user wants… | Use |
|---|---|
| Full-screen TUI app | **Ink** (React-flexbox) |
| Modern Clack-style wizard prompts | **@clack/prompts** |
| Prompt-heavy CLI (many questions) | **@inquirer/prompts** (the modern modular rewrite) |
| Single elegant spinner | **ora** |
| Hierarchical task runner | **listr2** |
| Argument parsing, simple | **commander** (de facto default) |
| Argument parsing, validation-heavy | **yargs** |
| Heroku-class plugin CLI | **oclif** (Salesforce's framework) |
| Argument parsing, TS-first, lightweight | **citty** (UnJS) |
| Tiny argument parser | **cac** (one ~20 KB file, no deps) or `node:util.parseArgs` |
| Terminal colors, performance-critical | **picocolors** |
| Terminal colors, classic API | **chalk** |
| Single boxed message | **boxen** |
| Beyond Ink's perf ceiling | **OpenTUI** (Zig-backed, v0.x) |
| Image rendering in terminal | **terminal-image** 5 (Kitty/iTerm2 protocols, half-block fallback) |

**Default full-TUI stack: Ink + @inkjs/ui + zustand + commander + ink-testing-library** (@inkjs/ui and ink-testing-library have not shipped since May 2024 but still work with Ink 8 — see their caveats). For other project shapes, see "Stack recommendations by project shape" at the end.

---

## Ink (vadimdemedes/ink)

**Architectural model: React for the terminal.** A custom `react-reconciler` renderer that commits to terminal-native host nodes (`ink-root`, `ink-box`, `ink-text`), runs Yoga layout, paints into a screen buffer, diffs against the previous frame, and emits ANSI patches in a single buffered terminal write.

Current major: **Ink 8** (8.0.x, October 2026). Ink 7 and 8 require **Node ≥ 22**; Ink 8 also requires **React ≥ 19.3** (Ink 7: ≥ 19.2). Node 20 reached end-of-life on 2026-04-30, so target Node 22 (maintenance LTS until 2027-04-30) or Node 24 (LTS); Node 26 becomes LTS on 2026-10-28. Ink 6 (Node ≥ 20, React ≥ 19), commander 14, and chalk 5 are the last majors that run on Node 20 — use them only as a bridge for a deployment that can't move yet.

**Hello world:**

```tsx check=typescript/ink-hello
import React, {useState} from 'react';
import {render, Text, Box, useInput, useApp} from 'ink';

const App = () => {
  const [n, setN] = useState(0);
  const {exit} = useApp();

  useInput((input, key) => {
    if (key.upArrow) setN(c => c + 1);
    if (key.downArrow) setN(c => c - 1);
    if (input === 'q') exit();
  });

  return (
    <Box borderStyle="round" padding={1}>
      <Text color="green">Counter: {n}</Text>
    </Box>
  );
};

render(<App />);
```

### Lifecycle and terminal handoff

Ink 7.1+ (unchanged in 8.0) distinguishes permanent React-tree unmounting from a resumable terminal suspension (`suspendTerminal` is not in 7.0 or Ink 6). Keep process-level signal policy at the render boundary and temporary ownership changes inside `useApp()`.

| Boundary | Ink 7.1+ contract |
|---|---|
| Normal exit | Use `useApp().exit(...)` inside the tree or the render handle's `unmount()` outside it. At the top level, await `waitUntilExit()` before post-run output or process termination so pending terminal writes finish. |
| SIGINT / SIGTERM | `exitOnCtrlC` handles Ctrl+C bytes while stdin is raw; it is not an operating-system SIGTERM handler. If the host requires graceful signals, register them once at the process boundary, unmount, await `waitUntilExit()`, set the intended exit status, and remove listeners. Do not call `process.exit()` before cleanup flushes. |
| Interactive child | Use `await suspendTerminal(async () => runChild())`. Ink pauses input and rendering, restores cursor/input/screen modes, runs the callback even without an interactive TTY, and re-enters with a full redraw in `finally`. Reload child-mutated data afterward because redraw preserves the existing React state. For the manual form, always `await suspension.resume()` in `finally` or use `await using`. Do not unmount. |
| Foreground suspend | Ink has no separate high-level job-control action. If a Unix-only Ctrl+Z feature is necessary, perform SIGTSTP inside `suspendTerminal` and let its resume path re-enter and redraw; expose a different action on Windows. |

The tagged [`suspendTerminal` documentation](https://github.com/vadimdemedes/ink/blob/v8.0.0/readme.md#suspendterminalcallback) includes callback, manual, non-interactive, and nesting behavior. A second overlapping suspension is an error, so centralize ownership rather than letting arbitrary components suspend independently.

## Primitives

Ink ships only a handful of components — everything else composes from these:

- **`<Text>`** — only place strings can live. Props: `color`, `backgroundColor`, `bold`, `italic`, `underline`, `inverse`, `dimColor`, `wrap`, `strikethrough`. Strings as direct children of `<Box>` will throw — they must be wrapped.
- **`<Box>`** — flexbox container. Props: `padding`, `paddingX/Y/Top/Right/Bottom/Left`, `margin*`, `borderStyle`, `borderColor`, `borderTop`/`Bottom`/`Left`/`Right` (selective borders), `width`, `height`, `minWidth`/`maxWidth` (numbers only in Ink 8), `flexDirection`, `flexGrow`, `flexShrink`, `flexBasis`, `justifyContent`, `alignItems`, `gap`, `display: 'flex' | 'none'`, `overflow`/`overflowX`/`overflowY`. Ink 8 adds `contentOffsetX`/`contentOffsetY` for scrolling: give a fixed-height viewport `overflow="hidden"`, wrap the content in a `flexShrink={0}` box that carries any padding, and clamp the offset to `content.height - clientHeight` from `useBoxMetrics()`.
- **`<Newline>`** — vertical spacing.
- **`<Spacer>`** — flex-grow filler in a flex layout.
- **`<Static>`** — renders items permanently above the live UI; once rendered, never re-renders. Suited to log streaming — completed tests (tap) or generated pages (Gatsby) — so the live UI doesn't have to repaint thousands of historical log lines.
- **`<Transform>`** — wraps children and transforms their rendered string. Used for gradients, OSC 8 hyperlinks, custom effects.

## Layout

Yoga (Meta's open-source flexbox engine, same as React Native). No CSS, no className — props on `<Text>` and `<Box>`. Border styles come from `cli-boxes`: `single`, `double`, `round`, `bold`, `singleDouble`, `doubleSingle`, `classic`, plus custom char strings.

```tsx check=typescript/ink-layout
<Box flexDirection="column" height="100%">
  <Box borderStyle="single">
    <Text>Header</Text>
  </Box>
  <Box flexGrow={1} flexDirection="row">
    <Box width={30} borderStyle="single">
      <Text>Sidebar</Text>
    </Box>
    <Box flexGrow={1} borderStyle="single">
      <Text>Main content</Text>
    </Box>
  </Box>
</Box>
```

## Hooks

Ink-specific (items marked 7+ are not in Ink 6):
- **`useInput((input, key) => ...)`** — keyboard events. `key` is `{upArrow, downArrow, leftArrow, rightArrow, return, escape, tab, backspace, delete, ctrl, shift, meta, pageUp, pageDown, home, end}` (plus Kitty-protocol extras). Ink 8 drops mouse, focus, and cursor-position reports instead of passing them in as typed text, so mouse support must parse raw stdin.
- **`useApp()`** — `{exit(errorOrResult?), waitUntilRenderFlush (7+), suspendTerminal (7.1+)}`. Pass an `Error` to `exit` to reject the render handle's `waitUntilExit()`; there is no separate error variant. Use `suspendTerminal` for a resumable interactive child, not final shutdown.
- **`useStdin()`** — `{stdin, setRawMode, isRawModeSupported}`.
- **`useStdout()`** / **`useStderr()`** — write outside the live UI. Ink 8 types these streams as generic Node streams, so `stdout.columns` no longer type-checks; read the size from `useWindowSize()`.
- **`useFocus({autoFocus, isActive, id})`** — Tab-focusable component.
- **`useFocusManager()`** — `{focus(id), focusNext, focusPrevious, enableFocus, disableFocus, activeId (7+)}`.
- **`useWindowSize()`** (7+) — `{columns, rows}`, updates on resize.
- **`usePaste(callback)`** (7+) — bracketed-paste events, so pasted text arrives as one chunk instead of spraying through `useInput` character by character.
- **`useCursor()`** — position the real terminal cursor (IME-friendly input).
- **`useAnimation()`** (7+) — frame ticker with pause/resume.
- **`useBoxMetrics()`** (7+) — measured dimensions of a box at runtime; Ink 8 adds `clientWidth`/`clientHeight`, the size inside its borders.

Plus all standard React hooks (`useState`, `useEffect`, `useReducer`, `useContext`, `useMemo`).

State management for larger apps: **Zustand** (recommended), **Jotai**, or just `useReducer` + Context.

## ink-ui (vadimdemedes/ink-ui)

The companion component library — themeable widgets you don't want to write yourself:

- `<TextInput>`, `<EmailInput>`, `<PasswordInput>` — uncontrolled inputs: `defaultValue`, `onChange`, `onSubmit`, and no `value` prop (issue #20), so remount with a new `key` to clear one.
- `<Select>`, `<MultiSelect>` — choose from options.
- `<ConfirmInput>` — y/n prompt.
- `<Spinner>` — animated spinner (use this inside Ink, not `ora`).
- `<ProgressBar>` — determinate progress.
- `<Badge>` — colored status pill.
- `<StatusMessage>` — `success`/`info`/`warning`/`error` variants.
- `<Alert>` — bordered alert box.

Theming via theme objects passed at the root. @inkjs/ui has had no release since 2.0.0 (May 2024; peer `ink >=5`), but it still works on Ink 8; fork or wrap any component you need to change.

## Testing — ink-testing-library

```tsx check=typescript/ink-testing-library
import {render} from 'ink-testing-library';

const tick = () => new Promise(resolve => setTimeout(resolve, 0));

const {lastFrame, rerender, stdin, frames, unmount} = render(<App />);
expect(lastFrame()).toContain('Counter: 0');  // frame includes the round border
stdin.write('\u001B[A');  // up arrow — see the input caveat below
await tick();             // input and re-render are asynchronous
expect(lastFrame()).toContain('Counter: 1');
unmount();
```

Plus `renderToString()` (Ink 6.8+) for synchronous render-to-string in tests.

**Staleness caveat.** ink-testing-library's last release is v4.0.0 (May 2024, unmaintained; its devDeps still pin `ink ^5` / React 18), but it installs and works with Ink 6.8, 7.1, and 8. `stdin.write` drives `useInput` and `usePaste` as long as the test awaits after each write, because frames update asynchronously. The reported "stdin.write() does not trigger useInput callbacks" failure reproduces only on Ink 5.0.1 when the test writes immediately after `render()`. Frames are text only because chalk sees a non-TTY stream, not because of the library (set `FORCE_COLOR=1` to assert styles). For app-wide context providers, wrap `render` in a small helper (Gemini CLI's vitest harness does this around Ink's own `render`), and add a few PTY-level tests (**node-pty** + `strip-ansi`) that drive the built CLI for real keyboard flows. Both `ink` and `ink-testing-library` are ESM-only, so vitest is the path of least resistance over Jest.

## Debugging

- **`console.log` is not lost.** The `patchConsole` render option defaults to `true`: Ink intercepts `console.*` calls, clears the live output, renders the message above it, and repaints the UI. If console output *is* garbling the screen, something bypassed the patch — direct `process.stdout.write` calls or child-process stdio inherited straight to the terminal.
- **`render(<App />, {debug: true})`** renders every update as separate appended output instead of diffing in place — the single best tool for answering "which frame went wrong".
- **React DevTools:** install the optional `react-devtools-core` peer, run your CLI with `DEV=true`, and launch `npx react-devtools` separately to inspect the live component tree (documented for Ink 8, which connects over Node's built-in `WebSocket` instead of bundling `ws`; exit the CLI manually with Ctrl+C afterwards). **This is for tree/props inspection only — don't reach for it to profile performance.** Ink's own docs never mention the Profiler/flame-chart tab, and its reconciler sets none of the flags React's Profiler mechanism needs; the Components tab genuinely works, but treat the Profiler tab as unsupported for Ink.
- **High-volume tracing** goes to stderr — `useStderr().write()` preserves Ink's output — or to a file you `tail -f` in another terminal. Never stdout; Ink owns it.
- **Profiling:** `node --cpu-prof` is the real path — it writes a `.cpuprofile` on process exit (no signal-forwarding gotcha to worry about, unlike Rust's `perf`-based tooling), loadable in Chrome DevTools' Performance panel or `speedscope`. Common Ink-specific cost sinks: unmemoized components re-rendering on every parent update (each one potentially triggers a fresh Yoga layout pass, pricier than a DOM diff); `<Static>` misuse — it exists for content written once and never revisited, and Ink 3 made it nearly 2x faster specifically because putting the wrong content in it (or leaving genuinely-static content out of it) was a known pain point; and a small always-animating leaf component (the standard `ink-spinner` is a named example) keeping the whole render cycle continuously active instead of idle between real state changes.

## Pastel — Next.js-style filesystem routing

`vadimdemedes/pastel` builds CLI command structure from filesystem layout, with Zod schemas for argument validation. Like Next.js for CLIs:

```
commands/
  index.tsx        // "myapp"
  create.tsx       // "myapp create"
  list.tsx         // "myapp list"
  user/
    add.tsx        // "myapp user add"
    remove.tsx     // "myapp user remove"
```

Each file exports a default React component plus optional `args`/`options` Zod schemas. Pastel 4 calls Ink's `render()` without options, so `alternateScreen`, `kittyKeyboard`, `incrementalRendering`, and `concurrent` can't be set (issue #43); full-screen apps that need them should call `render()` directly.

## Strengths and weaknesses

**Strengths:**
- Declarative, full React ecosystem available (use any state library, any hooks).
- Yoga flexbox is genuinely good for terminal layout.
- Render-to-string and `stdin.write` input tests work with ink-testing-library (await after each write); add PTY tests for end-to-end keyboard flows.
- React Devtools work (`DEV=true` env var).
- Native alternate-screen mode since Ink 7: `render(<App />, {alternateScreen: true})` enters the alt screen and restores the terminal on exit. Inline stays the default, and the option applies only in interactive mode; it is ignored in CI or with piped stdout. Historically Ink's biggest gap — previously papered over with `fullscreen-ink` hacks.
- Used by some of the most-deployed CLIs in the world.

**Weaknesses:**
- **Heavy startup**: loading React + Yoga + the reconciler takes roughly 150–250 ms to the first frame on a modern laptop (measure yours with `hyperfine`). Bad for one-shot scripts where users expect instant response. For frequently-invoked commands (`mycli --version`, `mycli completion`), provide a fast path that bypasses Ink.
- **ESM-only since Ink 4** (March 2023), and unlike chalk or commander it can't be `require()`d even on Node 22: Yoga's top-level await makes `require('ink')` throw `ERR_REQUIRE_ASYNC_MODULE` (Ink 6.8–8). From CommonJS, use `await import('ink')` or a bundler.
- **Node 22 floor** on Ink 7 and 8 (React ≥ 19.2 on Ink 7, ≥ 19.3 on Ink 8); Ink 6 is the last major for the end-of-life Node 20.
- **High-frequency re-renders used to flicker.** Ink 6.7+ ships synchronized updates (plus an opt-in Kitty keyboard protocol), which largely fixed this. When streaming LLM tokens or tailing logs, still prefer `useDeferredValue` (fully functional only with `render(<App />, {concurrent: true})`) or manual debounce; `<Static>` exists specifically to handle large append-only logs without re-rendering them. Ink 8's `incrementalRendering: true` rewrites only the changed part of each line, and a lower `maxFps` (default 30) cuts churn further.
- **Screen-reader support is opt-in and partial.** Ink 6.2 added `aria-label` and `aria-hidden` (on `<Box>` and `<Text>`), `aria-role` and `aria-state` (on `<Box>`, from fixed lists), and a screen-reader renderer that turns on only through `render(..., {isScreenReaderEnabled: true})` or `INK_SCREEN_READER=true`. Nothing detects a screen reader; `useIsScreenReaderEnabled()` only reports that flag. The renderer prints linear text without borders or color and replaces children with their `aria-label`, but it still erases and rewrites the whole live region on every change. In that mode, move finished output into `<Static>`, keep the live part small, and stop per-tick spinners and timers; Gemini CLI's `--screen-reader` mode also swaps in a separate layout and a static spinner. Design guidance: `visual-patterns.md` → *Accessibility*.

## Pitfalls

1. **Strings as direct children of `<Box>` throw.** Wrap in `<Text>`.
2. **`nodemon` breaks Inquirer/Ink arrow keys.** Use `nodemon --no-stdin` or `node --watch`.
3. **`ora` and Ink fight over the terminal.** Use `<Spinner>` from `@inkjs/ui` instead inside Ink.
4. **`console.log` from Ink 3+ is intercepted** and displayed cleanly above the live UI. Don't fight it.
5. **Ink 7 fixed Backspace reporting as `key.backspace`.** Old input handlers that check `key.delete` for Backspace silently break on upgrade—check `key.backspace`.
6. **Ink 8 upgrades break in three places:** percentage `minWidth`/`maxWidth` (numbers only now), `useStdout().stdout.columns` (no longer typed; use `useWindowSize()`), and `useInput` handlers that parsed mouse or focus reports (now dropped).

Raw-mode/TTY guards, CI detection, and Windows caveats apply here too — see "Pitfalls common to JS/TS terminal apps" below.

---

## Modern prompts: @clack/prompts

`@clack/prompts` has largely displaced Inquirer for new wizard-style CLIs. Now stable at 1.x under the bombshell-dev org (bomb.sh/clack) and actively released. Used by **create-vite, `sv` (Svelte's CLI), and create-t3-app**, and inspired the Rust port `cliclack`; create-astro ships the same look through its own `@astrojs/cli-kit` rather than Clack.

```ts check=typescript/clack
import {intro, outro, text, confirm, select, spinner, isCancel, cancel} from '@clack/prompts';

intro('create-my-app');

const name = await text({
  message: 'Project name',
  validate: v => !v ? 'Required' : undefined,  // v is string | undefined
});
if (isCancel(name)) {
  cancel('Cancelled');
  process.exit(0);
}

const framework = await select({
  message: 'Pick a framework',
  options: [
    {value: 'react', label: 'React'},
    {value: 'vue', label: 'Vue'},
    {value: 'svelte', label: 'Svelte'},
  ],
});
if (isCancel(framework)) {
  cancel('Cancelled');
  process.exit(0);
}

const s = spinner();
s.start('Installing dependencies');
await install();
s.stop('Dependencies installed');

outro(`You're all set!`);
```

**Components:** `intro`/`outro`, `text`, `multiline`, `password`, `confirm`, `select`, `multiselect`, `groupMultiselect`, `selectKey`, `autocomplete`, `autocompleteMultiselect`, `path`, `date`, `spinner`, `progress`, `tasks`, `taskLog`, `note`, `box`, `log.info/.warn/.error/.success`, `stream`, `group`. `validate` also accepts a Standard Schema validator (Zod, Valibot, ArkType) or an async function.

**`isCancel(value)`** is critical — it detects Ctrl+C per prompt and lets you clean up gracefully.

The visual style — blue-bordered connectors between prompts, single Unicode glyph progress markers — has become a recognizable aesthetic. If the user's building a `create-*`-style scaffolder, this is the right choice.

---

## @inquirer/prompts

The modern modular rewrite of Inquirer — TypeScript-first packages that replaced the legacy monolithic `inquirer`. The workhorse for prompt-heavy CLIs:

```ts check=typescript/inquirer
import {input, select, confirm, password} from '@inquirer/prompts';

const name = await input({message: 'What is your name?'});
const role = await select({
  message: 'Choose a role',
  choices: [
    {name: 'Admin', value: 'admin'},
    {name: 'User', value: 'user'},
  ],
});
```

**Modular packages:** `@inquirer/input`, `@inquirer/select`, `@inquirer/checkbox`, `@inquirer/confirm`, `@inquirer/password`, `@inquirer/editor`, `@inquirer/expand`, `@inquirer/rawlist`, `@inquirer/search`, `@inquirer/number`. Pull only what you use.

**i18n** via `@inquirer/i18n`.

**When to choose Inquirer over Clack:** complex prompt flows with conditional questions, validation chains, custom prompt types (extensible plugin architecture), i18n requirements. **When to choose Clack over Inquirer:** modern aesthetic, minimal API, you're scaffolding (a `create-*` tool).

---

## Color libraries

| Library | Size | Speed | API | Best for |
|---|---|---|---|---|
| **picocolors** | 7 KB | Fastest single-style | Functional only: `pc.red('hi')` | Tooling internals (PostCSS, SVGO, Stylelint, Browserslist, Babel, Prettier, Vite all use it) |
| **chalk** | ~56 KB (v6) | Slower; chainable | `chalk.red.bold('hi')` | End-user CLIs with frequent chaining; truecolor; familiar |
| **kleur** | Small | Fast | Chainable | Middle ground |
| **ansis** | Small | Fastest when chaining 2+ | Chainable + truecolor | Performance-critical with chained styles |

Color-disable behavior varies by library and version. Treat `NO_COLOR`, non-TTY output, and an explicit `--color` override as an application-level output policy, then configure or bypass the styling library accordingly. **chalk v5+ is ESM-only**, but CommonJS code on Node 20.19+/22.12+ can load it with `require('chalk').default` (`require(esm)`), so there is no need to pin chalk 4. **chalk 6 requires Node ≥ 22**; chalk 5 is the last line for the end-of-life Node 20. For zero dependencies, `node:util` `styleText` (added in Node 20.12, stable since 22.13; honors `NO_COLOR`/TTY since 20.18/22.8; hex colors since 24.16/26.1) covers basic styling.

Recommendation: **picocolors for libraries / internal tools, chalk for user-facing CLIs.**

---

## Other utilities

- **ora** — single elegant spinner; 90 styles via `cli-spinners`. Use outside Ink. Inside Ink, use `<Spinner>` from `@inkjs/ui`.
- **cli-progress** — single and multi-bar progress with ETA. Use outside Ink.
- **listr2** — hierarchical animated task list with concurrency, retries, rollback. Renderers: `default` (live), `simple` (line-per-task, CI-friendly), `verbose`, `silent`, `test`. Used by Angular CLI, GraphQL Code Generator, and Electron Forge (lint-staged dropped it in v17.1). listr2 11 requires Node ≥ 22.13.
- **boxen** — text in boxes; used by `update-notifier`. Useful for one-time announcements ("Update available!").
- **figlet** + **gradient-string** — banner text with color gradients, for branding splashes.
- **terminal-link** — OSC 8 hyperlinks; falls back to plain URL on terminals without support.
- **string-width** — cell width for strings (handles CJK, emoji). Ink uses it internally.
- **update-notifier** — checks npm registry for newer versions; shows a boxed notice.

---

## Argument parsers

| Parser | Adoption | Style | Best for |
|---|---|---|---|
| **commander** | The most downloaded by a wide margin | Fluent API | The default for most projects (webpack-cli, Babel CLI, Svelte's `sv`); v15 is ESM-only and needs Node ≥ 22.12 (CommonJS callers load it through `require(esm)`); v14, the last line for Node 20, gets security fixes until May 2027 |
| **yargs** | Very high | Fluent + middleware | Best validation; used by Jest and nyc |
| **citty** | Growing | TS-first, declarative | UnJS ecosystem (Nuxt, Nitro, unbuild) |
| **cac** | Niche | One ~20 KB file, no deps | Vite uses it; ESM-only since v7 |
| **oclif** | Plugin marketplace | Class-per-command | Heroku, Salesforce, Shopify CLI; heaviest startup of the group |
| **node:util.parseArgs** | stdlib | Argument parsing only | Experimental in Node 18, stable since Node 20; zero-dep; Mocha 12 moved to it from yargs |

**Startup overhead** grows from the stdlib parser and cac, through commander and yargs, to oclif's plugin loader at the heavy end. Measure your own binary with `hyperfine 'mycli --version'` rather than trusting published numbers; for frequently-invoked CLIs the difference matters.

**commander hello world:**

```ts check=typescript/commander
import {Command} from 'commander';

const program = new Command();
program
  .name('myapp')
  .description('CLI to do things')
  .version('1.0.0');

program.command('greet')
  .description('Say hello')
  .option('-n, --name <name>', 'who to greet', 'world')
  .action(({name}) => console.log(`Hello, ${name}!`));

program.parse();
```

---

## OpenTUI (anomalyco/opentui)

The credible new entrant, built by Anomaly: TypeScript bindings over a Zig native core via C ABI. Double-buffered rendering with alpha blending, scissor clipping, hit grid for mouse. Packages: `@opentui/core` (low-level), `@opentui/react` and `@opentui/solid` (React and SolidJS renderers), and `@opentui/three` — a Three.js WebGPU renderer that rasterizes 3D scenes into terminal cells, its most differentiating capability.

Powers **opencode** (Anomaly's terminal coding agent) in production.

**Choose if** pushing past Ink's performance ceiling — animations, real-time streaming with low latency, or complex layouts where Yoga's CPU cost matters. **Trade-off:** v0.x churn (0.6.0 moved layout and painting into the native core and broke custom renderables), smaller community than Ink, native binary in the install. `@opentui/core` 0.6+ declares `engines` of Bun ≥ 1.3.14 or Node ≥ 26.4 (Node 26 becomes LTS on 2026-10-28).

---

## blessed / neo-blessed / terminal-kit

Retained-mode classics, pre-Ink era.

- **blessed** — reimplements ncurses in pure JS with terminfo/termcap parsing, painter's algorithm with damage buffers. Massive widget set: `box`, `list`, `form`, `textbox`, `textarea`, `progressbar`, `log`, `table`, `tree`, `terminal` (embedded shell). **Largely abandoned** — last release (0.1.81) in September 2015.
- **neo-blessed** — fork of blessed; also unmaintained (last publish 0.2.0, June 2018).
- **terminal-kit** — cursor control, screen buffers, input fields, menus, **truecolor image drawing with half-block cells** (no Sixel), even a Document model.

**Choose** for precise damage-region control or maintaining legacy code; for images, prefer terminal-image (see the table at the top). **Don't choose** for new TUI apps in 2026 — Ink is better-supported and the React/JSX model is more productive.

---

## Notable JS/TS TUI apps to study

- **Claude Code** (Anthropic) — Ink + React.
- **GitHub Copilot CLI** — Ink.
- **Gemini CLI** — Ink.
- **Cloudflare Wrangler** — Ink.
- **Gatsby CLI** — Ink (was one of the first major adopters).
- **Prisma CLI** — Ink.
- **opencode** (Anomaly) — OpenTUI.
- **create-vite, create-astro, `sv`** — Clack-style scaffolders (`sv` replaced the deprecated create-svelte).
- **Listr** demos — task runner aesthetic.

---

## Pitfalls common to JS/TS terminal apps

1. **ESM/CJS and Node floors**. Much of the modern stack (chalk v5+, ora v6+, ink v4+, commander v15+, @inquirer/prompts, @clack/prompts) is ESM-only at latest, and the current majors of ink (7+), chalk (6), commander (15), listr2 (11), and execa (10) require Node ≥ 22; target Node 22 or 24. On Node 20.19+/22.12+, CommonJS code can `require()` synchronous ESM packages such as chalk, commander, @clack/prompts, and @inquirer/prompts, so don't pin old CJS majors. Ink is the exception: `require('ink')` throws `ERR_REQUIRE_ASYNC_MODULE`, so load it with `await import('ink')` or bundle.
2. **Restore terminal state on exit.** Ink: `exit()` or `unmount()`, then await `waitUntilExit()`; blessed: `screen.destroy()`. Treat OS signal listeners as an application boundary because Ink's Ctrl+C input handling does not cover SIGTERM.
3. **Detect non-TTY and CI.** `process.stdout.isTTY === false` or `process.env.CI` — degrade to plain output. Spinners and prompts must not run in CI.
4. **Raw mode requires `process.stdin.isTTY`.** Pipe input fails silently otherwise. Guard.
5. **Cell width**. Use `string-width` (Ink does); never `.length` for terminal width math.
6. **Frequently-invoked CLI startup**. Lazy-load subcommand modules. Have a fast path for `--version`/`--help`.
7. **Don't mix raw-mode libraries.** Ink + ora, Ink + blessed → broken.
8. **Windows older `cmd.exe`** doesn't support all ANSI. Target Windows Terminal.

---

## Stack recommendations by project shape

**Simple non-interactive CLI** (~50ms cold start, 19 packages and under 1 MB installed):
```
commander + picocolors + ora
```

**Interactive setup wizard** (`create-*` tool):
```
@clack/prompts + picocolors + commander
```

**Complex CLI with tasks**:
```
commander + @inquirer/prompts + listr2 + chalk + boxen + update-notifier
```

**Full TUI app**:
```
ink + @inkjs/ui + zustand + commander + ink-testing-library
```
(ink-testing-library 4.0.0 was built against Ink 5 / React 18 but still works with Ink 6–8; await after each `stdin.write`.)

**Heroku-class plugin CLI**:
```
oclif + ink + chalk + listr2
```

**Performance-critical / animated TUI**:
```
@opentui/core or @opentui/react
```

---

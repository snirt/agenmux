# #36: Safe application configuration implementation plan

**Status:** Implemented, with the amendments below. Kept as the design record;
read the amendments before trusting any specific field or command here.

**Amended by:**

- [Launcher configuration boundary](2026-09-06-launcher-configuration-boundary.md).
  `[keys.prefix]` is rejected as an unknown field: tmux configuration owns the
  opening keys, so the section shown below never shipped.
- [`agenmux config` output](2026-09-08-config-command-output.md). Reporting is a
  table of setting, value and source rather than the `field: source` lines this
  plan implies.
- `behavior.wheel_jump_ms` was dropped. Master replaced the delayed wheel jump
  with viewport scrolling before this work merged, so the setting would have
  configured a feature that no longer exists.
- `gg` and `G` edge navigation arrived on master separately. They are installed
  as fixed bindings, not configurable actions, and a configured chord on the
  same key replaces them.
- `agenmux config reload` re-reads the file on request and updates running
  views. The "no file watcher" decision below still holds: nothing re-reads on
  its own.

**Original status:** Proposed design for approval; no implementation authorized by this document.
**Issues:** [#36](https://github.com/snirt/agenmux/issues/36), related theme work [#51](https://github.com/snirt/agenmux/issues/51).
**Goal:** Optional XDG configuration covering application behavior, theme overrides, and keymaps without executable configuration or divergent split/popup behavior.
**Architecture:** Parse strict TOML into typed partial settings, resolve precedence once through one application-config module, and pass validated settings to existing owners. Keep detector configuration separate. Generate terminal styles and built-in action invocations from typed values, never user-supplied code.
**Tech stack:** Existing Rust/tmux implementation; add `serde` with derive and `toml` rather than inventing another parser. Use existing Rust and private-tmux test harnesses.

## Design decision

Recommend **typed application configuration**, not either alternative:

- Translating a file into `@agenmux-*` options loses source provenance, turns defaults into overrides, and perpetuates scattered parsing.
- Sourcing shell/tmux configuration makes theme/keymap data executable and introduces multiple quoting languages.
- Typed TOML gives explicit types, schema errors, deterministic precedence, and one validation boundary.

No file splitting is approved here. Propose one new `src/app_config.rs` for the new responsibility; retain existing renderer, input, detector, and tmux modules. If implementing shared action types requires moving existing input code into another module, obtain approval first; default is to keep those types in `sidebar.rs`.

## Observed integration points

- `src/conf.rs`: agent detection rules, not application behavior; loads shipped, legacy user, then canonical user agent files.
- `src/detect.rs::subject_cmd`: explicitly executes agent `SUBJECT_CMD` through `bash -c`.
- `agenmux.tmux`: pre-engine installer and prefix bindings; currently embeds paths/client formats in shell commands.
- `src/setup.rs`: legacy option migration, hooks, normal/search tables, mouse bindings, picker filter, status substitution.
- `src/sidebar.rs`: hardcoded palette, raw-terminal decoding, FIFO action encoding, dispatch, help text, live wheel/width reads and drag-written width option.
- `src/panes.rs`: independently reads width for creation and pinning.
- `src/toggle.rs`: independently resolves options, chooses display, and builds popup command.
- `src/notifications.rs`: independently reads notification setting at delivery.
- `src/tmux.rs`: argv command interface, string control-mode interface, and a shell-quoting helper. Shell quoting is not a universal tmux/format encoder.

## Configuration contract

### Discovery and precedence

1. Use `$XDG_CONFIG_HOME/agenmux/config.toml` when XDG home is nonempty and absolute; otherwise use absolute `$HOME/.config/agenmux/config.toml`.
2. If neither root is valid, use defaults without searching current directory. Explicit checking reports that no user config root is available.
3. No project-local discovery, includes, environment interpolation, shell expansion, network themes, or implicit loading of arbitrary paths.
4. No legacy application filename fallback: #36 has no existing application-file contract. Keep existing legacy **agent** directories unchanged.
5. Per field: explicit CLI value, where supported > explicitly present canonical tmux option > explicitly present legacy option > TOML > built-in default. An empty canonical option must not revive its legacy counterpart.
6. Theme resolution: selected built-in palette, then explicitly supplied semantic overrides. Key resolution: built-in action bindings, then replace only action lists explicitly supplied in TOML. Empty list unbinds that action; do not append silently.
7. Keep `@agenmux-bin`, installation paths, socket/client identifiers, and runtime options outside application-file schema. These are bootstrap/trusted execution or runtime state, not theme/keymap preferences.
8. Do not copy resolved values into public tmux options. Stop copying legacy behavioral options into canonical options: resolve compatibility on read so old copied values do not become a new source of stale precedence. Existing canonical options remain explicit until users unset them.

### Proposed example

```toml
version = 1

[display]
mode = "split"
sidebar_width = 30
popup_width = 40
popup_height = "auto"

[behavior]
notifications = true
wheel_jump_ms = 300 # use "off" to select without jumping
# Omitted: leave user's window-picker binding unchanged.
# Empty string: explicitly restore unfiltered picker.
hide_windows = "agents*"

[theme]
base = "light" # dark | light | terminal

[theme.colors]
header_bg = "#eeeeee"
header_fg = "#202020"
working_bg = "#fff0cc"
working_bg_unfocused = "#f7f2e5"
working_fg = "#775500"

[keys.prefix]
toggle = ["A"]
popup = ["e"]

[keys.normal]
down = ["j", "Down"]
up = ["k", "Up"]
jump = ["Enter", "l"]
search = ["/"]
filter = ["f"]
reset = ["Escape"]
help = ["?"]
versions = ["u"]
close = ["q", "Q"]

[keys.search]
up = ["Up", "C-p"]
down = ["Down", "C-n"]
accept = ["Enter"]
cancel = ["Escape", "C-c"]
backspace = ["BSpace"]
clear = ["C-u"]
```

Empty file and no file both retain current behavior. `version` defaults to 1 if omitted; reject other versions. Apply `deny_unknown_fields` at every structured level. Reject duplicate TOML fields, wrong types, unsupported actions, and alias collisions.

### Values and defaults

- Widths: positive bounded integers, 1..=10000 cells; clamp actual layout against available client/window dimensions. Preserve existing separate defaults 30/40. Existing `@agenmux-width` overrides both, using the same integer validation. Do not feed arbitrary strings to tmux geometry flags.
- Popup height: `"auto"` or integer 1..=10000; preserve current auto calculation but cap to actual available height, including terminals shorter than the old 15-row floor.
- Notifications: Boolean; compatibility options accept documented on/off forms, not arbitrary strings interpreted differently by consumers.
- Wheel delay: `"off"` or integer 0..=60000 milliseconds. Compatibility adapter converts finite nonnegative seconds to this type; reject NaN, infinity, overflow, negatives, and junk.
- Display: `split` or `popup`; normalize existing legacy `float` alias only in compatibility adapter.
- Picker exclusion: optional literal glob, maximum 256 bytes, no control characters. Preserve absent versus empty. README currently claims default `agents*`, while setup leaves an absent option alone: lock actual behavior with a regression test and correct documentation rather than introducing a default picker takeover.
- Theme roles: `header_fg/bg`, `text_fg`, `muted_fg`, `accent_fg`, `error_fg`, plus `blocked`, `working`, `idle`, and `done` each with `_fg`, `_bg`, `_bg_unfocused`. These cover row fills, cursor/status marks, groups, search, help, version overlay, and update notice. No configurable raw style strings or templates.
- Colors: `"default"`, integer 0..=255, or exact `#RRGGBB`; produce ANSI sequences internally. Preserve current dark default visual behavior, including basic ANSI colors where currently used. `terminal` uses terminal-default backgrounds; `light` supplies explicit readable foregrounds and pale fills. Keep state text/glyph distinctions independent of color.

### Keymap semantics

Use a typed `Action` and canonical `KeyChord`; do not make keymap values shell commands, tmux commands, FIFO bytes, or macros. Keep physical-input decoding separate from logical action dispatch. Both popup input and tmux table installation consume the same resolved keymap.

- Support printable ASCII, named arrows/Home/End/PageUp/PageDown/Enter/Escape/Tab/BSpace/Space, and representable `C-` combinations. Reject chords that cannot be represented unambiguously in both terminal input and tmux; reject modifier stacks and raw escape sequences. Prefix bindings may use the same restricted grammar.
- Canonicalize aliases and indistinguishable terminal encodings before checking collisions, e.g. `C-i`/Tab and `C-m`/Enter. Validate each mode after merging defaults and overrides.
- For search, ordinary printable characters remain query text; reject printable command bindings that would consume typing. UTF-8 search text remains supported; configurable non-ASCII chords are not required.
- Built-in mouse events remain fixed and cannot be replaced by arbitrary configured commands.
- Preserve current overlay behavior; use resolved navigation/accept/cancel actions in version picker and generate displayed help/hints from actual bindings.
- Reserve Ctrl-C/Ctrl-D as emergency terminal exit paths where appropriate; document split-versus-popup close behavior. EOF is transport closure, not configurable user action.
- FIFO packets represent actions/text, not physical keys. Remapping `j` must not reinterpret an already-dispatched `down` action. Preserve compatibility of public `agenmux key` aliases.
- Replace only plugin-owned prefix bindings; never bulk-unbind prefix/root tables. Record installed binding identity, and remove an old binding only if it still matches the plugin-installed command. Refuse collisions with unrelated existing prefix bindings rather than silently destroying them.

## Safety and lifecycle contract

The guarantee is **non-executable application configuration**, not a sandbox for the same Unix user or a claim that existing agent hooks are safe to import from strangers.

- Read at most 64 KiB from an opened regular file; enforce byte limit while reading, not only from pre-open metadata. Reject FIFOs/devices/directories without blocking. Permit regular-file symlinks for dotfile managers; do not add a misleading permissions-based sandbox.
- Decode strict UTF-8. Bound field sizes and binding lists (maximum 16 chords per action), validate every layer even when overridden, and reject invalid config before applying hooks, bindings, panes, or terminal raw mode.
- Report file location, field, and reason; escape control characters in diagnostics and avoid dumping full source documents or environment values.
- Use `Command::args` for executable arguments. Never `eval`, `source` app config, or build `sh -c` strings from its contents. An argv API alone does not protect tmux command/format parsing: treat those as separate languages.
- Configuration strings must never enter tmux control-mode command text or `run-shell` command bodies directly. Use fixed action enums, typed geometry, and explicit format-literal escaping where a literal glob enters `choose-tree` format syntax.
- Audit the existing binary-path shell, nested tmux command, `#(...)` status, and client-format interpolation boundaries touched by setup/toggle/bootstrap. Prefer native argv-capable tmux forms. Where tmux requires shell text, encode each interpretation layer explicitly and prove literal handling with real tmux tests. Do not reuse `tmux::quote` as a universal encoder.
- Keep agent rules and `SUBJECT_CMD` outside TOML. Document them as trusted executable customization. Do not silently enable app-config command hooks or change detector behavior in this issue.
- Load file once per process, not per frame. Keep live tmux overrides for width/wheel/notifications through the same typed resolver. Batch reads on existing control connection where practical; no added subprocesses on each render.
- Border dragging stays a transient live width override; never rewrite TOML. Explain that unsetting `@agenmux-width` returns to file/default width.
- V1 activation is explicit: `agenmux config check`, then `agenmux setup`, then close/reopen running sidebar/popup. No file watcher or fake hot reload. Config check works without tmux and does not execute agent hooks or network/update operations; `--effective` additionally validates current tmux overrides and reports per-field source.
- Setup validates everything before mutation. A tmux application failure is reported, not swallowed; snapshot and restore plugin-owned binding changes on failure, reporting rollback failure too. Do not claim tmux provides an atomic multi-command transaction.
- Teardown, emergency close/key delivery, and notification-open must remain available with broken app config. Do not put unconditional config loading ahead of every CLI command.
- Bootstrap must install/verify engine before Rust owns config-dependent prefix bindings. Remove duplicate permanent binding logic; when installation is pending, report that fact rather than creating a second parser or temporary configured-binding workaround. Preserve serialized installer and stale-engine version checks.

## Implementation sequence

Each task: write listed failing checks, run targeted suite to confirm failure, implement, rerun to green, inspect diff, then make one focused commit. Do not commit before checks pass. This document defines intended contracts, not existing APIs.

### 1. Typed schema, discovery, and read-only validation

**Files:** create `src/app_config.rs`; modify `Cargo.toml`, `Cargo.lock`, `src/main.rs`, `tests/cli.rs`.
**Interfaces:** `app_config::parse(&str) -> Result<FileConfig, ConfigError>`; `app_config::load() -> Result<FileConfig, ConfigError>`. `FileConfig` stores optional fields, distinct from resolved defaults. `ConfigError` owns escaped diagnostic rendering.

- [ ] Add discovery tests using explicit root inputs rather than mutating process-global environment in parallel tests.
- [ ] Add parser tests for empty/default/example config, unknown nested fields, duplicate keys, unsupported version, non-UTF-8, invalid colors/delays/geometry/chords, oversized and non-regular files.
- [ ] Implement schema/deserialization/validation and `agenmux config check` without tmux access; invalid input exits 2, read errors exit 1, valid/missing file exits 0.
- [ ] Add a public CLI regression showing config check does not execute a `SUBJECT_CMD` marker even when agent files exist.
- [ ] Run `cargo test --bin agenmux app_config` and `cargo test --test cli`.
- [ ] Commit: `feat: add strict XDG application config validation`.

Representative unit check inside `app_config.rs`:

```rust
#[test]
fn rejects_executable_or_unknown_theme_values() {
    assert!(parse("[theme.colors]\nheader_bg = '#(touch marker)'\n").is_err());
    assert!(parse("[theme]\ncommand = 'touch marker'\n").is_err());
    assert!(parse("[theme.colors]\nheader_bg = '\u{1b}[31m'\n").is_err());
}
```

### 2. One resolver for behavior and bootstrap ownership

**Files:** `src/app_config.rs`, `src/main.rs`, `src/setup.rs`, `src/toggle.rs`, `src/panes.rs`, `src/sidebar.rs`, `src/notifications.rs`, `agenmux.tmux`, `tests/plugin.rs`, `tests/run.sh`.
**Interfaces:** `app_config::resolve(file: &FileConfig, options: &std::collections::BTreeMap<String, String>) -> Result<AppConfig, ConfigError>`; `AppConfig` holds resolved display/behavior/theme/keys. Map membership expresses option presence; empty value is not absence. Use this resolver for argv-based setup helpers and control-mode runtime readers.

- [ ] Add precedence table tests: missing/empty canonical, legacy-only, file-only, explicit disable, unset override, conflicting sources, malformed shadowed values. Test absent picker leaves existing binding unchanged.
- [ ] Replace scattered behavioral parsing and legacy-copy side effects with resolver calls. Keep runtime state and binary selection distinct.
- [ ] Validate requested CLI mode before mutation, including explicit empty mode used by bootstrap.
- [ ] Move permanent prefix-binding ownership to Rust after verified engine installation. Make setup failures visible through bootstrap instead of discarding errors.
- [ ] Add `config check --effective` with source labels; preserve no-server operation for plain check.
- [ ] Test live width, border drag, notifications, wheel delay and new pane pinning; invalid configuration must not prevent teardown or change existing layout.
- [ ] Run `cargo test --test plugin`, `cargo test --test cli`, and existing shell bootstrap checks with the newly built binary.
- [ ] Commit: `feat: resolve application settings consistently across runtime paths`.

### 3. Typed themes and all renderer consumers (#51)

**Files:** `src/app_config.rs`, `src/sidebar.rs`; extend existing inline renderer tests.
**Interfaces:** `Color::{Default, Indexed(u8), Rgb(u8,u8,u8)}` and resolved `Palette`; renderer consumes `&Palette`, never color source strings. Preserve internal distinction needed for existing basic ANSI defaults.

- [ ] Capture sanitized baseline dark frames before replacing constants. Assert no-config output remains equivalent.
- [ ] Add light/terminal/partial-override tests for every state, focused/unfocused rows, header, cursor, search and both overlays.
- [ ] Replace palette constants and hardcoded semantic color use with resolved values; centralize foreground/background encoding, preserving reset/background restoration.
- [ ] Assert one working-color override leaves other roles unchanged; assert control/OSC/DCS payloads cannot produce a palette.
- [ ] Verify narrow terminals, default backgrounds, visible non-color state cues, and split/popup rendering parity.
- [ ] Run `cargo test --bin agenmux`; manually inspect both background types in private tmux.
- [ ] Commit: `feat: support built-in themes and semantic color overrides`.

### 4. Shared logical keymap, not duplicated remapping

**Files:** `src/app_config.rs`, `src/sidebar.rs`, `src/setup.rs`, `src/input.rs`, `tests/navigation.sh`, `tests/plugin.rs`.
**Interfaces:** typed `Action`/`KeyChord` shared by config validation, tmux installer, popup decoder and dispatcher. A resolved per-mode lookup returns `Option<Action>`; transport EOF and text remain separate input events.

- [ ] Add tests remapping normal down/up, unbinding defaults, alias collisions, missing safe exit, conflicting prefix ownership, and rejecting unsupported chords/actions.
- [ ] Decode terminal bytes into physical events before keymap lookup. Keep FIFO actions independent of physical bindings, with compatibility tests for existing `agenmux key` names and atomic bounded packet writes.
- [ ] Generate tmux bindings exclusively from validated chords and fixed actions. Keep printable search text distinct from normal action keys.
- [ ] Generate help/search/version hints from resolved bindings; test disabled actions do not leave misleading hints.
- [ ] In private tmux, remap `down` to `n`, clear old `j`, navigate split and popup, enter query `jqf`, accept/cancel, open/close overlays, and verify actions never leak to underlying agent pane.
- [ ] Test repeated setup, changed/removed prefix keys, preserving user-replaced bindings, and rollback when a later binding command fails.
- [ ] Run `cargo test --test plugin` and `AGENMUX_BIN=target/debug/agenmux bash tests/navigation.sh`.
- [ ] Commit: `feat: share configurable actions across sidebar input modes`.

### 5. Injection boundary verification and hardening

**Files:** `src/tmux.rs`, `src/setup.rs`, `src/toggle.rs`, `agenmux.tmux`, `tests/plugin.rs`, `tests/cli.rs`.
**Interfaces:** retain argv command API; give any new encoder an explicit target language in its name. No generic 'escape everything' function.

- [ ] Add real private-server marker tests for literals containing spaces, quotes, dollar signs, backticks, semicolons, backslashes, newlines, `#{...}`, and `#(...)`. Separate rejected config fields from valid literal filesystem paths/globs so tests do not merely prove blanket rejection.
- [ ] Exercise actual prefix activation, installed action, popup launch, picker format, and status substitution, not only generated string equality.
- [ ] Place trusted test executable in adversarially named directories and prove correct executable runs, marker commands never run, and unexpected tmux options/bindings are not created.
- [ ] Verify escaped error output contains no live ESC/OSC/DCS/control sequences; cap diagnostic size.
- [ ] Remove nested interpolation hazards in touched paths with argv forms or layer-specific encoding; rerun every caller through the same tests.
- [ ] Run `cargo test --test plugin`, `cargo test --test cli`, and inline tmux encoder tests.
- [ ] Commit: `fix: keep config and launch data literal across tmux boundaries`.

### 6. Documentation and release acceptance

**Files:** `README.md`; create `examples/config.toml`; update `tests/cli.rs` to validate shipped example.

- [ ] Document discovery, precedence/source reporting, all accepted values/actions/chords, absent versus empty, explicit activation sequence, picker behavior, and border-drag override removal.
- [ ] Document app config as data and agent hooks as trusted code; never call the entire app injection-proof.
- [ ] Include minimal and full example; parse shipped full example through public `config check` in a temporary XDG root.
- [ ] Run LSP diagnostics before builds, then `cargo fmt --check`, `cargo test`, `cargo build --release`, and `AGENMUX_BIN=target/release/agenmux bash tests/run.sh` on supported macOS/Linux environments. Existing release tests may use network: record unavailable dependencies rather than claiming green.
- [ ] Inspect active `@agenmux-bin`, derive plugin root, confirm actual loaded `agents/*.conf`, and use a fresh real-agent tmux pane for final smoke test. Restart active daemon only with user approval; verify working during activity and idle afterward under default and custom config. No detector changes are planned; if any become necessary, follow repository's full real-fixture transition rules.
- [ ] Confirm no-config defaults, custom light palette, split/popup key parity, invalid-config recovery, small terminals, and no added per-frame config I/O/processes.
- [ ] Inspect exact diff and git status; scan changed/new files for secrets/private identifiers; keep captures and generated diagnostic/session artifacts untracked and `.pi/` ignored.
- [ ] Commit: `docs: document safe configuration themes and keymaps`.

## Completion criteria

Issue #36 is complete only when one optional file configures every documented behavior above, both UI modes consume the same validated settings/actions, invalid input cannot partially apply user settings, and adversarial tests pass through real tmux. #51 can close when light/terminal themes plus semantic overrides cover the whole UI and its visual acceptance passes. No watcher, executable hooks, arbitrary command bindings, project config, remote theme loader, or generic plugin/settings framework is part of this plan.

## Approval requested before implementation

Approve proposed TOML schema, existing-tmux-option precedence, shared portable key subset, and explicit setup/reopen activation model. This is a planning deliverable only; no application files, GitHub issue bodies, or active daemon were modified.

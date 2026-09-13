# Responsive Settings View Design

**Issue:** #83

## Goal

Expose every application configuration field in one responsive split/popup view while retaining the existing TOML schema, precedence rules, validation, and reload behavior.

## Design

Settings is an existing sidebar overlay opened by configurable normal action `settings` (default `s`). It reads and edits the existing XDG TOML document; no second settings store exists. Wide views show persisted, effective, and source columns. Narrow views show compact rows and details for the selected setting. Height-aware scrolling and terminal-cell clipping keep controls usable in short panes.

Edits preserve TOML comments and formatting with `toml_edit`. Candidate documents pass the existing typed parser and resolver before an atomic same-directory replacement. Applying reuses setup's key-table rollback and the existing reload signal. Failure restores previous file and runtime settings.

**Revert to defaults** removes persisted display, behavior, theme, color, and keymap tables after confirmation while retaining the schema version and harmless document comments. CLI and tmux overrides remain effective and visible.

Launcher bindings and `agents/*.conf` remain outside application configuration.

## Input

Browsing uses configured navigation actions. Editing uses fixed printable, arrow, Enter, Escape, Backspace, and clear controls so changing a keymap cannot strand the active editor. Popup reads terminal input directly; split mode sends equivalent fixed FIFO packets through a dedicated tmux key table.

## Verification

Cover document preservation, validation, symlinks, revert semantics, responsive frames, generated key tables, split/popup persistence, reload behavior, and existing overlay regressions through Rust tests and the private-tmux navigation harness.

## Reusable Controls Follow-up

Introduce `src/sidebar/ui.rs` as the internal seam for Settings controls. It exposes four control kinds: `Label`, `Select`, `TextEdit`, and `Action`. Each control owns rendering-relevant state and keyboard behavior; Settings continues to own persistence, live application, rollback, and effective-source data.

Closed-list fields (`display.mode`, `theme.base`, `display.show_all_panes`, and `behavior.notifications`) use `Select`. Enter expands options inline beneath the setting while keeping the Settings list visible. Up and Down move the highlighted option, Enter saves and collapses, and Escape cancels. Expanded options count toward viewport height so scrolling retains the highlighted option in wide, narrow, and short panes.

Open-value fields continue to use `TextEdit`. Revert remains an `Action`; headings and source details use `Label`. Existing TOML validation remains authoritative after control-level interaction.

### Implementation

- [x] Add render/input coverage for inline Select open, navigation, save, cancel, and short-pane scrolling.
- [x] Create `src/sidebar/ui.rs` with the minimum shared control state and rendering helpers.
- [x] Replace Settings-specific choice cycling and text-editor branching with reusable controls.
- [x] Keep the Versions picker separate because its dynamically refreshed options do not match the static Settings `Select` interface.
- [x] Run focused Settings tests, full Rust tests, release build, navigation harness, diagnostics, diff review, and privacy/secret scans.

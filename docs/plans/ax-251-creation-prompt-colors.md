# Issue #251: Matching creation prompt colors

## Scope and decisions

- `cs` uses the existing session accent; `cc` uses the existing window foreground.
- Reuse the shared palette; introduce no new colors, settings, or UI components.
- Preserve indentation, clipping, selection background, and behavior.
- Issue has no comments or unresolved product decisions.
- Initial branch is clean at `e3f2daa`, matching freshly fetched `origin/master`.

## Implementation

1. Add regression coverage through the sidebar renderer for both creation actions,
   with distinct session/window colors and empty and typed names.
2. Confirm the window assertions fail before changing production code.
3. Use the window foreground for the window creation row only.

## Verification

- Run the focused rendering regression and `cargo test --locked`.
- Run `bash tests/run.sh` and `cargo fmt --check` under explicit Bash.
- Inspect the final diff, working tree, and changed files for unintended changes
  and private or secret-like data.
- Do not bump a release, reset, stash, or rebase; push only with explicit approval.

## Outcome

- Fixed both agent-only inline prompts and all-panes tree placeholders.
- Regression first failed on the window prompt, then passed with the fix.
- `cargo test --locked`: 351 passed, 1 ignored.
- `bash tests/run.sh`: 116 checks passed. Initial 120-second invocation timed
  out; a full rerun with a longer timeout passed.
- `cargo fmt --check` and `git diff --check` passed.
- Rust LSP was unavailable; Rust compilation and tests provided verification.
- Final diff and added-content privacy scan passed; `.pi/` remains ignored.

## Follow-up: matching creation icons

- Use the ordinary window icon for the `cc` tree placeholder, with the existing
  plain-glyph fallback; retain the session marker for `cs`.
- Share the window glyph choice with ordinary window rows.
- Extend renderer coverage to both glyph modes, and update real tmux integration
  assertions to expect the window icon.
- Full Rust suite passed again: 351 passed, 1 ignored. Shell suite passed again.

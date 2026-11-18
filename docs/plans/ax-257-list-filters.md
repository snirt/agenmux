# Issue #257 · `agenmux list --all`

## Compatibility decision

`--session` and `--command` narrow the selected rows. Use `--all` to opt in
to non-agent panes.

## Implementation

1. Extend the shared `list` and `scan` parser to accept `--all` in any flag
   order. Keep plain `list` agent-only and preserve the existing TSV columns,
   ordering, and missing-session error.
2. Apply each filter to the existing scan snapshot. Keep
   `--agent` restricted to matched agents, including when `--all` is present.
3. Update the CLI help and `docs/usage.md`. Document the migration to
   `--all` in `RELEASE_NOTES.md`.
4. Use the existing Rex CLI fixture to verify plain output, all sessions,
   each filter with and without `--all`, and the `scan` alias.

## Verification

Run focused CLI tests, `cargo test --locked`, `bash tests/run.sh`,
`cargo fmt --check`, and `git diff --check`. Review the final diff and scan
changed files for private data.

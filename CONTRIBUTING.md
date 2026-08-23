# Contributing

Thanks for helping out. This is a small Rust plugin with minimal pre-binary
shell bootstrap — keep changes in that spirit.

## Setup

```sh
git clone https://github.com/snirt/tmux-agents-mon
cd tmux-agents-mon
cargo test
tests/run.sh          # private-tmux and integration checks
tests/sanity.sh       # Nix release/install smoke (network required)
```

Requirements: Rust 1.90 or newer, tmux, and bash for TPM/pre-binary bootstrap.

## Adding an agent

Most contributions are new agents — and most need **no code**, just a `.conf`.
See [Adding / overriding agents](README.md#adding--overriding-agents) for the
config format.

1. Add `agents/<name>.conf`.
2. Capture real screens into fixtures so the detection is tested against actual
   output:

   ```sh
   tmux capture-pane -p -t <pane> > tests/fixtures/<name>-idle.txt
   tmux capture-pane -p -t <pane> > tests/fixtures/<name>-working.txt
   tmux capture-pane -p -t <pane> > tests/fixtures/<name>-blocked.txt
   ```

   Real captures beat synthetic ones — only reconstruct a screen by hand when a
   state is hard to trigger.
3. Add the expected states to the test suite and run `tests/run.sh`.

## Code changes

- Rust is the sole runtime. Keep bootstrap and packaging shell small and avoid
  new runtime dependencies.
- Detection lives in `src/detect.rs`; `agents-mon list`/`status` TSV and status
  output are contracts consumed by the sidebar and tmux status segment.
- Runtime tmux integration lives in `src/input.rs`, `src/panes.rs`,
  `src/setup.rs`, and `src/toggle.rs`; preserve option names, hook indexes,
  processless panes, and exact-client targeting.
- The only shell boundary is `agents-mon.tmux`, `scripts/install-bin.sh`,
  `scripts/install-app.sh`, and `scripts/version.sh`. Do not put runtime logic
  back into shell wrappers.
- Match existing Rust and shell style, quote shell expansions, and prefer tmux
  format strings over extra subprocesses on hot paths.

## Before you open a PR

- [ ] `cargo test` passes
- [ ] `tests/run.sh` passes (includes `tests/no-stale-runtime-refs.sh`)
- [ ] New/changed detection has a fixture behind it
- [ ] README updated if you added an option or changed behavior
- [ ] One focused change per PR

## Releasing

`Cargo.toml` is the only source of truth for the project version. Update its
`[package].version`, then let Cargo refresh the generated lockfile:

```sh
cargo check
scripts/version.sh tag
```

Commit both manifest and lockfile changes, then create and push the tag printed
by `scripts/version.sh tag`. GitHub Actions rejects a release tag that does not
match the manifest and publishes only after the version, sanity, and platform
build jobs pass.

## Reporting bugs

Detection is scraping-only, so state bugs are usually a screen that didn't match
a rule. Include a `tmux capture-pane -p` dump of the misdetected pane, the agent,
and what state you expected — that dump can often become the fixture that fixes it.

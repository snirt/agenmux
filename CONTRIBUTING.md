# Contributing

Thanks for helping out. This is a small Rust plugin with minimal pre-binary
shell bootstrap — keep changes in that spirit.

## Setup

```sh
git clone https://github.com/snirt/agenmux
cd agenmux
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
- Detection lives in `src/detect.rs`; `agenmux list`/`status` TSV and status
  output are contracts consumed by the sidebar and tmux status segment.
- Runtime tmux integration lives in `src/input.rs`, `src/panes.rs`,
  `src/setup.rs`, and `src/toggle.rs`; preserve option names, hook indexes,
  processless panes, and exact-client targeting.
- The only shell boundary is `agenmux.tmux`, `scripts/install-bin.sh`,
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

`Cargo.toml` is the only source of truth for the project version. Update
`RELEASE_NOTES.md` first, then prepare a patch or minor release:

```sh
make patch-bump          # 0.6.1 -> 0.6.2; make bump is an alias
make minor-bump          # 0.6.1 -> 0.7.0 instead, resetting patch to zero
git diff                 # review notes, Cargo.toml, and Cargo.lock
cargo test
tests/run.sh
```

Preparation checks that notes changed since the previous release, then updates
the manifest and generated lockfile. It creates no commit or tag. Commit the
three files and open a PR. CI checks release readiness on version-changing PRs
and again on untagged `master` or a manually pushed release tag, before builds.
Ordinary PRs do not need new release notes. Once the checks and platform builds
pass on `master`, CI tags that commit and publishes the release from the same
run. A `master` push whose version is already tagged does nothing.

`make release` remains a guarded manual fallback for a local bump commit and
tag on `master`. It pushes both atomically; use it only when automatic
publication cannot be used. CI rejects a manually pushed tag that does not
match the manifest.

## Reporting bugs

Detection is scraping-only, so state bugs are usually a screen that didn't match
a rule. Include a `tmux capture-pane -p` dump of the misdetected pane, the agent,
and what state you expected — that dump can often become the fixture that fixes it.

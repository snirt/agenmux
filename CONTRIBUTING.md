# Contributing

Thanks for helping out. This is a small, dependency-free bash plugin — keep
changes in that spirit.

## Setup

```sh
git clone https://github.com/snirt/tmux-agents-mon
cd tmux-agents-mon
tests/run.sh          # everything should pass before you start
```

Requirements: tmux, bash, grep, awk, ps. No build step, no package manager.

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

- Keep it bash + coreutils. No new runtime dependencies.
- Scripts live in `scripts/`; each does one thing (`scan`, `sidebar`, `toggle`,
  `follow`, `click`, `orphan`).
- `scripts/scan.sh` is the detection core — the CLI (`scan.sh list` / `status`)
  is the contract the sidebar and status segment build on. Don't break its
  output format without updating both consumers.
- Match the existing style: `set -euo pipefail`, quote expansions, prefer tmux
  format strings over extra subshells (they run on the render tick).

## Before you open a PR

- [ ] `tests/run.sh` passes
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

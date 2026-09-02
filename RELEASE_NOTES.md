# Agenmux v0.3.0

## What's changed

### Agenmux rebrand

- Renamed the runtime, binaries, tmux options, configuration path, release artifacts, and documentation to Agenmux ([#37](https://github.com/snirt/agenmux/pull/37)).
- Preserved `agents-mon` entrypoints, options, environment variables, config paths, package launchers, and release state as compatibility inputs for one release cycle.
- Renamed the macOS notification helper to `Agenmux.app` with bundle ID `io.github.snirt.agenmux`.

### Developer workflow

- Added `make dev-use` and `make dev-stop` for switching the active tmux server between local debug and release binaries.
- Added debug build timestamps to sidebar titles while keeping release titles versioned.

### Assets

- Linux x86_64
- Linux aarch64
- macOS x86_64
- macOS aarch64
- SHA-256 checksums

**Full changelog:** <https://github.com/snirt/agenmux/compare/v0.2.1...v0.3.0>

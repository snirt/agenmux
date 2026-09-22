# agenmux v0.6.1

## What's changed

v0.6.1 includes all changes from v0.6.0.

### Tmux integration

- Reduced setup overhead by skipping unchanged plugin setup and batching generated key bindings, cutting unchanged reloads to about 0.2 seconds and reducing first-setup tmux client calls ([#115](https://github.com/snirt/agenmux/pull/115)).

### Reliability

- Made the mirror lifecycle integration test wait for delayed agent frames instead of relying on a fixed sleep ([#116](https://github.com/snirt/agenmux/pull/116)).

### Assets

- Linux x86_64
- Linux aarch64
- macOS x86_64
- macOS aarch64
- SHA-256 checksums

**Full changelog:** <https://github.com/snirt/agenmux/compare/v0.6.0...v0.6.1>

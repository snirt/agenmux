# Development

## Tests

```sh
cargo test --locked  # Rust unit and integration tests
tests/run.sh         # fast fixture and integration tests
tests/sanity.sh      # release smoke + source build (requires tmux 3.7)
make container-test # run the release/real-tmux sanity suite in Docker or Podman
make container-use  # open the current checkout in an interactive tmux harness
make container-install # run the public website installer in a clean tmux harness
make container-install-local # run this checkout's installer before site deployment
```

For live development without overwriting the installed release binary:

```sh
make dev-use                                # run this checkout on the host tmux server
make dev-docker                             # run this checkout with Pi in Docker
make dev-docker REF=master                  # clone and run GitHub master in Docker
make dev-docker REF=my-feature              # clone and run a pushed branch in Docker
make dev-docker TMUX_CONFIG=/path/to/tmux.conf # override host tmux config path
make dev-stop                               # restore the existing release binary
```

`dev-use` builds with mise-managed `rust@latest`. `dev-use` and `dev-stop`
preserve sidebar state. Debug builds show `agenmux dev (YYYY-MM-DD HH:MM)`
with the local build time. `dev-stop` restores the existing local release
binary; it does not download a newer GitHub release. Docker copies
`~/.tmux.conf` into the disposable container by default; set `TMUX_CONFIG` to override its path. Docker drops
host-specific `agenmux.tmux`/`agents-mon.tmux` bootstrap lines and runs the
installer wizard against a writable copy. It does not edit the host config.
Other host-only plugin paths need matching mounts.
The Docker image caches system tools, Rust, and Pi; named volumes cache Cargo
downloads and build output between runs.

The OCI harness accepts Docker or Podman (override detection with
`CONTAINER_ENGINE=podman`). `container-test` builds one pinned image containing
the exact checkout and tmux 3.7b, then runs the release and real-tmux sanity
checks. `container-use` bind-mounts the current checkout, builds
it in an isolated target directory, and attaches to a disposable tmux session
starting in a real shell with a mock Codex agent in a second window. Press
`prefix + A` to exercise the sidebar, then use
`cc` for a window or `cs` for a session. `container-install` instead starts a
clean disposable HOME and runs the public website installer in its shell so its
prompts, clone, binary verification, tmux.conf update, and reload can be exercised
end to end. `container-install-local` uses this checkout's `install.sh` in the same
harness, allowing installer changes to be tested before the website is deployed.
GitHub Actions uses the baked image without a bind mount, so CI always
tests the baked commit. Network access is
required for the release smoke checks. Rust integration tests also create private
tmux servers for exact-client, pane lifecycle, setup, toggle, and release behavior.
The image runs under `C.UTF-8` so tmux preserves the sidebar's Unicode glyphs;
the terminal itself still renders them and must use a Nerd Font for private-use icons.

Any interactive container target can start from a local tmux config:

```sh
AGENMUX_CONTAINER_TMUX_CONF=~/.tmux.conf make container-use
```

The file is mounted read-only, copied into the disposable HOME, and never modified
on the host. The same variable works with `container-install` and
`container-install-local`; installer edits affect only the copy.
Host-specific `default-shell` and `default-command` entries are replaced with
`/bin/bash` and an empty default command inside the disposable copy, so macOS paths
such as `/opt/homebrew/bin/nu` cannot prevent Linux panes from starting.
Existing `agenmux`/`agents-mon`, TPM runner, and `@plugin` lines are also removed
from the disposable copy so the harness exercises a clean install and cannot retain
bindings or plugin-manager commands that point to host-only paths. Choose launcher
keys again when the installer prompts.

Runtime shell is limited to five entrypoints: `agenmux.tmux` is TPM/pre-binary bootstrap,
`scripts/install-bin.sh` installs and verifies the engine,
`scripts/install-app.sh` packages the macOS notification app,
`scripts/version.sh` validates manifest/release versions, and
`scripts/container-entrypoint.sh` drives the OCI test harness. The other `scripts/` files are
release and dev tooling. All plugin runtime behavior lives in Rust.

Fixtures in `tests/fixtures/` are named `<agent>-<state>[-N].txt`, with an
optional matching `.title`. Most are sanitized real `tmux capture-pane -p` dumps;
hard-to-trigger states (some `*-blocked` and `opencode-*`) are synthetic
reconstructions. To improve accuracy, re-capture a real screen into a fixture:

```sh
tmux capture-pane -p -t <pane> > tests/fixtures/claude-blocked.txt
```

## Runtime architecture

The Rust engine is the sole runtime implementation. It runs the scan/sidebar
hot path with one persistent tmux control-mode connection. Pane output from the
attached session invalidates cached screens and triggers a scan no more often
than every 500 ms. Inventory and background sessions are still reconciled every
two seconds, and attached-session screens are recaptured at least every ten
seconds; silence is never treated as an agent state. A key arriving mid-scan
stops the capture loop: panes not yet recaptured keep their last screen and
are refreshed on the next output scan. Direct `scan`/`list`
commands always take a fresh snapshot. The plugin downloads and verifies a
prebuilt binary automatically; if one is unavailable and
[cargo](https://rustup.rs) is installed, it builds the engine in the background. `make build` does the same
by hand, and `@agenmux-bin` overrides the binary path. Agent detection stays
in `agents/*.conf`, so adding or tuning agents never needs a rebuild. Building
on macOS needs rustc 1.90 or newer (for the native notification helper).

Sidebar (`split`) mode creates and live-renders the focused window before open
returns. Other windows receive panes lazily when a real client visits them, so
startup cost does not grow with hidden-window count. Each pane runs a lightweight
input reader for terminal paste; the single daemon scans the global inventory
and sends frames only to panes visible in attached clients. With every client
detached, one active pane stays warm for the next attach.

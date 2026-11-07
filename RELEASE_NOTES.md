# agenmux v0.8.7

## What's changed

v0.8.7 shows pi as working again, applies a downloaded update while the sidebar stays open, keeps the daemon alive through a short tmux stall, and fixes popup sizing and clicks in an unframed sidebar.

### Agent detection

- pi panes show as working again. With an extension that installs its own editor, pi 1.5.0 draws its status as a separate line (` ⠙ Working`) instead of in the editor border, and the old rule missed it, so pi always read idle. The rule now matches pi's spinner in either spot, including retries and compaction, and checks the bottom 40 lines so a tall todo or recap widget above the editor no longer hides it ([#250](https://github.com/snirt/agenmux/pull/250)).
- Agent confs accept `SCREEN_LINES`, the number of bottom screen lines the `*_SCREEN` rules check (default 20) ([#250](https://github.com/snirt/agenmux/pull/250)).

### Auto-update

- An open sidebar no longer waits for a fresh start to switch to a prepared release. Once the release is ready, the sidebar view closes and reopens on it, so its panes run the new engine. An open popup is left alone and switches on its next launch. Before, the version picker stayed on `ready · next start` until the tmux server restarted ([#246](https://github.com/snirt/agenmux/pull/246), [#245](https://github.com/snirt/agenmux/issues/245)).

### Daemon

- A tmux server that stops answering for a few seconds (sleep, heavy load) no longer makes the daemon exit with `no preserved panes left` and leave frozen sidebars. An unanswered pane query now skips that refresh instead of counting as "no sidebars" ([#238](https://github.com/snirt/agenmux/pull/238), [#237](https://github.com/snirt/agenmux/issues/237)).

### Popup

- A fixed `@agenmux-width`/`@agenmux-height` is clamped against the client that opens the popup. Before, it used the size of a client on another session, so a larger client elsewhere made `display-popup` fail with "height too large" ([#242](https://github.com/snirt/agenmux/pull/242), [#162](https://github.com/snirt/agenmux/issues/162)).
- The scan cache is written to a temporary file and renamed into place, so `#{agenmux}` and popup sizing never read it half-written ([#242](https://github.com/snirt/agenmux/pull/242)).

### Sidebar

- With `show_frame = false`, a click selects the row under the cursor instead of the row below it ([#242](https://github.com/snirt/agenmux/pull/242)).

### Assets

- Linux x86_64
- Linux aarch64
- macOS x86_64
- macOS aarch64
- SHA-256 checksums

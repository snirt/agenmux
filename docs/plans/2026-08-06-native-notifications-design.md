# Native notifications for hidden agent panes

## Decision

The existing Rust sidebar process sends native desktop notifications when an
agent transitions to blocked or reaches stable idle after working, provided no
focused real tmux client is viewing that pane. The first observation of every
pane is a silent baseline, unchanged states never repeat, and a notification
suppressed while focused is not queued for later. The existing one-tick idle
debounce remains the completion boundary.

Focus is evaluated across all tmux clients. Control-mode clients are ignored.
With `focus-events on`, a pane is focused only when a selecting real client has
tmux's `focused` client flag. Without focus events, every pane selected by a
real client is conservatively treated as focused. A popup owns terminal input,
so the pane underneath it is not considered focused.

## Delivery

An attention tracker owns transition memory and emits a small event containing
the outcome, agent, subject, directory, pane ID, and tmux location. A separate
notifications module formats and sanitizes that event, reads the default-on
`@agents-mon-notifications` option, and delegates to platform adapters.

macOS delivers through a bundled helper app, `AgentsMon.app`, assembled and
ad-hoc signed by `scripts/install-app.sh` (`make install-app`) into
`~/Applications` — a registered location, since temporary directories cannot
register notification permission on macOS 26. The helper binary
(`agents-mon-notifier`, built from this crate with `mac-usernotifications`)
owns the notification permission, is background-only (`LSUIElement`), posts
through `UNUserNotificationCenter` with the built-in `Glass` sound, and
detaches itself so the sidebar never blocks. The detached instance keeps the
main run loop alive awaiting the body click for up to 24 hours, then closes
the notification and exits.

The click command invokes the internal `notification-open` command with
shell-quoted executable, socket, pane, and terminal bundle arguments. That
command revalidates the pane, chooses the most recently active real client,
activates the terminal first, and then performs the exact-pane navigation
sequence (activation must precede the jump). Stale targets are silent no-ops.
Install-time `--setup` waits for the user's permission answer and reports it.
Denial is respected: an installed helper is authoritative, so a denied
permission means silence — the AppleScript path never runs as an end-run.
Without the installed app a fixed AppleScript whose title and body arrive only
as arguments displays the notification (display-only, `Glass` sound). Linux
uses `notify-send` when a graphical session is available and is display-only.
Other platforms degrade silently. Failures are best effort and visible only
through `AGENTS_MON_DEBUG`.

No Homebrew or other runtime dependency, OSC notification, tmux status
message, or Bash fallback implementation is introduced. Closing the sidebar or
popup therefore stops notification monitoring.

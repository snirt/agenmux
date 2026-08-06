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

macOS tries `terminal-notifier`, then a fixed AppleScript whose title and body
arrive only as arguments. Linux uses `notify-send` when a graphical session is
available. Failures are best effort and visible only through
`AGENTS_MON_DEBUG`.

When `terminal-notifier` is available, its click command invokes an internal
binary command with shell-quoted executable, socket, pane, and terminal bundle
arguments. The helper revalidates the pane, chooses the most recently active
real client, performs the existing exact-pane navigation sequence, and then
activates the terminal. Stale targets are silent no-ops. AppleScript and Linux
notifications are display-only so delivery never leaves a waiting helper.

No additional daemon, Rust dependency, OSC notification, tmux status message,
or Bash fallback implementation is introduced. Closing the sidebar or popup
therefore stops notification monitoring.

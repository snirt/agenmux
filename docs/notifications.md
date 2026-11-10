# Desktop notifications

The Rust engine sends a native desktop notification when an agent finishes or
needs attention while its pane is not focused. The title identifies the agent
and outcome; the body includes the remembered subject, directory, and tmux
target when available. Existing blocked/idle agents are a silent baseline when
the monitor starts, and unchanged states do not repeat notifications.

Notifications are enabled by default. Disable them in tmux with:

```tmux
set -g @agenmux-notifications off
```

or, in tmux and Rex alike, in `config.toml`:

```toml
[behavior]
notifications = false
```

## Focus detection

For complete focus detection, including a pane selected in Ghostty, Kitty, or
another terminal while that application is in the background, enable tmux
focus events:

```tmux
set -g focus-events on
```

The [tmux manual](https://man.openbsd.org/tmux.1#focus-events) notes that clients
may need to detach and attach again after this option changes. With focus events
off, agenmux conservatively suppresses a notification whenever any real tmux
client has the pane selected. With them on, it suppresses only when at least one
real client both selects the pane and reports itself focused; control-mode
clients are ignored.

In Rex there is nothing to enable: a notification is suppressed while the
agent's block is focused in the session the Rex app shows.

## macOS

On macOS, agenmux sends notifications natively through
`UNUserNotificationCenter` via a small helper app built from this repo — no
Homebrew or other runtime dependency, and no setup: installing or updating
the plugin automatically places a signed, background-only `Agenmux.app`
into `~/Applications` (skipped while notifications are off).
macOS asks for permission with the first notification; allow **agenmux**
when prompted, or later under System Settings → Notifications → agenmux.
Denying keeps notifications fully silent — there is no fallback around your
choice. Plugin updates refresh the app automatically and the permission
survives.

## Platform implementation and edge cases

To set up (or verify) permission right now instead of on first use:

```sh
make install-app
```

This assembles and installs the app, shows the permission prompt, waits for
your answer, and confirms with a test notification — or tells you
notifications are off and where to enable them.

Clicking a notification body activates your terminal (Ghostty, Kitty, iTerm2,
WezTerm, Apple Terminal, and Alacritty are recognized) and jumps the most
recently active real tmux client to the exact pane. Panes that no longer exist
are safe no-ops. In Rex, a click brings the Rex app forward and focuses the
agent's block. Notifications play macOS's built-in `Glass` alert sound.

Without the installed app, agenmux falls back to the built-in `osascript`,
which displays notifications with the `Glass` sound but cannot handle clicks.

Each notification keeps a small helper process waiting for its click; after 24
hours the notification is closed and the helper exits, so clicks on older
entries do nothing. If several notifications are pending at once, macOS may
route a click to the newest helper only — the click is then ignored rather
than jumping to the wrong pane.

On Linux, agenmux uses the optional `notify-send` command when a `DISPLAY`
or `WAYLAND_DISPLAY` session is available; Linux notifications are
display-only. Without it, delivery is silently skipped—the rest of the plugin
has no additional runtime requirement. Delivery is best effort and never
interrupts the sidebar if a notifier is unavailable or permission is denied.
The operating system may require notification permission for the sender it
displays.

The sidebar or popup must remain open while the state transition occurs because
notifications use the existing monitor process; no extra daemon is installed.
A transition suppressed while focused is not delivered later merely because
focus moves away.

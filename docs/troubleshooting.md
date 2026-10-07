# Troubleshooting

The sidebar daemon keeps its diagnostics in `$XDG_STATE_HOME/agenmux/daemon.log`
(`~/.local/state/agenmux/daemon.log` when the variable is unset). The file is
owner-only and starts over on every sidebar launch and past 256 KiB. The
generation before it is kept as `daemon.log.1`; anything older is removed, and
a daemon that wrote nothing removes the `.1` as well. Look there first when the sidebar
disappears or a configuration reload does nothing: every `agenmux: ...`
message the daemon would have printed is in it.

For detection bugs and timing questions, enable the trace. It is off by default
and only costs anything when enabled:

```tmux
set -g @agenmux-debug /tmp/agenmux-trace.log   # then reopen the sidebar
```

`AGENMUX_DEBUG=<file>` does the same for direct commands (`agenmux scan`) and
wins over the option when both are set. Each line carries the writing process,
a UTC clock, and the number of the scan it belongs to:

```text
[4242] 12:34:56.789 s17 # scan 23ms reason=output captured=1 reused=3
[4242] 12:34:56.790 s17 # state %5 claude working->idle shown=working ticks=1
[4242] 12:34:56.802 s17 3ms capture-pane -b 'agenmux-4242' -t '%5' -> ok 0B ""
```

Per-scan lines record the trigger, duration, and how many screens were
captured versus reused; per-command lines record every tmux round trip; `state`
lines record each pane's detected state, what the sidebar shows, and the
debounce tick that held a transition back. Release builds never write captured
screen text to the trace. Debug builds (`make dev-use`) add `detect` lines with the
pane title and the last two screen lines, which is what tuning an
`agents/*.conf` rule needs. Review and sanitize either file before attaching it
to an issue: paths, session names, and titles come from your own panes.

## Auto-update

The version picker (`u`) shows what auto-update is doing: a prepared release
(`v0.7.1 ready · next start`), why it skips this install, or the last failure.

- **Ready but never applied:** a release switches only when no agenmux view
  from this install is running on any tmux server. Close every sidebar and
  popup (including on other tmux servers), then open one.
- **Run a check now:** `target/release/agenmux internal auto-update` from the
  plugin directory (the daily throttle still applies; it prints the outcome).
- **A release failed to start:** the previous release was restored and kept.
  That release is not retried until a newer one appears; turning auto-update
  off and on in settings retries it.
- **Start over:** with agenmux closed, delete
  `<plugins dir>/.agenmux-state/<plugin dir name>/`. An interrupted switch is
  recovered from that directory on the next start, so delete it only after a
  start has opened normally.

## Known limits

- After a tmux server restart through a session-restore tool, restored sidebar
  panes may return as idle shells. Press `prefix+A` to remove them and reopen
  the sidebar. The original per-window layout cannot be recovered.
- State is inferred from what's on screen; transient redraws can flicker
  (the sidebar debounces transitions to idle by one tick).
- Pane titles are only used when the agent's OSC title escapes reach tmux.
- Desktop notifications are local to the tmux host; headless and remote hosts
  without a desktop notification service silently skip delivery.
- No Windows support.

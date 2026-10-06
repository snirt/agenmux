# Isolate runtime files by tmux server

## Reproduction

Two Docker tmux servers sharing a temporary directory each start Agenmux. The second daemon replaces `/tmp/agenmux-keys`; the first daemon retains an open descriptor to `/tmp/agenmux-keys (deleted)` and no longer receives its keys.

## Plan

1. Derive a private runtime directory from the tmux socket when no explicit runtime override or published tmux option exists. Create it before starting the daemon and pane readers.
2. Put daemon keys, row/cache files, pane frame FIFOs, and popup pin files in that directory. Preserve explicit runtime overrides used by isolated tests.
3. Add a two-server integration regression that shares `TMPDIR`, verifies distinct runtime directories, and sends keys through the public CLI after both daemons start.
4. Run the focused and full project tests inside Docker, then repeat the two-server live reproduction there.

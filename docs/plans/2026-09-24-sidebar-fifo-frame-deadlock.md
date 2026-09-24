# Issue 132: large sidebar frames deadlock the FIFO

## Cause

The daemon writes a whole frame through a blocking FIFO. On macOS, a write
larger than the 8 KiB FIFO buffer can sleep without delivering any bytes, while
the sidebar pane waits for readable data. The single daemon loop then stops
handling input and updates.

## Plan

1. Confirm the write and read paths and preserve the current branch state.
2. Limit each blocking FIFO write to 4 KiB in `PaneWriter::emit`, matching the
   issue's validated fix.
3. Add a real FIFO regression test: emit a frame larger than 8 KiB, read it
   through `poll`, and bound the wait so a deadlock fails promptly.
4. Run the focused test and the project's test suite. Review the exact diff,
   repository status, and added content for sensitive data.

## Scope decision

Use the issue's validated 4 KiB fix. The proposed nonblocking event loop would
also address a slow or stalled reader, but requires a broader delivery change.

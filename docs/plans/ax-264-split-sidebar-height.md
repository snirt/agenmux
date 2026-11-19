# Issue #264 · Fit split sidebars to their panes

1. Reproduce with real tmux clients in separate sessions and sidebar heights of 49 and 24 rows. Check the heading and viewport of each pane, then switch one client between the two window sizes.
2. Track each visible sidebar's measured height through writer reconciliation and layout updates. Render each distinct height using the existing list and overlay renderers, preserving shared navigation, width handling, and popup behavior.
3. Send each pane its own fitted frame and publish the matching click map. Preserve atomic row-map updates and bounded FIFO writes.
4. Run the live regression on tmux 3.8, `cargo test --locked`, `tests/run.sh`, and formatting and diff checks. Review the exact changes for scope and private data.

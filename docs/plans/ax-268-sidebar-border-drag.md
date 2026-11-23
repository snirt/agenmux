# Keep sidebar navigation active after a tmux border drag

1. Reproduce the issue with SGR mouse input through an attached tmux client. Check that the border moves, focus stays on the sidebar, and subsequent navigation moves the selection.
2. Preserve tmux's root border-drag binding when inheriting it into sidebar key tables. Restore the originating sidebar table for the client whose focused pane is still the sidebar.
3. Run the navigation regression, Rust suite, and shell suite.

The live configuration also uses inherited Alt-h and Alt-l bindings for keyboard resizing. Reproduce that path with the configured conditional bindings and verify that each resize keeps the sidebar's key table and selection controls active.

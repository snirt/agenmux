# Theme-aware status animations

The first implementation added the working shine, finished fade, and Display animations switch. It cached visible animated rows for fast text ticks and sent full frames on spinner and blink ticks.

1. Keep the existing theme blending, shared shine position, and animation cadence. Use the visible row cache to repaint changes on both the text and status tick paths.
2. Use full frames for layout changes and when the pane size or frame contents make row updates unsafe. Keep the click map in step with full redraws.
3. Check the actual bytes delivered to a split pane on a spinner tick, a text tick, and a geometry mismatch. Run the Rust and shell suites and review the branch diff.

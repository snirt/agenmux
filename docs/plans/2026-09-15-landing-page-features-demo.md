# Landing page: latest features and demo (#90)

Site-focused change under `site/`, plus removal of the unsafe raw recording link
from the README; no Rust, no version bump. `docs-skip.yml` satisfies the required
CI contexts and `pages.yml` deploys on master.

## Decisions

- **The recording is omitted** because the available capture contains private
  identifiers, local paths and agent-session content. The interactive mock is
  the safe demo until a recording made entirely with neutral data is available;
  the README no longer links the raw capture.
- **The interactive mock follows `src/sidebar/render.rs`**, not the older
  recording: header is `app_title()` (`agenmux v<tag>`, read from the baked
  `#version` so `scripts/site-release.py` keeps it current) on the xterm-236
  bar; agent rows show the cwd basename; the selected cursor keeps its state
  color. The all-pane tree uses hidden `.prow` rows (never `.entry`, so the
  status segment and search filter ignore them) drawn like
  `inventory_lines()`: accent window headers, done-colored `▢ command` pane
  rows, agents in a split window indented 2ch, a muted `❯` and 236 background
  for a selected ordinary pane. `▣` stands in for the Nerd Font window glyph.
  `dd` shares the cursor row with the accent `delete pane? y/N` prompt and the
  hint row shows `y delete · any other key cancels`, like the inline overlay.
- **Features are a static grid** (`#features`), not extra carousel slides: the
  how-it-works carousel shows one trait at a time on an 8 s rotation, so new
  capabilities parked there would be missed.
- Copy says agenmux is also a tmux navigator/manager (title, meta description
  and hero headline) because that is now a first-class use, not only monitoring.
- **Installation follows Pi's compact method picker:** the real one-line
  installer is selected by default, manual setup is one keyboard-selectable tab
  away, and both reuse the site's code-block copy control.

## Verified

- `scripts/site-release.py --release fixture` on a copy: all four anchors
  rewritten, second run is a no-op.
- `html.parser` walk: tags balanced.
- Local preview returns HTTP 200 and renders the revised hero, interactive
  navigation demo and feature grid in Brave.
- Browser automation at 375 px reports no document overflow; the install tabs
  switch by pointer and arrow key, and copy writes the exact visible command.

## Skipped

- Sanitized recording; add one only after recording with neutral data.
- A CI check for the site; none exists and `docs-skip.yml` no-ops site PRs.

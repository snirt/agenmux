# Adding / overriding agents

Drop a `.conf` in `~/.config/agenmux/agents/`. A file with the same name
as a built-in (see `agents/`) overrides only the keys it assigns; every other
key keeps its built-in value. Assigning an empty value (`KEY=""`) clears that
key, so a one-line file changes or removes a built-in icon:

```bash
# ~/.config/agenmux/agents/claude.conf
AGENT_ICON="✻"    # or AGENT_ICON="" for the name only
```

A new file name adds a custom agent. Example:

```bash
# ~/.config/agenmux/agents/aider.conf
AGENT_BINS="aider"                 # process names that identify the agent
AGENT_PATH_HINTS=""                # optional: substring of a wrapped script path
BLOCKED_TITLE=''                   # grep -Ei pattern against #{pane_title}
BLOCKED_SCREEN='\(Y\)es/\(N\)o'    # grep -Ei pattern against the pane's bottom SCREEN_LINES lines
WORKING_TITLE=''
WORKING_SCREEN='esc to interrupt'
IDLE_SCREEN=''                     # explicit idle marker (rarely needed)
SCREEN_LINES=20                    # bottom screen lines the *_SCREEN rules see
CHECK_ORDER="bt wt bs ws"          # rule order; first hit wins, fallback is idle
TITLE_STRIP='^aider: '              # optional regex removed from the pane title
SUBJECT_SCREEN=''                   # optional sed -E capture used as the subject line
SUBJECT_CMD=''                      # optional shell snippet used as a final subject fallback
AGENT_ICON=""                       # optional glyph or short string shown before the agent name
AGENT_RESUME=''                     # optional command typed into restored panes (tmux_management.resume_agents)
```

`AGENT_ICON` width is counted per character, like other sidebar text: Nerd Font
glyphs fit, but double-width characters such as emoji or CJK may misalign rows.

`CHECK_ORDER` tokens: `bt`/`bs` blocked title/screen, `wt`/`ws` working
title/screen, `is` idle screen. Order matters when states can look alike —
Claude Code checks working before blocked so an already-answered permission
prompt left on screen doesn't read as blocked.

The sidebar subject shown below an agent is resolved from the cleaned pane
title, then `SUBJECT_SCREEN`, then `SUBJECT_CMD`. The shell snippet can use
`$path`, the pane's working directory. The Rust engine parses these assignments
and executes `SUBJECT_CMD` through the shell when needed, so only install configs
you trust.

Scans wait for `SUBJECT_CMD`, so keep it fast. A call still running after
1 second is killed with its process group and the subject stays empty until
the agent's next prompt. Each scan spends at most about 2 seconds on these
commands; panes past that budget get their subject on a later scan.

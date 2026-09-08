# `agenmux config` output

How the four `config` subcommands should print. Written against the behaviour
on `AX-36-app-configuration`; `--help` already matches, the other three do not.

## What is wrong today

`check` prints `valid (missing file uses defaults)` whether or not a file
exists, so it claims the file is missing while reading it. `check --effective`
prints forty-odd alphabetical `field: source` lines with no values, burying the
two settings the user actually changed among rows that say `default`. `reload`
says `open views pick this up within a moment` without saying what changed.

## Principles

1. **stdout is the answer, stderr is the diagnostic.** Exit codes stay as they
   are: 0 fine, 1 could not read, 2 invalid content. The installer's
   `notification-eligible` contract (3 = disabled) is untouched.
2. **Say what is, never what might be.** No sentence should be printed on a
   path where it is false.
3. **Lead with what differs.** Defaults are the uninteresting majority. Show
   the deltas, and put the exhaustive listing behind `--all`.
4. **A setting is a name, a value and a source.** Source alone does not answer
   "what is my sidebar width".
5. **Never echo file content.** Values that came from the file are printed
   only after they have parsed into a typed value, so a rejected value never
   reaches the terminal. Diagnostics keep the existing escaped, capped form.
6. **One column layout.** Aligned columns, 80 characters, no box drawing.

## `agenmux config check`

Validates the file alone. Never contacts tmux.

No file, which is the common case:

```
no configuration file; using defaults
looked for /home/u/.config/agenmux/config.toml
```

A valid file:

```
/home/u/.config/agenmux/config.toml: valid
3 settings differ from the defaults; agenmux config check --effective shows them
```

A valid file that sets nothing says `0 settings differ from the defaults` and
drops the second clause.

Invalid, on stderr, exit 2, unchanged from today:

```
agenmux: /home/u/.config/agenmux/config.toml: document (line 2, column 8): invalid configuration schema (field, type, or value)
```

## `agenmux config check --effective`

Resolves the file against live tmux options and prints the result. Read-only.

```
/home/u/.config/agenmux/config.toml

setting                value    source
display.mode           popup    file
display.sidebar_width  44       tmux @agenmux-width
display.popup_width    44       tmux @agenmux-width

36 settings are at their defaults; --all lists every one.
```

With `--all`, every setting appears, grouped by section in the order
`--help` documents rather than alphabetically, so the output reads like the
file it describes.

Source vocabulary, most specific first: `CLI`, `tmux @agenmux-<name>`,
`tmux @agents-mon-<name>` (legacy), `file`, `theme base`, `default`. Naming the
option makes the fix obvious when a tmux option is quietly winning.

Keys print as their chord list, `keys.normal.down  j, Down  default`. An
unbound action prints `(unbound)` as its value.

## `agenmux config reload`

Reports what it did:

```
reloaded /home/u/.config/agenmux/config.toml
2 settings differ from the defaults; key tables reinstalled
```

The key-table clause appears only when the tables were actually reinstalled.

This says "differ from the defaults", not "changed". A change count would need
to know what the running views held before, and nothing records that, so the
number would be invented. The honest fact reload can establish is how the file
resolves now.
A file that fails to validate prints nothing on stdout, the usual diagnostic on
stderr, exit 2, and leaves every running view untouched.

## `agenmux config --help`

Already correct. It stays the reference for names, values and defaults, and the
other three commands should not duplicate that content.

## Out of scope

No colour, no JSON, no `--quiet`. Add JSON only when something other than a
human needs to read this.

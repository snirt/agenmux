#!/bin/sh
# One-line installer:  curl -fsSL https://snirt.github.io/agenmux/install.sh | sh
# Clones (or fast-forwards) agenmux once into a multiplexer-neutral directory
# that tmux and Rex both load, shows the tmux.conf lines it wants to add
# (launcher keys, then the plugin line) and asks before writing, then reloads a
# running tmux so agenmux.tmux fetches the verified engine. Re-run to update.
# Override paths with AGENMUX_DIR and AGENMUX_TMUX_CONF. Without a terminal (CI, containers)
# every question takes its default: keys A and a, write yes, link the agenmux
# command into ~/.local/bin, and the agent label is left to config.toml unless
# AGENMUX_AGENT_LABEL names one.
#
# Rex: with the rex CLI installed, it also adds the init.lua line that loads
# agenmux (after asking) and reloads Rex. Rex alone needs no tmux.
#
# On a terminal a checklist shows each multiplexer's integration as it stands
# now; checking one adds it, unchecking one removes its lines, keys and open
# sidebars. AGENMUX_MUX=tmux, rex or both skips the checklist; without a
# terminal every installed multiplexer is integrated and nothing is removed.
set -eu

case "${XDG_DATA_HOME:-}" in
/*) DIR="$XDG_DATA_HOME/agenmux" ;;
*) DIR="$HOME/.local/share/agenmux" ;;
esac
# Installs from before multiplexer support live in tmux's plugin dir; they
# keep updating there so the tmux.conf line that loads them stays valid.
legacy="$HOME/.tmux/plugins/agenmux"
if [ ! -e "$DIR" ] && { [ -d "$legacy/.git" ] || [ -f "$legacy/.git" ]; }; then DIR="$legacy"; fi
DIR="${AGENMUX_DIR:-$DIR}"
REPO="${AGENMUX_REPO:-https://github.com/snirt/agenmux}"

# colour only on a terminal; piped output (CI, logs) stays plain
if [ -t 1 ] && [ "${TERM:-dumb}" != dumb ] && [ -z "${NO_COLOR:-}" ]; then
  bold="$(printf '\033[1m')" dim="$(printf '\033[2m')" green="$(printf '\033[32m')"
  red="$(printf '\033[31m')" yellow="$(printf '\033[33m')" reset="$(printf '\033[0m')"
else
  bold="" dim="" green="" red="" yellow="" reset=""
fi
# decided once here: inside $(...) stdout is a pipe, so the prompts cannot test it
if [ -t 1 ] && [ -r /dev/tty ] && [ -w /dev/tty ]; then interactive=1; else interactive=""; fi
tilde() { case "$1" in "$HOME"/*) printf '~%s' "${1#"$HOME"}" ;; *) printf '%s' "$1" ;; esac }
ok() { printf '  %s✓%s %-10s %s\n' "$green" "$reset" "$1" "$2"; }
skip() { printf '  %s-%s %-10s %s\n' "$dim" "$reset" "$1" "$2"; }
warn() { printf '  %s!%s %-10s %s\n' "$yellow" "$reset" "$1" "$2"; }
die() {
  printf '  %s✗%s %s\n' "$red" "$reset" "$1" >&2
  exit 1
}
# ask PROMPT DEFAULT(y|n): stdin is the script itself under `curl | sh`, so
# the answer comes from the terminal; no terminal on stdout means nobody is
# watching, so take the default.
ask() {
  [ -n "$interactive" ] || {
    [ "$2" = y ]
    return
  }
  if [ "$2" = y ]; then hint="[Y/n]"; else hint="[y/N]"; fi
  printf '  %s %s%s%s ' "$1" "$dim" "$hint" "$reset" >/dev/tty
  read -r answer </dev/tty || answer=""
  case "${answer:-$2}" in y | Y | yes | YES) return 0 ;; *) return 1 ;; esac
}
# ask_rex_key PROMPT DEFAULT: one Rex key, as rex.bind names it. macOS turns
# Option with e, i, n, u or ` into a dead key that never reaches Rex.
ask_rex_key() {
  [ -n "$interactive" ] || {
    printf '%s' "$2"
    return
  }
  while :; do
    printf '  %s %s[%s]%s ' "$1" "$dim" "$2" "$reset" >/dev/tty
    read -r answer </dev/tty || answer=""
    case "${answer:-$2}" in
    *[\ \'\"\\]*) printf '  one key, no spaces or quotes (e.g. alt+s, ctrl+g)\n' >/dev/tty ;;
    alt+[einu] | alt+\` | opt+[einu] | option+[einu]) printf '  macOS keeps %s from Rex (a dead key); pick another\n' "${answer:-$2}" >/dev/tty ;;
    *)
      printf '%s' "${answer:-$2}"
      return
      ;;
    esac
  done
}
# ask_key PROMPT DEFAULT: one tmux key name; empty or no terminal keeps DEFAULT
ask_key() {
  [ -n "$interactive" ] || {
    printf '%s' "$2"
    return
  }
  while :; do
    printf '  %s %s[%s]%s ' "$1" "$dim" "$2" "$reset" >/dev/tty
    read -r answer </dev/tty || answer=""
    case "${answer:-$2}" in
    *[\ \'\"]*) printf '  one key name, no spaces or quotes (e.g. A, C-g, F5)\n' >/dev/tty ;;
    *)
      printf '%s' "${answer:-$2}"
      return
      ;;
    esac
  done
}

# Everything runs from main, called on the last line: under curl | sh a
# dropped connection then runs nothing instead of half a script.
main() {
  # site/logo.png rendered as braille (48 columns): green agen, faded mu, >< chevrons
  printf '\n'
  printf '%s  ⣴⠶⠶⣦⡀ ⣠⡶⠶⢶⡶ ⢠⡶⠶⢶⣄ ⣠⡶⠶⣦ ⢠⡶⠶%s⣦⣴⠶⢶⡀⢰⡆  ⢰⡆%s⠰⣦⡀ %s ⣠⡶%s\n' "$green" "$dim" "$green" "$dim" "$reset"
  printf '%s ⢸⡇  ⢸⣷⠰⣿   ⣿ ⣿⠶⠶⠶⠿ ⣿  ⢸⡇⣿⡇ %s⢸⡇ ⢸⡇⢸⡇  ⣸⡇%s ⢈⣿⠆%s⢸⣏%s\n' "$green" "$dim" "$green" "$dim" "$reset"
  printf '%s ⠈⠻⠶⠶⠿⠟ ⠙⠷⠶⠾⠃ ⠘⠷⠶⠶⠃ ⠿  ⠸⠇⠿⠃ %s⠸⠇ ⠸⠇ ⠻⠶⠶⠟ %s⠰⠟⠁ %s ⠙⠷%s\n' "$green" "$dim" "$green" "$dim" "$reset"
  printf '%s        ⠿⣤⣤⣴⠟%s\n' "$green" "$reset"
  printf '\n  %s⣿ multiplexer sidebar for AI coding agents%s\n\n' "$dim" "$reset"
  # Only the user's eyes can tell whether the terminal renders Nerd Font
  # glyphs, so the answer picks the agent row label.
  label="${AGENMUX_AGENT_LABEL:-}"
  case "$label" in "" | icon | icon-text | text) ;; *) die "AGENMUX_AGENT_LABEL must be icon, icon-text or text" ;; esac
  if [ -n "$interactive" ]; then
    printf '  %sFont check:%s ⣿  ⠹    ▢\n' "$bold" "$reset"
    printf '  If  is a box or blank, configure a Nerd Font in your terminal.\n'
    if [ -z "$label" ]; then
      if ask "Does  show as an icon?" y; then label=icon; else label=text; fi
    fi
    printf '\n'
  fi

  # The rex CLI the way the engine finds it: actions see a bare PATH, so the
  # init.lua line names it by full path too.
  rex_cli=""
  for cli in "${AGENMUX_REX:-}" "${REX_BIN_DIR:+$REX_BIN_DIR/rex}" "$(command -v rex 2>/dev/null || true)" \
    "$HOME/Library/Application Support/com.superlogical.rex/RexServer/Versions/Current/Rex Server.app/Contents/MacOS/rex"; do
    if [ -n "$cli" ] && [ -x "$cli" ]; then
      rex_cli="$cli"
      break
    fi
  done
  has_tmux="" has_rex="" chosen=""
  if command -v tmux >/dev/null 2>&1; then has_tmux=1; fi
  if [ -n "$rex_cli" ]; then has_rex=1; fi
  tmux_conf_path
  rex_conf_path
  # integrated now: a live line loads agenmux
  tmux_on="" rex_on=""
  if [ -f "$CONF" ] && grep -v "^[[:space:]]*#" "$CONF" | grep -q agenmux; then tmux_on=1; fi
  if [ -f "$rex_conf" ] && grep -v '^[[:space:]]*--' "$rex_conf" | grep -q 'rex/agenmux\.lua'; then rex_on=1; fi
  case "${AGENMUX_MUX:-}" in
  tmux) want_tmux=1 want_rex="" ;;
  rex) want_tmux="" want_rex=1 ;;
  both) want_tmux=1 want_rex=1 ;;
  "")
    [ -n "$has_tmux$has_rex" ] || die "tmux or Rex is required"
    want_tmux="$has_tmux" want_rex="$has_rex"
    if [ -n "$interactive" ]; then
      # a re-run starts from what is integrated; a first run from what is installed
      if [ -n "$tmux_on$rex_on" ]; then
        want_tmux="${has_tmux:+$tmux_on}" want_rex="${has_rex:+$rex_on}"
      fi
      checklist
      chosen=1
    fi
    ;;
  *) die "AGENMUX_MUX must be tmux, rex or both" ;;
  esac
  [ -z "$want_rex" ] || [ -n "$rex_cli" ] || die "rex is required (install the Rex app, or set AGENMUX_REX)"
  for cmd in git bash ${want_tmux:+tmux}; do
    command -v "$cmd" >/dev/null 2>&1 || die "$cmd is required"
  done

  # the installer runs inside tmux more often than not; follow that server's
  # socket so a custom -L/-S session still gets reloaded
  tmux() {
    if [ -n "${TMUX:-}" ]; then command tmux -S "${TMUX%%,*}" "$@"; else command tmux "$@"; fi
  }
  version() { bash "$DIR/scripts/version.sh" tag 2>/dev/null || printf 'unknown'; }
  # conf_has PATTERN: a live (uncommented) tmux.conf line matches
  conf_has() { grep -v '^[[:space:]]*#' "$CONF" | grep -q "$1"; }

  # Linked worktrees use a .git file instead of a .git directory. In Docker the
  # file may point outside the mounted checkout, but skip-update users still have
  # all the plugin files they need and must not trigger a clone into that tree.
  # Standard installs sit on the latest published stable tag so auto-update can
  # follow releases; AGENMUX_REF=<branch|tag> picks a development ref instead.
  latest_release() {
    url="$(curl -fsSL --max-time 20 -o /dev/null -w '%{url_effective}' "$REPO/releases/latest" 2>/dev/null)" || return 1
    tag="${url##*/}"
    case "$tag" in v[0-9]*-* | *[!A-Za-z0-9.-]*) return 1 ;; v[0-9]*) printf '%s' "$tag" ;; *) return 1 ;; esac
  }
  checkout() { git -C "$DIR" checkout --quiet "$1" </dev/null || die "git checkout $1 failed in $(tilde "$DIR")"; }
  if [ -d "$DIR/.git" ] || [ -f "$DIR/.git" ]; then
    before="$(version)"
    if [ "${AGENMUX_SKIP_UPDATE:-}" != 1 ]; then
      on_tag="$(git -C "$DIR" describe --tags --exact-match 2>/dev/null || true)"
      if [ -n "${AGENMUX_REF:-}" ]; then
        git -C "$DIR" fetch --quiet --tags origin </dev/null || die "git fetch failed in $(tilde "$DIR")"
        checkout "$AGENMUX_REF"
        if git -C "$DIR" symbolic-ref -q HEAD >/dev/null; then
          git -C "$DIR" pull --ff-only --quiet </dev/null || die "git pull failed in $(tilde "$DIR")"
        fi
      elif [ "$on_tag" = "$before" ] && ! git -C "$DIR" symbolic-ref -q HEAD >/dev/null; then
        # A detached release checkout follows newer stable tags, never older.
        git -C "$DIR" fetch --quiet --tags origin </dev/null || die "git fetch failed in $(tilde "$DIR")"
        if tag="$(latest_release)" &&
          [ "$(printf '%s\n%s\n' "$before" "$tag" | sort -V | tail -n 1)" = "$tag" ]; then
          checkout "$tag"
        fi
      else
        git -C "$DIR" pull --ff-only --quiet </dev/null || die "git pull failed in $(tilde "$DIR")"
      fi
    fi
    after="$(version)"
    if [ "$before" = "$after" ]; then
      ok plugin "$after already current in $(tilde "$DIR")"
    else
      ok plugin "updated $before → $after in $(tilde "$DIR")"
    fi
  else
    git clone --quiet "$REPO" "$DIR" </dev/null || die "git clone failed"
    if [ -n "${AGENMUX_REF:-}" ]; then
      checkout "$AGENMUX_REF"
    elif tag="$(latest_release)"; then
      checkout "$tag"
    else
      warn release "latest release unknown; kept the default branch"
    fi
    ok plugin "cloned $(version) to $(tilde "$DIR")"
  fi

  # same root the engine resolves: XDG_CONFIG_HOME when absolute, else ~/.config
  case "${XDG_CONFIG_HOME:-}" in
  /*) CFG="$XDG_CONFIG_HOME/agenmux" ;;
  *) CFG="$HOME/.config/agenmux" ;;
  esac
  mkdir -p "$CFG/agents" || die "cannot create $(tilde "$CFG")"
  ok config "$(tilde "$CFG")/ (config.toml optional, agents/ for overrides)"
  if [ -n "$label" ]; then
    app="$CFG/config.toml"
    # Line-based TOML edit; an inline `display = { ... }` table is
    # not recognised and would end up with a duplicate [display].
    if [ -f "$app" ] && grep -q '^[[:space:]]*\(display\.\)\{0,1\}agent_label[[:space:]]*=' "$app"; then
      skip labels "agent_label already set in $(tilde "$app")"
    else
      if [ -f "$app" ] && grep -q '^[[:space:]]*\[display\][[:space:]]*$' "$app"; then
        tmp="$app.agenmux.tmp"
        { cp -p "$app" "$tmp" &&
          label="$label" awk '{ print } /^[[:space:]]*\[display\][[:space:]]*$/ && !done { print "agent_label = \"" ENVIRON["label"] "\""; done = 1 }' \
            "$app" >"$tmp" && mv "$tmp" "$app"; } || {
          rm -f "$tmp"
          die "could not edit $(tilde "$app")"
        }
      else
        { [ -s "$app" ] && printf '\n'; printf '[display]\nagent_label = "%s"\n' "$label"; } >>"$app" ||
          die "could not write $(tilde "$app")"
      fi
      ok labels "agent_label = \"$label\" in $(tilde "$app")"
    fi
  fi

  # A symlink, not a copy, so engine updates reach the shell command. It may
  # dangle until the engine lands; anything already at the path is left alone.
  cmd_link="$HOME/.local/bin/agenmux"
  engine="$DIR/target/release/agenmux"
  if [ -L "$cmd_link" ] && [ "$(readlink "$cmd_link")" = "$engine" ]; then
    skip command "$(tilde "$cmd_link") already links the engine"
  elif [ -e "$cmd_link" ] || [ -L "$cmd_link" ]; then
    skip command "$(tilde "$cmd_link") exists, left alone"
  elif ask "Link the agenmux command into $(tilde "$HOME/.local/bin")?" y; then
    { mkdir -p "$HOME/.local/bin" && ln -s "$engine" "$cmd_link"; } || die "cannot link $(tilde "$cmd_link")"
    ok command "linked $(tilde "$cmd_link")"
    case ":$PATH:" in
    *":$HOME/.local/bin:"*) ;;
    *)
      warn PATH "~/.local/bin is not on PATH; add to your shell rc:"
      # shellcheck disable=SC2016 # printed for the user's rc, not expanded here
      printf '                 export PATH="$HOME/.local/bin:$PATH"\n'
      ;;
    esac
  else
    skip command "not linked"
  fi

  tmux_next="" rex_next=""
  if [ -n "$want_tmux" ]; then
    if setup_tmux; then tmux_next=1; fi
  elif [ -n "$chosen" ] && [ -n "$tmux_on" ]; then
    remove_tmux
  fi
  if [ -n "$want_rex" ]; then
    if setup_rex; then rex_next=1; fi
  elif [ -n "$chosen" ] && [ -n "$rex_on" ]; then
    remove_rex
  fi

  if [ -n "$tmux_next" ]; then
    printf '\n  Next: inside tmux press %sprefix + %s%s for the sidebar, %sprefix + %s%s for a popup.\n' \
      "$bold" "${sidebar_key:-A}" "$reset" "$bold" "${popup_key:-e}" "$reset"
  fi
  if [ -n "$rex_next" ]; then
    printf '\n  Next: in Rex press %s%s%s for the sidebar, %s%s%s for a popup.\n' \
      "$bold" "$rex_key" "$reset" "$bold" "$rex_popup_key" "$reset"
  fi
  printf '  %sStatus-bar summary, keys, width, notifications: https://github.com/snirt/agenmux#usage%s\n' "$dim" "$reset"
  printf '  %sApp config and agent overrides live in %s/%s\n\n' "$dim" "$(tilde "$CFG")" "$reset"
}

rex_conf_path() {
  case "${REX_CONFIG:-}" in
  /*) rex_conf="$REX_CONFIG" ;;
  *)
    case "${XDG_CONFIG_HOME:-}" in
    /*) rex_conf="$XDG_CONFIG_HOME/rex/init.lua" ;;
    *) rex_conf="$HOME/.config/rex/init.lua" ;;
    esac
    ;;
  esac
}

tmux_conf_path() {
  if [ -n "${AGENMUX_TMUX_CONF:-}" ]; then
    CONF="$AGENMUX_TMUX_CONF"
  elif [ -f "$HOME/.tmux.conf" ]; then
    CONF="$HOME/.tmux.conf"
  elif [ -f "${XDG_CONFIG_HOME:-$HOME/.config}/tmux/tmux.conf" ]; then
    CONF="${XDG_CONFIG_HOME:-$HOME/.config}/tmux/tmux.conf"
  else
    CONF="$HOME/.tmux.conf"
  fi
}

# checklist: arrows (or j/k) move, space toggles, enter applies. Starts from
# want_tmux/want_rex; a multiplexer that is not installed cannot be checked.
checklist() {
  cur=1 esc="$(printf '\033')" saved="$(stty -g </dev/tty)"
  trap 'stty "$saved" </dev/tty; exit 130' INT TERM
  stty -icanon -echo min 1 time 0 </dev/tty
  printf '  %sIntegrations%s %s(↑/↓ move, space toggles, enter applies)%s\n' "$bold" "$reset" "$dim" "$reset" >/dev/tty
  drawn=""
  while :; do
    [ -z "$drawn" ] || printf '\033[2A' >/dev/tty
    drawn=1
    row 1 tmux "$has_tmux" "$want_tmux" "$(tilde "$CONF")"
    row 2 Rex "$has_rex" "$want_rex" "$(tilde "$rex_conf")"
    key="$(dd bs=1 count=1 </dev/tty 2>/dev/null)"
    case "$key" in
    "$esc")
      case "$(dd bs=1 count=2 </dev/tty 2>/dev/null)" in
      "[A") cur=1 ;;
      "[B") cur=2 ;;
      esac
      ;;
    k) cur=1 ;;
    j) cur=2 ;;
    " " | x)
      if [ "$cur" = 1 ] && [ -n "$has_tmux" ]; then
        if [ -n "$want_tmux" ]; then want_tmux=""; else want_tmux=1; fi
      elif [ "$cur" = 2 ] && [ -n "$has_rex" ]; then
        if [ -n "$want_rex" ]; then want_rex=""; else want_rex=1; fi
      fi
      ;;
    "") break ;;
    esac
  done
  stty "$saved" </dev/tty
  trap - INT TERM
  printf '\n'
}

# row N NAME INSTALLED CHECKED FILE: one checklist line, redrawn in place
row() {
  if [ "$cur" = "$1" ]; then mark="❯"; else mark=" "; fi
  if [ -z "$3" ]; then
    box="$dim[-]" note="not installed"
  elif [ -n "$4" ]; then
    box="$green[x]" note="$5"
  else
    box="[ ]" note="$5"
  fi
  printf '\r\033[K  %s %s%s %-5s %s%s%s\n' "$mark" "$box" "$reset" "$2" "$dim" "$note" "$reset" >/dev/tty
}

# drop_lines FILE PATTERN COMMENT: FILE without its live (not COMMENT-led)
# lines matching PATTERN. A dotfiles symlink stays a symlink: its target is
# rewritten, atomically and with its mode.
drop_lines() {
  target="$1"
  # readlink without -f: macOS before 12.3 lacks it
  while [ -L "$target" ]; do
    link="$(readlink "$target")"
    case "$link" in /*) target="$link" ;; *) target="$(dirname "$target")/$link" ;; esac
  done
  tmp="$target.agenmux.tmp"
  { cp -p "$target" "$tmp" &&
    pattern="$2" comment="$3" awk '$0 !~ "^[[:space:]]*" ENVIRON["comment"] && $0 ~ ENVIRON["pattern"] { next } { print }' \
      "$target" >"$tmp" && mv "$tmp" "$target"; } || {
    rm -f "$tmp"
    die "could not edit $(tilde "$target")"
  }
}

# Removes the tmux.conf lines that load agenmux (after asking), then unbinds
# its keys and closes its sidebars in a running tmux.
remove_tmux() {
  printf '\n  Lines to remove from %s%s%s:\n\n' "$bold" "$(tilde "$CONF")" "$reset"
  grep -v '^[[:space:]]*#' "$CONF" | grep agenmux | sed 's/^/      /'
  printf '\n'
  if ! ask "Remove them?" y; then
    skip tmux.conf "left untouched"
    return
  fi
  drop_lines "$CONF" agenmux '#'
  ok tmux.conf "removed agenmux from $(tilde "$CONF")"
  tmux list-sessions >/dev/null 2>&1 || return 0
  bin="$(tmux show-option -gqv @agenmux-bin)"
  tmux list-keys | awk '/agenmux/ { for (i = 1; i < NF; i++) if ($i == "-T") { print $(i + 1), $(i + 2); break } }' |
    while read -r table key; do tmux unbind-key -T "$table" "$key" || true; done
  AGENMUX_MUX=tmux "${bin:-$DIR/target/release/agenmux}" teardown >/dev/null 2>&1 || true
  for option in @agenmux-key @agenmux-popup-key @agenmux-plugin-dir @agenmux-bin; do
    tmux set-option -gu "$option" || true
  done
  ok tmux "keys unbound, sidebars closed"
}

# Removes the init.lua line that loads agenmux (after asking), reloads Rex and
# closes its sidebars.
remove_rex() {
  line="$(grep -v '^[[:space:]]*--' "$rex_conf" | grep 'rex/agenmux\.lua')"
  printf '\n  Line to remove from %s%s%s:\n\n' "$bold" "$(tilde "$rex_conf")" "$reset"
  printf '%s\n' "$line" | sed 's/^/      /'
  printf '\n'
  if ! ask "Remove it?" y; then
    skip init.lua "left untouched"
    return
  fi
  drop_lines "$rex_conf" 'rex/agenmux[.]lua' '--'
  ok init.lua "removed agenmux from $(tilde "$rex_conf")"
  # the binary that line loaded closes the sidebars it opened
  bin="$(printf '%s\n' "$line" | sed -n 's/.*bin *= *"\([^"]*\)".*/\1/p' | head -n 1)"
  if "$rex_cli" --autostart=false config reload >/dev/null 2>&1; then
    AGENMUX_MUX=rex AGENMUX_REX="$rex_cli" "${bin:-$DIR/target/release/agenmux}" teardown >/dev/null 2>&1 || true
    ok rex "reloaded, sidebars closed"
  fi
}

# Adds the line that loads agenmux to init.lua (after asking) and reloads Rex.
# Fails when the user keeps init.lua as it is.
setup_rex() {
  # Without tmux nothing fetches the engine on reload: install it here.
  if [ -z "$want_tmux" ] && [ ! -x "$DIR/target/release/agenmux" ]; then
    if bash "$DIR/scripts/install-bin.sh" </dev/null >/dev/null 2>&1; then
      ok engine "installed $(version)"
    else
      skip engine "not installed; run: bash $(tilde "$DIR")/scripts/install-bin.sh"
    fi
  fi
  rex_key=alt+s rex_popup_key=alt+p
  if [ -z "$rex_on" ]; then
    printf '\n  Rex keys:\n'
    rex_key="$(ask_rex_key "Sidebar toggle" alt+s)"
    rex_popup_key="$(ask_rex_key "Popup toggle" alt+p)"
  fi
  [ "$rex_key" != "$rex_popup_key" ] || die "sidebar and popup need different keys"
  # Lua strings: backslashes and double quotes escaped
  lua() { printf '"%s"' "$(printf '%s' "$1" | sed 's/[\\"]/\\&/g')"; }
  line="dofile($(lua "$DIR/rex/agenmux.lua")){ bin = $(lua "$DIR/target/release/agenmux"), key = $(lua "$rex_key"), popup_key = $(lua "$rex_popup_key") }"
  if [ -n "$rex_on" ]; then
    skip init.lua "already loads agenmux; to change keys, edit $(tilde "$rex_conf"):"
    printf '\n      %s\n\n' "$line"
    ask "Reload Rex now?" y || return 0
  else
    printf '\n  Line for %s%s%s:\n\n' "$bold" "$(tilde "$rex_conf")" "$reset"
    printf '      %s\n\n' "$line"
    if ! ask "Add it to $(tilde "$rex_conf")?" y; then
      skip init.lua "left untouched; add the line above yourself"
      return 1
    fi
    { mkdir -p "$(dirname "$rex_conf")" && printf '%s\n' "$line" >>"$rex_conf"; } ||
      die "could not write $(tilde "$rex_conf")"
    ok init.lua "updated $(tilde "$rex_conf")"
  fi
  if "$rex_cli" --autostart=false config reload >/dev/null 2>&1; then
    ok rex "reloaded"
  else
    skip rex "not running; init.lua loads on its next start"
  fi
}

# Writes the tmux.conf lines (after asking) and reloads a running tmux.
# Fails when the user keeps tmux.conf as it is.
setup_tmux() {
  [ -f "$CONF" ] || : >"$CONF" || die "cannot create $(tilde "$CONF")"

  if [ "${AGENMUX_FORCE_WIZARD:-}" != 1 ] && conf_has agents-mon; then
    skip tmux.conf "still loads agents-mon; see README › Upgrading from agents-mon"
  elif [ "${AGENMUX_FORCE_WIZARD:-}" != 1 ] && conf_has agenmux; then
    skip tmux.conf "unchanged, already declares agenmux"
  else
    # run-shell, never TPM's @plugin: TPM would clone a second, tmux-only copy.
    plugin="run-shell \"$(tilde "$DIR")/agenmux.tmux\""
    printf '\n  Launcher keys, pressed after the tmux prefix:\n'
    sidebar_key="$(ask_key "Sidebar toggle" A)"
    popup_key="$(ask_key "Popup toggle" a)"
    [ "$sidebar_key" != "$popup_key" ] || die "sidebar and popup need different keys"
    # options must precede the plugin line; the plugin reads them when it loads
    block="set -g @agenmux-key '$sidebar_key'
set -g @agenmux-popup-key '$popup_key'
$plugin"
    printf '\n  Lines for %s%s%s:\n\n' "$bold" "$(tilde "$CONF")" "$reset"
    printf '%s\n' "$block" | sed 's/^/      /'
    printf '\n'
    if ask "Add them to $(tilde "$CONF")?" y; then
      printf '%s\n' "$block" >>"$CONF" || die "could not write $(tilde "$CONF")"
      ok tmux.conf "updated $(tilde "$CONF")"
    else
      skip tmux.conf "left untouched; add the lines above yourself, options before the plugin line"
      printf '\n'
      return 1
    fi
  fi

  if tmux list-sessions >/dev/null 2>&1; then
    # With someone watching, install the engine here with visible progress,
    # under the plugin's own lock; the reload's background install then finds
    # it current. Unattended runs leave it to that background install.
    if [ -n "$interactive" ]; then
      (
        tmux wait-for -L agenmux-install || exit 1
        rc=0
        bash "$DIR/scripts/install-bin.sh" || rc=$?
        tmux wait-for -U agenmux-install
        exit "$rc"
      ) </dev/null >/dev/null 2>&1 &
      job=$!
      i=0
      while kill -0 "$job" 2>/dev/null; do
        i=$(((i + 1) % 10))
        frame="$(printf '⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏' | cut -b"$((i * 3 + 1))-$((i * 3 + 3))")"
        printf '\r  %s%s%s %-10s %s' "$dim" "$frame" "$reset" engine "installing (a source build can take a few minutes)"
        sleep 0.1
      done
      printf '\r\033[K'
      if wait "$job"; then
        ok engine "installed $(version)"
      else
        skip engine "not installed; the first toggle retries (see README › Troubleshooting)"
      fi
    fi
    tmux source-file "$CONF" || die "tmux rejected $(tilde "$CONF"); fix the error above and run: tmux source-file $(tilde "$CONF")"
    if [ -n "$interactive" ]; then
      ok tmux "reloaded"
    else
      ok tmux "reloaded; the engine installs in the background"
    fi
  else
    ok tmux "not running; the engine downloads on first start"
  fi
}

main

#!/bin/sh
# One-line installer:  curl -fsSL https://snirt.github.io/agenmux/install.sh | sh
# Clones (or fast-forwards) the plugin, shows the tmux.conf lines it wants to
# add (launcher keys, then the plugin line) and asks before writing, then
# reloads a running tmux so
# agenmux.tmux fetches the verified engine. Re-run to update. Override paths
# with AGENMUX_DIR and AGENMUX_TMUX_CONF. Without a terminal (CI, containers)
# every question takes its default: keys A and a, write yes, link the agenmux
# command into ~/.local/bin, and the agent label is left to config.toml unless
# AGENMUX_AGENT_LABEL names one.
set -eu

DIR="${AGENMUX_DIR:-$HOME/.tmux/plugins/agenmux}"
REPO="${AGENMUX_REPO:-https://github.com/snirt/agenmux}"
TPM="$HOME/.tmux/plugins/tpm"

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
  printf '\n  %s⣿ tmux sidebar for AI coding agents%s\n\n' "$dim" "$reset"
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

  for cmd in git tmux bash; do
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
    # ponytail: line-based TOML edit; an inline `display = { ... }` table is
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

  if [ -n "${AGENMUX_TMUX_CONF:-}" ]; then
    CONF="$AGENMUX_TMUX_CONF"
  elif [ -f "$HOME/.tmux.conf" ]; then
    CONF="$HOME/.tmux.conf"
  elif [ -f "${XDG_CONFIG_HOME:-$HOME/.config}/tmux/tmux.conf" ]; then
    CONF="${XDG_CONFIG_HOME:-$HOME/.config}/tmux/tmux.conf"
  else
    CONF="$HOME/.tmux.conf"
  fi
  [ -f "$CONF" ] || : >"$CONF" || die "cannot create $(tilde "$CONF")"

  if [ "${AGENMUX_FORCE_WIZARD:-}" != 1 ] && conf_has agents-mon; then
    skip tmux.conf "still loads agents-mon; see README › Upgrading from agents-mon"
  elif [ "${AGENMUX_FORCE_WIZARD:-}" != 1 ] && conf_has agenmux; then
    skip tmux.conf "unchanged, already declares agenmux"
  else
    # TPM removes plugins it does not know about on clean, so declare it the TPM
    # way: the @plugin line must sit above the line that runs tpm.
    if [ -d "$TPM" ] && conf_has tpm/tpm; then
      tpm_user=1 plugin="set -g @plugin 'snirt/agenmux'"
    else
      tpm_user="" plugin="run-shell \"$(tilde "$DIR")/agenmux.tmux\""
    fi
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
      if [ -n "$tpm_user" ]; then
        # Replace the file a dotfiles symlink points at, not the link itself.
        # readlink without -f: macOS before 12.3 lacks it. Link loops never get
        # here; the [ -f ] check above already failed on them.
        target="$CONF"
        while [ -L "$target" ]; do
          link="$(readlink "$target")"
          case "$link" in /*) target="$link" ;; *) target="$(dirname "$target")/$link" ;; esac
        done
        # The rename is atomic, so a failed write never leaves a partial config.
        # cp -p first so the rewritten file keeps the original's mode.
        # ENVIRON, not -v: BSD awk rejects newlines in -v values.
        tmp="$target.agenmux.tmp"
        { cp -p "$target" "$tmp" &&
          block="$block" awk '!/^[[:space:]]*#/ && /tpm\/tpm/ && !done { print ENVIRON["block"]; done = 1 } { print }' \
            "$target" >"$tmp" && mv "$tmp" "$target"; } || {
          rm -f "$tmp"
          die "could not edit $(tilde "$target")"
        }
      else
        printf '%s\n' "$block" >>"$CONF"
      fi
      ok tmux.conf "updated $(tilde "$CONF")"
    else
      skip tmux.conf "left untouched; add the lines above yourself, options before the plugin line"
      printf '\n'
      exit 0
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

  printf '\n  Next: inside tmux press %sprefix + %s%s for the sidebar, %sprefix + %s%s for a popup.\n' \
    "$bold" "${sidebar_key:-A}" "$reset" "$bold" "${popup_key:-e}" "$reset"
  printf '  %sStatus-bar summary, keys, width, notifications: https://github.com/snirt/agenmux#usage%s\n' "$dim" "$reset"
  printf '  %sApp config and agent overrides live in %s/%s\n\n' "$dim" "$(tilde "$CFG")" "$reset"
}

main

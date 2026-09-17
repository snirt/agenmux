#!/bin/sh
# One-line installer:  curl -fsSL https://snirt.github.io/agenmux/install.sh | sh
# Clones (or fast-forwards) the plugin, shows the tmux.conf lines it wants to
# add (launcher keys, then the plugin line) and asks before writing, then
# reloads a running tmux so
# agenmux.tmux fetches the verified engine. Re-run to update. Override paths
# with AGENMUX_DIR and AGENMUX_TMUX_CONF. Without a terminal (CI, containers)
# every question takes its default: keys A and a, write yes.
set -eu

DIR="${AGENMUX_DIR:-$HOME/.tmux/plugins/agenmux}"
REPO="${AGENMUX_REPO:-https://github.com/snirt/agenmux}"
TPM="$HOME/.tmux/plugins/tpm"

# colour only on a terminal; piped output (CI, logs) stays plain
if [ -t 1 ] && [ "${TERM:-dumb}" != dumb ] && [ -z "${NO_COLOR:-}" ]; then
  bold="$(printf '\033[1m')" dim="$(printf '\033[2m')" green="$(printf '\033[32m')"
  red="$(printf '\033[31m')" reset="$(printf '\033[0m')"
else
  bold="" dim="" green="" red="" reset=""
fi
# decided once here: inside $(...) stdout is a pipe, so the prompts cannot test it
if [ -t 1 ] && [ -r /dev/tty ] && [ -w /dev/tty ]; then interactive=1; else interactive=""; fi
tilde() { case "$1" in "$HOME"/*) printf '~%s' "${1#"$HOME"}" ;; *) printf '%s' "$1" ;; esac }
ok() { printf '  %s✓%s %-10s %s\n' "$green" "$reset" "$1" "$2"; }
skip() { printf '  %s-%s %-10s %s\n' "$dim" "$reset" "$1" "$2"; }
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

# site/logo.png rendered as braille (48 columns): green agen, faded mu, >< chevrons
printf '\n'
printf '%s  ⣴⠶⠶⣦⡀ ⣠⡶⠶⢶⡶ ⢠⡶⠶⢶⣄ ⣠⡶⠶⣦ ⢠⡶⠶%s⣦⣴⠶⢶⡀⢰⡆  ⢰⡆%s⠰⣦⡀ %s ⣠⡶%s\n' "$green" "$dim" "$green" "$dim" "$reset"
printf '%s ⢸⡇  ⢸⣷⠰⣿   ⣿ ⣿⠶⠶⠶⠿ ⣿  ⢸⡇⣿⡇ %s⢸⡇ ⢸⡇⢸⡇  ⣸⡇%s ⢈⣿⠆%s⢸⣏%s\n' "$green" "$dim" "$green" "$dim" "$reset"
printf '%s ⠈⠻⠶⠶⠿⠟ ⠙⠷⠶⠾⠃ ⠘⠷⠶⠶⠃ ⠿  ⠸⠇⠿⠃ %s⠸⠇ ⠸⠇ ⠻⠶⠶⠟ %s⠰⠟⠁ %s ⠙⠷%s\n' "$green" "$dim" "$green" "$dim" "$reset"
printf '%s        ⠿⣤⣤⣴⠟%s\n' "$green" "$reset"
printf '\n  %s⣿ tmux sidebar for AI coding agents%s\n\n' "$dim" "$reset"
if [ -n "$interactive" ]; then
  printf '  %sFont check:%s ⣿  ⠹    ▢\n' "$bold" "$reset"
  printf '  If  is a box or blank, configure a Nerd Font in your terminal.\n\n'
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

if [ -d "$DIR/.git" ]; then
  before="$(version)"
  if [ "${AGENMUX_SKIP_UPDATE:-}" != 1 ]; then
    git -C "$DIR" pull --ff-only --quiet </dev/null || die "git pull failed in $(tilde "$DIR")"
  fi
  after="$(version)"
  if [ "$before" = "$after" ]; then
    ok plugin "$after already current in $(tilde "$DIR")"
  else
    ok plugin "updated $before → $after in $(tilde "$DIR")"
  fi
else
  git clone --quiet "$REPO" "$DIR" </dev/null || die "git clone failed"
  ok plugin "cloned $(version) to $(tilde "$DIR")"
fi

# same root the engine resolves: XDG_CONFIG_HOME when absolute, else ~/.config
case "${XDG_CONFIG_HOME:-}" in
/*) CFG="$XDG_CONFIG_HOME/agenmux" ;;
*) CFG="$HOME/.config/agenmux" ;;
esac
mkdir -p "$CFG/agents" || die "cannot create $(tilde "$CFG")"
ok config "$(tilde "$CFG")/ (config.toml optional, agents/ for overrides)"

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

if [ "${AGENMUX_FORCE_WIZARD:-}" != 1 ] && grep -q agents-mon "$CONF"; then
  skip tmux.conf "still loads agents-mon; see README › Upgrading from agents-mon"
elif [ "${AGENMUX_FORCE_WIZARD:-}" != 1 ] && grep -q agenmux "$CONF"; then
  skip tmux.conf "unchanged, already declares agenmux"
else
  # TPM removes plugins it does not know about on clean, so declare it the TPM
  # way: the @plugin line must sit above the line that runs tpm.
  if [ -d "$TPM" ] && grep -q tpm/tpm "$CONF"; then
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
      # ENVIRON, not -v: BSD awk rejects newlines in -v values
      block="$block" awk '/tpm\/tpm/ && !done { print ENVIRON["block"]; done = 1 } { print }' \
        "$CONF" >"$CONF.agenmux.tmp" && mv "$CONF.agenmux.tmp" "$CONF" || die "could not edit $(tilde "$CONF")"
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
  tmux source-file "$CONF" || die "tmux rejected $(tilde "$CONF"); fix the error above and run: tmux source-file $(tilde "$CONF")"
  ok tmux "reloaded; the engine downloads now, the first toggle waits for it"
else
  ok tmux "not running; the engine downloads on first start"
fi

printf '\n  Next: inside tmux press %sprefix + %s%s for the sidebar, %sprefix + %s%s for a popup.\n' \
  "$bold" "${sidebar_key:-A}" "$reset" "$bold" "${popup_key:-e}" "$reset"
printf '  %sStatus-bar summary, keys, width, notifications: https://github.com/snirt/agenmux#usage%s\n' "$dim" "$reset"
printf '  %sApp config and agent overrides live in %s/%s\n\n' "$dim" "$(tilde "$CFG")" "$reset"

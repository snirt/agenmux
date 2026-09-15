#!/usr/bin/env bash
# install.sh must be idempotent and declare the plugin the way the user's
# tmux.conf already loads plugins (TPM @plugin line above tpm, run-shell otherwise).
set -u
DIR="$(cd "$(dirname "$0")/.." && pwd)"
fail=0
home="$(mktemp -d)"
trap 'rm -rf "$home"' EXIT
# an empty socket dir means "no tmux server", so the reload branch is skipped
export HOME="$home" TMUX_TMPDIR="$home" AGENMUX_REPO="$home/src"
unset TMUX XDG_CONFIG_HOME
# CI checks the PR out detached; a clone of that has no branch to pull, so the
# installer clones from a copy that sits on one
git init -q "$home/src"
git -C "$home/src" fetch -q "$DIR" HEAD
git -C "$home/src" checkout -q -b main FETCH_HEAD

check() {
  if [ "$2" = "$3" ]; then
    echo "ok   install-script-$1"
  else
    echo "FAIL install-script-$1: expected [$3] got [$2]"
    fail=1
  fi
}

# plain tmux.conf: run-shell appended once, clone lands in plugins dir
sh "$DIR/install.sh" >/dev/null && sh "$DIR/install.sh" >/dev/null
check fresh-conf "$(cat "$home/.tmux.conf")" \
  'run-shell "~/.tmux/plugins/agenmux/agenmux.tmux"'
check clone "$([ -x "$home/.tmux/plugins/agenmux/agenmux.tmux" ] && echo yes)" yes
check config-dir "$([ -d "$home/.config/agenmux/agents" ] && echo yes)" yes

# TPM present: @plugin above the tpm run line, still only once
mkdir -p "$home/.tmux/plugins/tpm"
printf 'set -g mouse on\nrun "~/.tmux/plugins/tpm/tpm"\n' >"$home/.tmux.conf"
sh "$DIR/install.sh" >/dev/null && sh "$DIR/install.sh" >/dev/null
check tpm-conf "$(cat "$home/.tmux.conf")" \
  "$(printf "set -g mouse on\nset -g @plugin 'snirt/agenmux'\nrun \"~/.tmux/plugins/tpm/tpm\"")"

# a legacy agents-mon line is left alone rather than loading the plugin twice
printf 'run-shell ~/.tmux/plugins/tmux-agents-mon/agents-mon.tmux\n' >"$home/.tmux.conf"
sh "$DIR/install.sh" >/dev/null
check legacy-conf "$(cat "$home/.tmux.conf")" "run-shell ~/.tmux/plugins/tmux-agents-mon/agents-mon.tmux"

# XDG config is used when ~/.tmux.conf is absent
rm "$home/.tmux.conf"
mkdir -p "$home/.config/tmux"
: >"$home/.config/tmux/tmux.conf"
sh "$DIR/install.sh" >/dev/null
check xdg-conf "$(grep -c agenmux "$home/.config/tmux/tmux.conf")" 1
check no-dotfile "$([ -e "$home/.tmux.conf" ] || echo absent)" absent

# an absolute XDG_CONFIG_HOME moves the config root, like the engine does
XDG_CONFIG_HOME="$home/xdg" sh "$DIR/install.sh" >/dev/null
check xdg-config-dir "$([ -d "$home/xdg/agenmux/agents" ] && echo yes)" yes

exit "$fail"

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
# shellcheck source=tests/helpers/install-fixture.sh
. "$DIR/tests/helpers/install-fixture.sh"
install_fixture "$DIR" "$home/src"

check() {
  if [ "$2" = "$3" ]; then
    echo "ok   install-script-$1"
  else
    echo "FAIL install-script-$1: expected [$3] got [$2]"
    fail=1
  fi
}

# plain tmux.conf: both launcher keys and run-shell appended once, clone lands
# in plugins dir
sh "$DIR/install.sh" >/dev/null && sh "$DIR/install.sh" >/dev/null
check fresh-conf "$(cat "$home/.tmux.conf")" \
  "$(printf "set -g @agenmux-key 'A'\nset -g @agenmux-popup-key 'a'\nrun-shell \"~/.tmux/plugins/agenmux/agenmux.tmux\"")"
check clone "$([ -x "$home/.tmux/plugins/agenmux/agenmux.tmux" ] && echo yes)" yes
check config-dir "$([ -d "$home/.config/agenmux/agents" ] && echo yes)" yes

# Docker's wizard uses a writable config copy and must not update the mounted checkout.
printf 'no-update\n' >"$home/src/no-update-marker"
git -C "$home/src" add no-update-marker
git -C "$home/src" -c user.name=fixture -c user.email=fixture@example.invalid commit -q -m newer
installed_before="$(git -C "$home/.tmux/plugins/agenmux" rev-parse HEAD)"
: >"$home/dev.conf"
AGENMUX_DIR="$home/.tmux/plugins/agenmux" AGENMUX_TMUX_CONF="$home/dev.conf" \
  AGENMUX_SKIP_UPDATE=1 AGENMUX_FORCE_WIZARD=1 sh "$DIR/install.sh" >/dev/null
check skip-update "$(git -C "$home/.tmux/plugins/agenmux" rev-parse HEAD)" "$installed_before"
check forced-wizard "$(grep -c 'agenmux.tmux' "$home/dev.conf")" 1

# Docker mounts linked worktrees as a .git file whose host path is unavailable
# in the container. With updates disabled, the installer should use the files
# in the mount rather than trying to clone over it.
git -C "$home/src" worktree add -q --detach "$home/worktree" HEAD
printf 'gitdir: /unavailable/.git/worktrees/worktree\n' >"$home/worktree/.git"
: >"$home/worktree.conf"
if AGENMUX_DIR="$home/worktree" AGENMUX_TMUX_CONF="$home/worktree.conf" \
  AGENMUX_SKIP_UPDATE=1 AGENMUX_FORCE_WIZARD=1 sh "$DIR/install.sh" >/dev/null; then
  worktree_install=ok
else
  worktree_install=failed
fi
check worktree-reuse "$worktree_install" ok
check worktree-conf "$(grep -c agenmux "$home/worktree.conf")" 3

# TPM present: @plugin above the tpm run line, still only once
mkdir -p "$home/.tmux/plugins/tpm"
printf 'set -g mouse on\nrun "~/.tmux/plugins/tpm/tpm"\n' >"$home/.tmux.conf"
sh "$DIR/install.sh" >/dev/null && sh "$DIR/install.sh" >/dev/null
check tpm-conf "$(cat "$home/.tmux.conf")" \
  "$(printf "set -g mouse on\nset -g @agenmux-key 'A'\nset -g @agenmux-popup-key 'a'\nset -g @plugin 'snirt/agenmux'\nrun \"~/.tmux/plugins/tpm/tpm\"")"

# a dotfiles symlink stays a symlink; the TPM edit lands in its target,
# through a relative link chain, keeping the target's mode
mkdir -p "$home/dotfiles/tmux"
printf 'run "~/.tmux/plugins/tpm/tpm"\n' >"$home/dotfiles/tmux/tmux.conf"
chmod 600 "$home/dotfiles/tmux/tmux.conf"
ln -s tmux/tmux.conf "$home/dotfiles/tmux.conf"
ln -sf dotfiles/tmux.conf "$home/.tmux.conf"
symlink_status=0
sh "$DIR/install.sh" >/dev/null || symlink_status=$?
check symlink-exit "$symlink_status" 0
check symlink-kept "$(readlink "$home/.tmux.conf") $(readlink "$home/dotfiles/tmux.conf")" \
  "dotfiles/tmux.conf tmux/tmux.conf"
check symlink-target "$(cat "$home/dotfiles/tmux/tmux.conf")" \
  "$(printf "set -g @agenmux-key 'A'\nset -g @agenmux-popup-key 'a'\nset -g @plugin 'snirt/agenmux'\nrun \"~/.tmux/plugins/tpm/tpm\"")"
check symlink-mode "$(ls -l "$home/dotfiles/tmux/tmux.conf" | cut -c1-10)" "-rw-------"
check symlink-no-tmp "$(find "$home" -name '*.agenmux.tmp' | wc -l | tr -d ' ')" 0

# a failed rewrite leaves the config intact and no temp file behind;
# root ignores the read-only dir, so containers skip this case
if [ "$(id -u)" != 0 ]; then
  chmod 500 "$home/dotfiles/tmux"
  failed_status=0
  AGENMUX_FORCE_WIZARD=1 sh "$DIR/install.sh" >/dev/null 2>&1 || failed_status=$?
  chmod 700 "$home/dotfiles/tmux"
  check failed-exit "$failed_status" 1
  check failed-intact "$(grep -c agenmux "$home/dotfiles/tmux/tmux.conf")" 3
  check failed-no-tmp "$(find "$home" -name '*.agenmux.tmp' | wc -l | tr -d ' ')" 0
fi
rm "$home/.tmux.conf"

# a legacy agents-mon line is left alone rather than loading the plugin twice
printf 'run-shell ~/.tmux/plugins/tmux-agents-mon/agents-mon.tmux\n' >"$home/.tmux.conf"
sh "$DIR/install.sh" >/dev/null
check legacy-conf "$(cat "$home/.tmux.conf")" "run-shell ~/.tmux/plugins/tmux-agents-mon/agents-mon.tmux"

# XDG config is used when ~/.tmux.conf is absent
rm "$home/.tmux.conf"
mkdir -p "$home/.config/tmux"
: >"$home/.config/tmux/tmux.conf"
sh "$DIR/install.sh" >/dev/null
check xdg-conf "$(grep -c agenmux "$home/.config/tmux/tmux.conf")" 3
check no-dotfile "$([ -e "$home/.tmux.conf" ] || echo absent)" absent

# an absolute XDG_CONFIG_HOME moves the config root, like the engine does
XDG_CONFIG_HOME="$home/xdg" sh "$DIR/install.sh" >/dev/null
check xdg-config-dir "$([ -d "$home/xdg/agenmux/agents" ] && echo yes)" yes

exit "$fail"

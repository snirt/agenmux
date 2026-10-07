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
check command-link "$(readlink "$home/.local/bin/agenmux")" "$home/.tmux/plugins/agenmux/target/release/agenmux"

# something else already at ~/.local/bin/agenmux is never replaced
mv "$home/.local/bin/agenmux" "$home/link.bak"
printf 'mine\n' >"$home/.local/bin/agenmux"
sh "$DIR/install.sh" >/dev/null
check command-foreign-kept "$(cat "$home/.local/bin/agenmux")" mine
mv "$home/link.bak" "$home/.local/bin/agenmux"

# a new link off PATH prints the line to add; on PATH it stays quiet
rm "$home/.local/bin/agenmux"
# shellcheck disable=SC2016 # matching the literal line the installer prints
check command-path-hint "$(sh "$DIR/install.sh" | grep -c 'export PATH="$HOME/.local/bin:$PATH"')" 1
rm "$home/.local/bin/agenmux"
check command-path-quiet "$(PATH="$home/.local/bin:$PATH" sh "$DIR/install.sh" | grep -c 'not on PATH')" 0

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

# commented lines neither count as a declaration nor anchor the TPM insert
printf "# set -g @plugin 'snirt/agenmux'\n# run '~/.tmux/plugins/tpm/tpm'\nrun '~/.tmux/plugins/tpm/tpm'\n" >"$home/.tmux.conf"
sh "$DIR/install.sh" >/dev/null
check commented-conf "$(cat "$home/.tmux.conf")" \
  "$(printf "# set -g @plugin 'snirt/agenmux'\n# run '~/.tmux/plugins/tpm/tpm'\nset -g @agenmux-key 'A'\nset -g @agenmux-popup-key 'a'\nset -g @plugin 'snirt/agenmux'\nrun '~/.tmux/plugins/tpm/tpm'")"

# curl | sh cut off mid-download runs nothing
cut_home="$home/cut"
mkdir -p "$cut_home"
cut_status=0
head -c "$(($(wc -c <"$DIR/install.sh") - 10))" "$DIR/install.sh" |
  HOME="$cut_home" sh >/dev/null 2>&1 || cut_status=$?
check truncated-fails "$([ "$cut_status" -ne 0 ] && echo yes)" yes
check truncated-no-writes "$(find "$cut_home" -mindepth 1 | wc -l | tr -d ' ')" 0

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
check no-label-unasked "$([ -e "$home/xdg/agenmux/config.toml" ] || echo absent)" absent

# the font answer lands in [display]: a new table, under an existing one, and
# never over a label the user already chose
app="$home/labels/agenmux/config.toml"
XDG_CONFIG_HOME="$home/labels" AGENMUX_AGENT_LABEL=icon sh "$DIR/install.sh" >/dev/null
check label-new "$(cat "$app")" "$(printf '[display]\nagent_label = "icon"')"
printf '[behavior]\nnotifications = false\n[display]\nmode = "popup"\n' >"$app"
XDG_CONFIG_HOME="$home/labels" AGENMUX_AGENT_LABEL=text sh "$DIR/install.sh" >/dev/null
check label-existing-table "$(cat "$app")" \
  "$(printf '[behavior]\nnotifications = false\n[display]\nagent_label = "text"\nmode = "popup"')"
XDG_CONFIG_HOME="$home/labels" AGENMUX_AGENT_LABEL=icon sh "$DIR/install.sh" >/dev/null
check label-kept "$(grep -c agent_label "$app") $(grep agent_label "$app")" '1 agent_label = "text"'
check label-no-tmp "$(find "$home/labels" -name '*.agenmux.tmp' | wc -l | tr -d ' ')" 0
check label-invalid "$(XDG_CONFIG_HOME="$home/labels" AGENMUX_AGENT_LABEL=big sh "$DIR/install.sh" >/dev/null 2>&1 || echo refused)" refused

# Fresh installs land on the latest published stable tag; a detached release
# checkout follows newer tags on re-run; AGENMUX_REF keeps a development branch.
git_fixture() { git -C "$home/src" -c user.name=fixture -c user.email=fixture@example.invalid "$@"; }
version_now="$(bash "$DIR/scripts/version.sh" tag)"
git_fixture tag "$version_now"
sed -i.bak 's/^version = .*/version = "99.0.0"/' "$home/src/Cargo.toml" && rm "$home/src/Cargo.toml.bak"
git_fixture commit -q -am "next release"
git_fixture tag v99.0.0-rc1
git_fixture commit -q --allow-empty -m "development"
mkdir -p "$home/fake-bin"
cat >"$home/fake-bin/curl" <<'EOF'
#!/bin/sh
printf '%s/releases/tag/%s' "$AGENMUX_REPO" "$LATEST_TAG"
EOF
chmod +x "$home/fake-bin/curl"
tagged="$home/tagged/agenmux"
PATH="$home/fake-bin:$PATH" LATEST_TAG="$version_now" AGENMUX_DIR="$tagged" \
  AGENMUX_TMUX_CONF="$home/tagged.conf" sh "$DIR/install.sh" >/dev/null
check fresh-on-release-tag "$(git -C "$tagged" describe --tags --exact-match)" "$version_now"
# a prerelease is never "latest stable": the checkout stays put
PATH="$home/fake-bin:$PATH" LATEST_TAG=v99.0.0-rc1 AGENMUX_DIR="$tagged" \
  AGENMUX_TMUX_CONF="$home/tagged.conf" sh "$DIR/install.sh" >/dev/null
check rerun-ignores-prerelease "$(git -C "$tagged" describe --tags --exact-match)" "$version_now"
git_fixture tag v99.0.0 HEAD~1
PATH="$home/fake-bin:$PATH" LATEST_TAG=v99.0.0 AGENMUX_DIR="$tagged" \
  AGENMUX_TMUX_CONF="$home/tagged.conf" sh "$DIR/install.sh" >/dev/null
check rerun-follows-newer-tag "$(git -C "$tagged" describe --tags --exact-match)" v99.0.0
PATH="$home/fake-bin:$PATH" LATEST_TAG="$version_now" AGENMUX_DIR="$tagged" \
  AGENMUX_TMUX_CONF="$home/tagged.conf" sh "$DIR/install.sh" >/dev/null
check rerun-never-downgrades "$(git -C "$tagged" describe --tags --exact-match)" v99.0.0
dev="$home/dev/agenmux"
PATH="$home/fake-bin:$PATH" LATEST_TAG=v99.0.0 AGENMUX_REF=main AGENMUX_DIR="$dev" \
  AGENMUX_TMUX_CONF="$home/dev-ref.conf" sh "$DIR/install.sh" >/dev/null
check ref-keeps-branch "$(git -C "$dev" symbolic-ref --short HEAD)" main
PATH="$home/fake-bin:$PATH" LATEST_TAG=v99.0.0 AGENMUX_DIR="$dev" \
  AGENMUX_TMUX_CONF="$home/dev-ref.conf" sh "$DIR/install.sh" >/dev/null
check branch-rerun-pulls "$(git -C "$dev" rev-parse HEAD)" "$(git -C "$home/src" rev-parse HEAD)"

exit "$fail"

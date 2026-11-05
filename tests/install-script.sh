#!/usr/bin/env bash
# install.sh must be idempotent and install once, outside any multiplexer's
# own directories, for tmux (a run-shell line) and Rex alike.
set -u
DIR="$(cd "$(dirname "$0")/.." && pwd)"
fail=0
home="$(mktemp -d)"
trap 'rm -rf "$home"' EXIT
# an empty socket dir means "no tmux server", so the reload branch is skipped
export HOME="$home" TMUX_TMPDIR="$home" AGENMUX_REPO="$home/src"
# a developer's Rex install must not join the tmux cases; Rex has its own below
export AGENMUX_MUX=tmux
unset TMUX XDG_CONFIG_HOME XDG_DATA_HOME REX_CONFIG
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
# in the data dir
sh "$DIR/install.sh" >/dev/null && sh "$DIR/install.sh" >/dev/null
check fresh-conf "$(cat "$home/.tmux.conf")" \
  "$(printf "set -g @agenmux-key 'A'\nset -g @agenmux-popup-key 'a'\nrun-shell \"~/.local/share/agenmux/agenmux.tmux\"")"
check clone "$([ -x "$home/.local/share/agenmux/agenmux.tmux" ] && echo yes)" yes
check config-dir "$([ -d "$home/.config/agenmux/agents" ] && echo yes)" yes
check command-link "$(readlink "$home/.local/bin/agenmux")" "$home/.local/share/agenmux/target/release/agenmux"

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
installed_before="$(git -C "$home/.local/share/agenmux" rev-parse HEAD)"
: >"$home/dev.conf"
AGENMUX_DIR="$home/.local/share/agenmux" AGENMUX_TMUX_CONF="$home/dev.conf" \
  AGENMUX_SKIP_UPDATE=1 AGENMUX_FORCE_WIZARD=1 sh "$DIR/install.sh" >/dev/null
check skip-update "$(git -C "$home/.local/share/agenmux" rev-parse HEAD)" "$installed_before"
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

# TPM present: still a run-shell line, never @plugin (TPM would clone a second,
# tmux-only copy), and only once
mkdir -p "$home/.tmux/plugins/tpm"
printf 'set -g mouse on\nrun "~/.tmux/plugins/tpm/tpm"\n' >"$home/.tmux.conf"
sh "$DIR/install.sh" >/dev/null && sh "$DIR/install.sh" >/dev/null
check tpm-conf "$(cat "$home/.tmux.conf")" \
  "$(printf "set -g mouse on\nrun \"~/.tmux/plugins/tpm/tpm\"\nset -g @agenmux-key 'A'\nset -g @agenmux-popup-key 'a'\nrun-shell \"~/.local/share/agenmux/agenmux.tmux\"")"
check tpm-no-copy "$([ -e "$home/.tmux/plugins/agenmux" ] || echo absent)" absent

# a dotfiles symlink stays a symlink; the lines land in its target
mkdir -p "$home/dotfiles/tmux"
: >"$home/dotfiles/tmux/tmux.conf"
ln -s tmux/tmux.conf "$home/dotfiles/tmux.conf"
ln -sf dotfiles/tmux.conf "$home/.tmux.conf"
sh "$DIR/install.sh" >/dev/null
check symlink-kept "$(readlink "$home/.tmux.conf") $(readlink "$home/dotfiles/tmux.conf")" \
  "dotfiles/tmux.conf tmux/tmux.conf"
check symlink-target "$(grep -c agenmux "$home/dotfiles/tmux/tmux.conf")" 3
rm "$home/.tmux.conf"

# a commented line does not count as a declaration
printf "# run-shell ~/.local/share/agenmux/agenmux.tmux\n" >"$home/.tmux.conf"
sh "$DIR/install.sh" >/dev/null
check commented-conf "$(grep -c '^run-shell' "$home/.tmux.conf")" 1
rm "$home/.tmux.conf"

# an install from before lives in tmux's plugin dir and keeps updating there,
# so the line that loads it stays valid
old_home="$home/old"
mkdir -p "$old_home/.tmux/plugins"
git clone -q "$home/src" "$old_home/.tmux/plugins/agenmux"
HOME="$old_home" sh "$DIR/install.sh" >/dev/null
check legacy-dir-kept "$(grep -c '~/.tmux/plugins/agenmux/agenmux.tmux' "$old_home/.tmux.conf") $([ -e "$old_home/.local/share/agenmux" ] || echo absent)" "1 absent"

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

# Rex alone: no tmux.conf, init.lua shown with full paths but never written,
# and Rex reloads only once init.lua loads agenmux.
mkdir -p "$home/rex-bin"
cat >"$home/rex-bin/rex" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >>"$REX_LOG"
EOF
chmod +x "$home/rex-bin/rex"
plugin="$home/.local/share/agenmux"
mkdir -p "$plugin/target/release"
[ -e "$plugin/target/release/agenmux" ] || { printf '#!/bin/sh\n' >"$plugin/target/release/agenmux" && chmod +x "$plugin/target/release/agenmux"; }
rex_install() {
  AGENMUX_MUX=rex AGENMUX_REX="$home/rex-bin/rex" REX_LOG="$home/rex.log" \
    AGENMUX_TMUX_CONF="$home/rex-only.conf" sh "$DIR/install.sh"
}
out="$(rex_install)"
line="dofile(\"$plugin/rex/agenmux.lua\"){ bin = \"$plugin/target/release/agenmux\", key = \"alt+s\", popup_key = \"alt+p\" }"
check rex-line "$(printf '%s\n' "$out" | grep -cF "$line")" 1
check rex-no-tmux-conf "$([ -e "$home/rex-only.conf" ] && echo written || echo none)" none
check rex-init-written "$(grep -cF "$line" "$home/.config/rex/init.lua")" 1
check rex-reload "$(cat "$home/rex.log")" "--autostart=false config reload"
printf -- '-- mine\n%s\n' "$line" >"$home/.config/rex/init.lua"
rex_install >/dev/null
check rex-init-kept "$(head -1 "$home/.config/rex/init.lua") $(grep -cF "$line" "$home/.config/rex/init.lua")" "-- mine 1"

# On a terminal the checklist starts from what is integrated: unchecking Rex
# and checking tmux swaps them, then unchecking tmux removes it too.
if command -v expect >/dev/null 2>&1; then
  checklist() { # keys for the checklist
    AGENMUX_MUX= AGENMUX_REX="$home/rex-bin/rex" REX_LOG="$home/rex.log" AGENMUX_TMUX_CONF="$home/rex-only.conf" expect -c "
      set timeout 30
      log_user 0
      spawn sh $DIR/install.sh
      expect \"show as an icon\" { send \"\\r\" }
      expect \"Integrations\" { send \"$1\" }
      expect {
        -re \"(Sidebar|Popup) toggle\" { send \"\\r\"; exp_continue }
        \"Add \" { send \"y\\r\"; exp_continue }
        \"Remove \" { send \"y\\r\"; exp_continue }
        eof
      }
      lassign [wait] pid spawnid os_error exit_code
      exit \$exit_code
    "
  }
  checklist ' j \r' >/dev/null
  check checklist-rex-removed "$(grep -c agenmux "$home/.config/rex/init.lua")" 0
  check checklist-rex-kept-mine "$(cat "$home/.config/rex/init.lua")" "-- mine"
  check checklist-tmux-added "$(grep -c agenmux "$home/rex-only.conf")" 3
  checklist '\r' >/dev/null
  check checklist-rerun-keeps "$(grep -c agenmux "$home/rex-only.conf") $(grep -c agenmux "$home/.config/rex/init.lua")" "3 0"
  checklist ' \r' >/dev/null
  check checklist-tmux-removed "$(grep -c agenmux "$home/rex-only.conf")" 0
fi
check rex-mux-invalid "$(AGENMUX_MUX=screen sh "$DIR/install.sh" >/dev/null 2>&1 || echo refused)" refused

exit "$fail"

#!/usr/bin/env bash
# install_fixture SRC DEST: a committed git repo of the SRC working tree, on a
# branch, for the installer to clone and pull from. CI checks the PR out
# shallow and detached, so neither `git clone` nor `git fetch` of the checkout
# itself yields something `git pull --ff-only` accepts.
install_fixture() {
  local src="$1" dest="$2"
  mkdir -p "$dest"
  tar --exclude=.git --exclude=target --exclude=target-linux -C "$src" -cf - . |
    tar -C "$dest" -xf -
  git -C "$dest" init -q -b main
  git -C "$dest" add -A
  git -C "$dest" -c user.name=fixture -c user.email=fixture@example.invalid \
    commit -q -m fixture
}

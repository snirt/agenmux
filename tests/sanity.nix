let
  pkgs = import (builtins.fetchTarball {
    url = "https://github.com/NixOS/nixpkgs/archive/ffb3c9b700e759be2ef13237c9d8f953b32a1e46.tar.gz";
    sha256 = "0h1nqp7vdqqivfc8fimdc6rmjyrnjlsmd28zxbl1iy46c0ss8ga4";
  }) { };
  tmux37 = pkgs.tmux.overrideAttrs (_: {
    version = "3.7b";
    src = pkgs.fetchFromGitHub {
      owner = "tmux";
      repo = "tmux";
      rev = "3.7b";
      hash = "sha256-CTq06XP997M0ODxQihTq34dI9H6jSRLUXLYuTWOwDpc=";
    };
  });
in
pkgs.mkShell {
  packages = with pkgs; [
    bash
    cacert
    cargo
    coreutils
    curl
    expect
    gawk
    git
    gnugrep
    gnused
    gnutar
    rustc
    tmux37
  ];
}

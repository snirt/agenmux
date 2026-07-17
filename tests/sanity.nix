let
  pkgs = import <nixpkgs> { };
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
    gawk
    gnugrep
    gnused
    gnutar
    rustc
    tmux37
  ];
}

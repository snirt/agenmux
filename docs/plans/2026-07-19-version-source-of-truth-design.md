# Version source of truth design

`Cargo.toml`'s `[package].version` is the sole authored version for
tmux-agents-mon. `Cargo.lock` remains a generated Cargo artifact, and Git release
tags derive from the manifest version rather than duplicating it independently.

The compiled binary exposes `agents-mon --version` using Cargo's
`CARGO_PKG_VERSION` build-time value. A small repository script reads the same
package version from Cargo metadata, prints either the bare version or its `v`-
prefixed release tag, and can verify a proposed tag. This gives local release
steps and GitHub Actions one shared interface without introducing another
version file.

For tagged builds, GitHub Actions verifies that the pushed tag is exactly
`v<package-version>` before publishing. A mismatch fails the workflow instead
of producing a release whose tag and binary disagree. Regular branch and pull
request builds continue unchanged.

Tests cover the binary's `--version` output and the repository script's print,
tag, and mismatch behavior. Contributor documentation describes the release
sequence: update `Cargo.toml`, let Cargo refresh `Cargo.lock`, verify the derived
tag, then create and push that tag.

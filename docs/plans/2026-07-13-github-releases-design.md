# GitHub Releases design

Version tags matching `v*` publish permanent, publicly downloadable plugin
archives through GitHub Releases. Regular pushes, pull requests, and manual runs
continue to test all supported targets and retain their outputs as temporary
workflow artifacts.

The existing build matrix remains the sole producer of packages: x86_64 and
ARM64 builds run natively on Linux and macOS, execute the Rust and fixture parity
tests, and package the complete TPM plugin with its native binary at
`target/release/agents-mon`. Linux outputs use musl so they do not depend on the
runner's glibc version. Each matrix job uploads its `.tar.gz` without another ZIP
layer, preserving the executable permissions stored inside the tarball.

A separate release job runs only for version tags and only after every matrix
job succeeds. It downloads the four exact artifacts produced by those jobs,
asserts that all four archives are present, generates a `SHA256SUMS` file, and
creates a release for the existing tag with generated notes. The job alone gets
`actions: read` and `contents: write`; builds retain read-only repository access.
`--verify-tag` prevents the publishing command from silently creating a tag at
an unintended commit.

This sequencing makes failure atomic from a user's perspective: a platform
build or test failure prevents release publication. A successful tagged run
produces four permanent assets and their checksums under Releases, while the
short-lived Actions artifacts remain useful for reviewing ordinary commits.

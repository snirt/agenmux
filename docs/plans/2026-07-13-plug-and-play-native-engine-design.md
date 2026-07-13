# Plug-and-play native engine design

TPM remains the only required installation flow. When the default native binary
is missing, both plugin entry points call one background installer and continue
with the bash fallback immediately.

The installer maps `uname` OS and architecture values to one of the four Release
archives, downloads the archive and `SHA256SUMS`, verifies the exact asset, and
atomically places its binary at `target/release/agents-mon`. Unsupported systems
or download failures fall back to the existing local Cargo build. If neither is
available, the dependency-free bash engine continues to work.

No updater or startup daemon is added. Once the binary exists, startup performs
only the existing executable check. A test serves a real local archive through
stubbed platform/download commands and verifies the installed binary runs.

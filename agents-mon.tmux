#!/usr/bin/env bash
# Compatibility entrypoint for pre-agenmux tmux configurations.
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec bash "$DIR/agenmux.tmux" "$@"

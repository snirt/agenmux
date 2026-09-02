#!/usr/bin/env bash
# Compatibility entrypoint for pre-Agenmux tmux configurations.
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec bash "$DIR/agenmux.tmux" "$@"

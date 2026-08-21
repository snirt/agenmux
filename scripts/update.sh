#!/usr/bin/env bash
# Compatibility entry point; native release switching lives in agents-mon.
DIR="$(cd "$(dirname "$0")/.." && pwd)"
exec "$DIR/target/release/agents-mon" update "${1:-latest}"

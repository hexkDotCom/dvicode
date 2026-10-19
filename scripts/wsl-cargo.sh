#!/usr/bin/env bash
# Runs cargo for the server inside WSL with a Linux-side target directory (much faster than
# building on the Windows drive). Usage: scripts/wsl-cargo.sh <cargo args...>
set -e
HERE="$(cd "$(dirname "$0")/.." && pwd)"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/dvicode-target}"
cd "$HERE/server"
exec cargo "$@"

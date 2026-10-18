#!/usr/bin/env bash
# Builds the static Linux server (in WSL or on Linux) and packages the VS Code extension.
#   scripts/package.sh            server + VSIX
#   scripts/package.sh --server   server only (copied into client/server/linux-x64)
# On Windows run it from Git Bash or PowerShell via: wsl.exe -e bash scripts/package.sh --server
set -euo pipefail
HERE="$(cd "$(dirname "$0")/.." && pwd)"
source "$HOME/.cargo/env" 2>/dev/null || true
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/dvicode-target}"
TARGET=x86_64-unknown-linux-musl
rustup target add "$TARGET" >/dev/null 2>&1 || true
(cd "$HERE/server" && cargo build --release --target "$TARGET")
mkdir -p "$HERE/client/server/linux-x64"
cp "$CARGO_TARGET_DIR/$TARGET/release/dvicode-server" "$HERE/client/server/linux-x64/dvicode-server"
chmod +x "$HERE/client/server/linux-x64/dvicode-server"
echo "server: $HERE/client/server/linux-x64/dvicode-server"
if [ "${1:-}" = "--server" ]; then exit 0; fi
cd "$HERE/client"
npm install --no-audit --no-fund
npm run build
npx vsce package --no-dependencies

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
(cd "$HERE/server" && cargo build --release --locked --target "$TARGET")
mkdir -p "$HERE/client/server/linux-x64"
cp "$CARGO_TARGET_DIR/$TARGET/release/dvicode-server" "$HERE/client/server/linux-x64/dvicode-server"
chmod +x "$HERE/client/server/linux-x64/dvicode-server"
echo "server: $HERE/client/server/linux-x64/dvicode-server"
# Licenses shipped with the extension: ours, plus everything bundled or linked (the npm
# packages are read from client/node_modules).
[ -d "$HERE/client/node_modules" ] || (cd "$HERE/client" && npm ci --no-audit --no-fund)
python3 "$HERE/scripts/third_party_notices.py"
cp "$HERE/LICENSE" "$HERE/NOTICE" "$HERE/THIRD_PARTY_NOTICES.md" "$HERE/client/"
if [ "${1:-}" = "--server" ]; then exit 0; fi
cd "$HERE/client"
npm ci --no-audit --no-fund
npm run build
npx vsce package --no-dependencies

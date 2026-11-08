#!/usr/bin/env bash
# Build the UI bundle (apps/ui) and put it in $PF_SHARE_DIR/ui/ (default
# ~/.local/share/pulse-factory/ui). `scripts/install.sh` calls it after the skill copy. It needs
# Node 22 or later and npm. It needs no systemd and no cargo, so it runs alone:
#
#   PF_SHARE_DIR="$(mktemp -d)" scripts/install-ui.sh
#
# `npm ci` runs once per tree: only when apps/node_modules is missing or older than the lock file.
# It never downloads a browser (the d12 journey uses the one in the Playwright cache).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
apps="$root/apps"
share="${PF_SHARE_DIR:-$HOME/.local/share/pulse-factory}"

fail() { echo "install-ui: $*" >&2; exit 1; }

command -v node >/dev/null || fail "node is missing: install Node 22 or later (the install builds the UI with it)"
command -v npm >/dev/null || fail "npm is missing: install npm with Node 22 or later (the install builds the UI with it)"
major="$(node -p 'process.versions.node.split(".")[0]')"
[ "$major" -ge 22 ] || fail "Node $(node -p process.versions.node) is too old: install Node 22 or later"

if [ ! -d "$apps/node_modules" ] || [ "$apps/package-lock.json" -nt "$apps/node_modules" ]; then
  PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 nice -n 10 ionice -c3 npm --prefix "$apps" ci --no-audit --no-fund
fi

nice -n 10 ionice -c3 npm --prefix "$apps" run build --workspace ui
[ -f "$apps/ui/dist/index.html" ] || fail "the build made no apps/ui/dist/index.html"

# Replace $share/ui in one move: copy beside it, then swap.
mkdir -p "$share"
rm -rf "$share/.ui.new"
cp -R "$apps/ui/dist" "$share/.ui.new"
rm -rf "$share/ui"
mv "$share/.ui.new" "$share/ui"
echo "install-ui: the UI is in $share/ui"

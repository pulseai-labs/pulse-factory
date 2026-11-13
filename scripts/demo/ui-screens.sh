#!/usr/bin/env bash
# d25: the screens of the Mac app, each compared with a committed screenshot, in one headless Chromium
# launch, on fixed test data, under a fixed rendering contract. The number of screens is the number of
# `test(` calls in apps/ui/e2e/screens.spec.ts; this script writes no count of its own.
#
#   PF_UI_DIR=apps/ui/dist scripts/demo/ui-screens.sh
#
# `PF_UI_DIR` is the built bundle (default `${PF_SHARE_DIR:-$HOME/.local/share/pulse-factory}/ui`) and must
# hold `index.html`. This script builds nothing. It runs `npm ci` in apps/ only when apps/node_modules is
# missing, and it downloads no browser: Chromium 1248 must be in the Playwright cache.
#
# It starts no controller and no bridge. The Playwright worker starts the fixture server
# (apps/ui/e2e/screens/server.ts): a Node http server on 127.0.0.1 that serves the bundle with the bridge's
# Content-Security-Policy and answers the bridge protocol from fixed test data. The spec is
# apps/ui/e2e/screens.spec.ts and the screenshots are in apps/ui/e2e/screens.spec.ts-snapshots/. A compare
# run also checks that the folder holds exactly one `-linux.png` file for each test, and names the missing
# or extra file. The run reaches no address beyond 127.0.0.1 and removes its temp directory on exit.
#
# `PF_SHOTS_DIR` names a directory that keeps the diff images of a failed run. `PF_SCREENS_UPDATE=1` writes
# the screenshots from the bundle instead of comparing them, prints how many it wrote and exits 3, so an
# update run never counts as a pass. The ledger's command never sets it. The screenshots are made on
# draco-desk: they carry Playwright's `-linux` suffix, and no CI job runs this line.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
PF_UI_DIR="${PF_UI_DIR:-${PF_SHARE_DIR:-$HOME/.local/share/pulse-factory}/ui}"
browsers="${PLAYWRIGHT_BROWSERS_PATH:-$HOME/.cache/ms-playwright}"
started=$(date +%s)

fail() { echo "ui-screens: FAIL: $*" >&2; exit 1; }

for tool in node npm; do command -v "$tool" >/dev/null || fail "$tool is not on PATH"; done
[ "$(node -p 'process.versions.node.split(".")[0]')" -ge 22 ] || fail "Node 22 or later is needed"
[ -f "$PF_UI_DIR/index.html" ] || fail "PF_UI_DIR=$PF_UI_DIR has no index.html: build the UI first (this script builds nothing)"
[ -d "$browsers/chromium-1248" ] || fail "Chromium 1248 is not in the Playwright cache ($browsers/chromium-1248): it is the browser of @playwright/test 1.64.1, and this script downloads none"

if [ ! -d "$root/apps/node_modules" ]; then
  PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 npm --prefix "$root/apps" ci --no-audit --no-fund
fi

tmp="$(mktemp -d)"
cleanup() {
  # A failed run keeps its diff images only when PF_SHOTS_DIR is set.
  if [ -n "${PF_SHOTS_DIR:-}" ] && [ -d "$tmp/playwright-output" ]; then
    mkdir -p "$PF_SHOTS_DIR"
    find "$tmp/playwright-output" -name '*.png' -exec cp {} "$PF_SHOTS_DIR"/ \; 2>/dev/null || true
  fi
  rm -rf "$tmp"
}
trap cleanup EXIT

export PF_UI_DIR PF_E2E_OUT="$tmp/playwright-output"
spec="$root/apps/ui/e2e/screens.spec.ts"
snaps="$root/apps/ui/e2e/screens.spec.ts-snapshots"
# One screen is one `test(` call. The screenshot a test takes is named by the first word of its title, which
# is also the file name: `test("2a-chat: ...")` takes `2a-chat.png`, kept as `2a-chat-linux.png`.
screens="$(grep -cE '^[[:space:]]*test\(' "$spec" || true)"
[ "${screens:-0}" -gt 0 ] || fail "no test( call in $spec"
update=()
if [ "${PF_SCREENS_UPDATE:-}" = "1" ]; then update=(--update-snapshots=all); fi

# In a compare run the snapshot folder must hold one file per test, no more and no fewer.
if [ -z "${update[*]:-}" ]; then
  want="$(grep -E '^[[:space:]]*test\(' "$spec" | sed -E 's/^[[:space:]]*test\("([^":]+)[":].*$/\1-linux.png/' | sort)"
  have="$(cd "$snaps" 2>/dev/null && ls -1 -- *-linux.png 2>/dev/null | sort || true)"
  missing="$(comm -23 <(echo "$want") <(echo "$have"))"
  extra="$(comm -13 <(echo "$want") <(echo "$have"))"
  [ -z "$missing" ] || fail "screenshot missing from screens.spec.ts-snapshots/: $(echo "$missing" | tr '\n' ' ')"
  [ -z "$extra" ] || fail "screenshot with no test in screens.spec.ts-snapshots/: $(echo "$extra" | tr '\n' ' ')"
fi

log="$tmp/playwright.log"
if ! npm --prefix "$root/apps" run e2e --workspace ui -- e2e/screens.spec.ts ${update[@]+"${update[@]}"} > "$log" 2>&1; then
  cat "$log" >&2
  fail "a screen did not match its screenshot, or the run failed (the output above names the screen)"
fi
passed="$(sed -n 's/^ *\([0-9][0-9]*\) passed.*$/\1/p' "$log" | tail -n 1)"
[ "${passed:-0}" = "$screens" ] || { cat "$log" >&2; fail "$screens screens were expected and ${passed:-0} passed"; }
if grep -qE '^ *[0-9]+ (failed|skipped|flaky|did not run)' "$log"; then
  cat "$log" >&2
  fail "the run reports a screen that did not pass"
fi

if [ "${PF_SCREENS_UPDATE:-}" = "1" ]; then
  echo "ui-screens: updated $passed screenshots"
  exit 3
fi
echo "ui-screens: ok ($passed screens, $(( $(date +%s) - started )) s)"

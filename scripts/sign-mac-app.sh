#!/usr/bin/env bash
# Sign the Mac app again on the Mac, with an Apple-issued code-signing identity, so that a new build
# reads the device token from the Keychain without asking for the login password.
#
# CI signs the app ad hoc (`.github/workflows/macos-app.yml`). macOS keys a Keychain item of an ad-hoc
# or self-signed app to the binary's code hash, so every new build asks for the password again. A
# certificate that Apple issued (Apple Development, or Developer ID) keys the item to the team ID
# instead: the first build signed with it asks once ("Always Allow"), and later builds do not ask.
# A free Apple Development certificate is enough. Xcode makes one: Settings > Accounts > Manage
# Certificates. The identity's name comes from the environment and is never committed.
#
# Usage, on the Mac:
#   PF_SIGNING_IDENTITY="Apple Development" scripts/sign-mac-app.sh ["/Applications/Pulse Factory.app"]
set -euo pipefail

if [ "$(uname -s)" != "Darwin" ]; then
  echo "sign-mac-app: run this on the Mac" >&2
  exit 2
fi

app="${1:-/Applications/Pulse Factory.app}"
identity="${PF_SIGNING_IDENTITY:-}"

if [ -z "$identity" ]; then
  echo "sign-mac-app: PF_SIGNING_IDENTITY is not set. The code-signing identities on this Mac:" >&2
  security find-identity -v -p codesigning >&2 || true
  echo "sign-mac-app: set it to an Apple Development or Developer ID identity, for example:" >&2
  echo "  PF_SIGNING_IDENTITY=\"Apple Development\" $0" >&2
  exit 2
fi

if [ ! -d "$app" ]; then
  echo "sign-mac-app: no app at $app" >&2
  exit 2
fi

codesign --force --deep --sign "$identity" "$app"
codesign --verify --deep --strict --verbose=2 "$app"

# A self-signed identity also signs, but it gets no team ID, and macOS then asks on every build.
team="$(codesign -dv "$app" 2>&1 | sed -n 's/^TeamIdentifier=//p')"
if [ -z "$team" ] || [ "$team" = "not set" ]; then
  echo "sign-mac-app: the signature has no team ID, so macOS will still ask for the password on every" >&2
  echo "sign-mac-app: new build. Use an identity that Apple issued (Apple Development or Developer ID)." >&2
  exit 1
fi

echo "sign-mac-app: $app is signed for team $team."
echo "sign-mac-app: at the first launch, macOS asks once for the login password. Press \"Always Allow\"."

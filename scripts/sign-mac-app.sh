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
# A part of the name is enough when one identity matches it. When several match, `codesign` refuses:
# pass the full name or the SHA-1 hash that `security find-identity -v -p codesigning` prints.
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

# Refuse an identity that Apple did not issue before the app is touched. Its certificate's issuer is one
# of Apple's developer CAs (Worldwide Developer Relations, or Developer ID). The check after signing
# below reads the team ID from the signature itself.
# A SHA-1 hash names the identity by its certificate. Look up its name, because `find-certificate -c`
# matches names only.
name="$identity"
case "$identity" in
  *[!0-9A-Fa-f]* | "") ;;
  *)
    if [ "${#identity}" -eq 40 ]; then
      name="$(security find-identity -v -p codesigning | grep -i "$identity" | sed -n 's/^[^"]*"\(.*\)"$/\1/p' | head -n 1)"
    fi
    ;;
esac
issuer="$(security find-certificate -c "$name" -p 2>/dev/null | openssl x509 -noout -issuer 2>/dev/null || true)"
case "$issuer" in
  *"Apple Worldwide Developer Relations"* | *"Developer ID Certification Authority"*) ;;
  *)
    echo "sign-mac-app: no certificate that Apple issued matches \"$identity\", so the app is left as it is." >&2
    echo "sign-mac-app: a self-signed identity has no team ID, and macOS would still ask on every build." >&2
    exit 1
    ;;
esac

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
echo "sign-mac-app: if the Keychain already holds a token from an ad-hoc build, the first launch asks once"
echo "sign-mac-app: for the login password: press \"Always Allow\". A new pairing asks nothing."

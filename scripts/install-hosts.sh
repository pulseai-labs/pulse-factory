#!/usr/bin/env bash
# Install the session host binary by content hash (r2.s2.w3, G8, audit 9). Called by install.sh after
# the upgrade guard, and callable on its own:
#
#   scripts/install-hosts.sh <share dir> <built pf-host> <check-upgrade json file>
#
# It copies the built binary to `<share>/hosts/<sha256>/pf-host` (mode 0755). A hash directory that
# exists is left as it is: a live host may run from it, and the name says what it holds. It points
# `~/.local/bin/pf-host` at that file with a symlink, so the controller, which looks for `pf-host`
# beside its own binary and canonicalizes the path, finds the content-hash path. It then removes every
# other `<share>/hosts/<sha256>/` directory except those whose `pf-host` path is in the JSON's
# `host_binaries`: a host that runs keeps its binary. It removes no path in `cli_copies`: when a copy
# sits inside a directory it would prune, it removes the host binary and keeps the directory.
# The JSON is the saved answer of `pulse-factory store check-upgrade --json`. It needs `jq`.
set -euo pipefail

usage() {
  echo "usage: install-hosts.sh <share dir> <built pf-host> <check-upgrade json file>" >&2
  exit 2
}
[ "$#" -eq 3 ] || usage
share="$1" built="$2" report="$3"

# Check everything before the first write.
command -v jq >/dev/null || { echo "install-hosts: jq is not on PATH; install jq, then run this again" >&2; exit 1; }
[ -f "$built" ] && [ -x "$built" ] || { echo "install-hosts: $built is not an executable file" >&2; exit 1; }
[ -f "$report" ] || { echo "install-hosts: $report is not a file" >&2; exit 1; }
jq -e '.host_binaries and .cli_copies' "$report" >/dev/null ||
  { echo "install-hosts: $report holds no host_binaries and cli_copies lists" >&2; exit 1; }

mkdir -p "$share/hosts"
share="$(cd "$share" && pwd -P)"
sha="$(sha256sum "$built" | cut -d' ' -f1)"
dest="$share/hosts/$sha"
if [ ! -d "$dest" ]; then
  # Made beside its name and moved into place, so no one sees a half-copied directory.
  tmp="$(mktemp -d "$share/hosts/.new.XXXXXX")"
  install -m 0755 "$built" "$tmp/pf-host"
  mv "$tmp" "$dest"
fi

bin_dir="${HOME:?HOME is not set}/.local/bin"
mkdir -p "$bin_dir"
ln -sfn "$dest/pf-host" "$bin_dir/pf-host"

mapfile -t keep < <(jq -r '.host_binaries[]' "$report")
mapfile -t copies < <(jq -r '.cli_copies[]' "$report")
for dir in "$share"/hosts/*/; do
  dir="${dir%/}"
  name="${dir##*/}"
  [[ "$name" =~ ^[0-9a-f]{64}$ ]] || continue
  [ "$name" != "$sha" ] || continue
  named=0
  for path in "${keep[@]}"; do
    [ "$path" != "$dir/pf-host" ] || named=1
  done
  [ "$named" -eq 0 ] || continue
  protected=0
  for path in "${copies[@]}"; do
    case "$path" in "$dir"/*) protected=1 ;; esac
  done
  if [ "$protected" -eq 1 ]; then
    rm -f "$dir/pf-host"
  else
    rm -rf "$dir"
  fi
done

#!/bin/sh
# stale-pass.sh <review_file> <forge_repo> <branch>
#
# The stale-pass test of the review stage. It exits 0 (the pass is stale: review again) when the
# first line of <review_file> is exactly `verdict: pass` (`verdict.sh` reads it) and the second
# line, `head: <sha>`, is not
# the PR's current head. It also exits 0 when the head line is missing or malformed, or when the
# current head cannot be read: it fails closed, and the stage's cap then parks the unit.
# It exits 1 when the verdict is not `pass`, or when the heads are equal.
#
# <forge_repo> that starts with `/` is a local bare repository: the head is its branch ref.
# Any other <forge_repo> is `owner/name` on the forge: the head comes from `gh pr view`.
# Nothing here calls a method tool.
set -eu

[ "$#" -eq 3 ] || {
    printf 'stale-pass.sh: needs <review_file> <forge_repo> <branch>\n' >&2
    exit 2
}
review=$1
repo=$2
branch=$3

sh "$(dirname -- "$0")/verdict.sh" "$review" pass || exit 1

second=$(sed -n '2p' -- "$review")
case $second in
"head: "*) reviewed=${second#"head: "} ;;
*) exit 0 ;;
esac
case $reviewed in
'' | *[!0-9A-Fa-f]*) exit 0 ;;
esac

case $repo in
/*) current=$(git -C "$repo" rev-parse "refs/heads/$branch" 2>/dev/null) || exit 0 ;;
*) current=$(gh pr view "$branch" --repo "$repo" --json headRefOid --jq .headRefOid 2>/dev/null) || exit 0 ;;
esac
case $current in
'' | *[!0-9A-Fa-f]*) exit 0 ;;
esac

lower() { printf '%s' "$1" | tr 'A-F' 'a-f'; }
if [ "$(lower "$reviewed")" = "$(lower "$current")" ]; then
    exit 1
fi
exit 0

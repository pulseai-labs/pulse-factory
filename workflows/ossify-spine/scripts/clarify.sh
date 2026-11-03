#!/bin/sh
# clarify.sh <handoff> <body_file> <answer_file> <request> <generation> <spine_dir>
#
# The answer hook of a `gaps` park. It appends the operator's answer to the work item handoff's
# `## Clarifications`, which ossify reads again on every dispatch (handoff contract section 3).
# It only appends: no byte of the handoff is rewritten. It checks everything first and writes
# once, so a refusal leaves the handoff as it was.
#
# Every text here is data. The gap list and the answer reach this script as files and as words
# of argv. Nothing below passes them to a shell or to `eval`.
set -eu

die() {
    printf 'clarify.sh: %s\n' "$*" >&2
    exit 1
}

[ "$#" -eq 6 ] || die "needs <handoff> <body_file> <answer_file> <request> <generation> <spine_dir>"
handoff=$1
body=$2
answer=$3
request=$4
generation=$5
spine_dir=$6

case $request in
R-[0-9]*) ;;
*) die "the request ID is not R-<n>" ;;
esac
case $generation in
'' | *[!0-9]*) die "the claim generation is not a number" ;;
esac

# The handoff must be a plain file inside the spine directory.
case $handoff in
/*) ;;
*) die "the handoff path is not absolute" ;;
esac
[ -d "$spine_dir" ] || die "the spine directory does not exist"
[ -f "$handoff" ] || die "the handoff is not a file"
[ ! -L "$handoff" ] || die "the handoff is a symbolic link"
spine_real=$(cd "$spine_dir" && pwd -P) || die "cannot resolve the spine directory"
handoff_dir=$(cd "$(dirname -- "$handoff")" && pwd -P) || die "cannot resolve the handoff directory"
case "$handoff_dir/" in
"$spine_real"/*) ;;
*) die "the handoff is outside the spine directory" ;;
esac
[ -f "$body" ] || die "the request body file is missing"
[ -f "$answer" ] || die "the answer file is missing"
[ -w "$handoff" ] || die "the handoff cannot be written"

# Read everything before the first write. One line of each gap: a seat wrote these fields, so a
# line break in one is flattened and cannot start a heading of its own.
gaps=$(jq -r '
    def flat: tostring | gsub("[\r\n]+"; " ");
    (.gaps // [])[]
    | "- **\(.section // "-" | flat), \(.severity // "-" | flat):** *\(.question // "" | flat)*"
' "$body") || die "the request body is not readable as JSON"
gaps_error=$(jq -r '
    select(.gaps_error != null)
    | "- (the gap list could not be read: \(.gaps_error | tostring | gsub("[\r\n]+"; " ")))"
' "$body") || die "the request body is not readable as JSON"
text=$(cat -- "$answer") || die "the answer file cannot be read"
count=$(grep -c '^### Dispatch ' "$handoff" || true)
n=$((count + 2))
date_utc=$(date -u +%Y-%m-%d)

entry=$(mktemp) || die "cannot make a temporary file"
trap 'rm -f "$entry"' EXIT HUP INT TERM
{
    # Start on a fresh line, whatever the file ends with.
    if [ -n "$(tail -c 1 "$handoff")" ]; then
        printf '\n'
    fi
    if ! grep -qx '## Clarifications' "$handoff"; then
        printf '\n## Clarifications\n'
    fi
    printf '\n### Dispatch %s — %s (%s, claim %s)\n\n' "$n" "$date_utc" "$request" "$generation"
    if [ -n "$gaps" ]; then
        printf '%s\n' "$gaps"
    fi
    if [ -n "$gaps_error" ]; then
        printf '%s\n' "$gaps_error"
    fi
    printf '\n'
    printf '→ %s\n' "$text" | sed 's/^/> /'
} >"$entry" || die "cannot compose the entry"

cat -- "$entry" >>"$handoff" || die "cannot append to the handoff"

#!/bin/sh
# review-prep.sh <review_file> <review_prev_file>
#
# The `before` step of the review stage. If <review_file> is a plain file whose first line is
# `verdict: pass` or `verdict: block` (`verdict.sh <review_file> any` exits 0), it replaces
# <review_prev_file>. A symbolic link is never followed. Then <review_file> is removed,
# so the stage's `done` test sees only a review written in this session. A review that is missing or
# malformed never overwrites <review_prev_file>. Nothing here calls a method tool.
set -eu

die() {
    printf 'review-prep.sh: %s\n' "$*" >&2
    exit 1
}

[ "$#" -eq 2 ] || die "needs <review_file> <review_prev_file>"
review=$1
prev=$2
case $review in
/*) ;;
*) die "the review path is not absolute" ;;
esac
case $prev in
/*) ;;
*) die "the previous-review path is not absolute" ;;
esac
[ ! -L "$prev" ] || die "the previous-review path is a symbolic link"
if [ -e "$prev" ] && [ ! -f "$prev" ]; then
    die "the previous-review path is not a file"
fi

if sh "$(dirname -- "$0")/verdict.sh" "$review" any; then
    tmp=$(mktemp "$prev.XXXXXX") || die "cannot make a temporary file"
    trap 'rm -f "$tmp"' EXIT HUP INT TERM
    cp -- "$review" "$tmp" || die "cannot copy the review"
    mv -f -- "$tmp" "$prev" || die "cannot replace the previous review"
fi
rm -f -- "$review" || die "cannot remove the review file"

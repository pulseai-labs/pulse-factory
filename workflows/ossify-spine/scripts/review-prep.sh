#!/bin/sh
# review-prep.sh <review_file> <review_prev_file>
#
# The `before` step of the review stage. If <review_file> is a plain file whose first line is
# `verdict: pass` or `verdict: block`, it replaces <review_prev_file>. Then <review_file> is removed,
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

if [ -f "$review" ] && [ ! -L "$review" ]; then
    first=$(head -n 1 -- "$review") || first=
    case $first in
    "verdict: pass" | "verdict: block")
        tmp=$(mktemp "$prev.XXXXXX") || die "cannot make a temporary file"
        trap 'rm -f "$tmp"' EXIT HUP INT TERM
        cp -- "$review" "$tmp" || die "cannot copy the review"
        mv -f -- "$tmp" "$prev" || die "cannot replace the previous review"
        ;;
    *) ;;
    esac
fi
rm -f -- "$review" || die "cannot remove the review file"

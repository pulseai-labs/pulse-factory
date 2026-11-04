#!/bin/sh
# verdict.sh <file> <pass|block|any>
#
# The one reader of the review verdict. The reviewer writes the verdict on line 1 of the review file
# (`verdict: pass` or `verdict: block`) and the head on line 2. This script reads line 1 and nothing
# else: a later line never counts, and never changes the answer.
#
# It exits 0 when <file> is a plain file (not a symbolic link) and line 1 is exactly `verdict: pass`
# (mode `pass`), exactly `verdict: block` (mode `block`), or exactly one of the two (mode `any`).
# It exits 1 in every other case: no file, a symbolic link, a directory, an empty file, or any other
# line 1, a trailing space or a carriage return included. A one-line file with no final newline still
# has a line 1.
# It exits 2 on a usage error (not two arguments, or another mode), with one line on stderr.
#
# The stage `done` test, the block outcome, `stale-pass.sh` and `review-prep.sh` all call it.
# Nothing here calls a method tool.
set -eu

usage() {
    printf 'verdict.sh: needs <file> <pass|block|any>\n' >&2
    exit 2
}

[ "$#" -eq 2 ] || usage
file=$1
mode=$2
case $mode in
pass | block | any) ;;
*) usage ;;
esac

[ -f "$file" ] && [ ! -L "$file" ] || exit 1

# `sed` returns line 1 whether or not the file ends in a newline. `read` would fail under `set -e`
# on a file with no final newline.
first=$(sed -n '1{p;q;}' -- "$file") || exit 1

case $mode in
pass) [ "$first" = "verdict: pass" ] || exit 1 ;;
block) [ "$first" = "verdict: block" ] || exit 1 ;;
any)
    [ "$first" = "verdict: pass" ] || [ "$first" = "verdict: block" ] || exit 1
    ;;
esac
exit 0

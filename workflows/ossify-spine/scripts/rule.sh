#!/bin/sh
# rule.sh <rulings_file> <stop_file> <answer_file> <request> <generation> <stage>
#
# The answer hook of a stop file. It appends the operator's ruling to the unit's rulings file,
# which every brief tells a seat to read first. Then it retires the stop file, so the next launch
# does not park on it again.
#
# The stop text was written by a seat, so every line of it is quoted: a seat cannot start a
# heading of its own. Nothing here passes a text to a shell or to `eval`.
set -eu

die() {
    printf 'rule.sh: %s\n' "$*" >&2
    exit 1
}

[ "$#" -eq 6 ] || die "needs <rulings_file> <stop_file> <answer_file> <request> <generation> <stage>"
rulings=$1
stop=$2
answer=$3
request=$4
generation=$5
stage=$6

case $request in
R-[0-9]*) ;;
*) die "the request ID is not R-<n>" ;;
esac
case $generation in
'' | *[!0-9]*) die "the claim generation is not a number" ;;
esac
case $stage in
'' | *[!A-Za-z0-9._-]*) die "the stage name holds a character other than A-Z a-z 0-9 . _ -" ;;
esac
[ -f "$answer" ] || die "the answer file is missing"
case $rulings in
/*) ;;
*) die "the rulings path is not absolute" ;;
esac
[ ! -L "$rulings" ] || die "the rulings file is a symbolic link"
if [ -e "$rulings" ] && [ ! -f "$rulings" ]; then
    die "the rulings path is not a file"
fi

# The stop file may be gone already: an operator can remove it after reading it.
have_stop=no
if [ -e "$stop" ] || [ -L "$stop" ]; then
    [ -f "$stop" ] && [ ! -L "$stop" ] || die "the stop path is not a plain file"
    have_stop=yes
fi

text=$(cat -- "$answer") || die "the answer file cannot be read"
if [ "$have_stop" = yes ]; then
    stop_text=$(head -c 65536 -- "$stop") || die "the stop file cannot be read"
else
    stop_text='(the stop file was already removed)'
fi
time_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
mkdir -p -- "$(dirname -- "$rulings")" || die "cannot make the directory of the rulings file"

entry=$(mktemp) || die "cannot make a temporary file"
trap 'rm -f "$entry"' EXIT HUP INT TERM
{
    # Start on a fresh line, whatever the file ends with.
    if [ -s "$rulings" ] && [ -n "$(tail -c 1 "$rulings")" ]; then
        printf '\n'
    fi
    printf '\n## %s — %s — stage %s — claim %s\n\n' "$request" "$time_utc" "$stage" "$generation"
    printf '%s\n' "$stop_text" | sed 's/^/> /'
    printf '\n%s\n' "$text"
} >"$entry" || die "cannot compose the entry"

cat -- "$entry" >>"$rulings" || die "cannot append to the rulings file"
if [ "$have_stop" = yes ]; then
    mv -f -- "$stop" "$stop.answered-$request" || die "cannot rename the stop file"
fi

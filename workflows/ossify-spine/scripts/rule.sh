#!/bin/sh
# rule.sh <rulings_file> <stop_file|-> <answer_file> <request> <generation> <stage> [<source>]
#
# An answer hook. It appends the operator's ruling to the unit's rulings file, which every brief
# tells a seat to read first. For a stop file it also retires the file, so the next launch does not
# park on it again.
#
# A stop path of `-` means the park has no file (a stall, a cap, a failed launch): the entry quotes
# nothing and renames nothing, and the ruling is a note for the next seat.
#
# <source> is the controller's `{answer.source}`: `typed`, or `option:<id>:<by>` when the operator
# took an option of the request. A hook with no source reads `typed`. For an option the heading
# says who wrote the words, so a seat reads "the seat's option, pressed under claim g" and not an
# operator's sentence. The words of an option are data a seat, a workflow or the controller wrote.
#
# The stop text was written by a seat, so every line of it is quoted: a seat cannot start a
# heading of its own. Nothing here passes a text to a shell or to `eval`.
set -eu

die() {
    printf 'rule.sh: %s\n' "$*" >&2
    exit 1
}

[ "$#" -eq 6 ] || [ "$#" -eq 7 ] || die "needs <rulings_file> <stop_file|-> <answer_file> <request> <generation> <stage> [<source>]"
rulings=$1
stop=$2
answer=$3
request=$4
generation=$5
stage=$6
source=typed
if [ "$#" -eq 7 ]; then
    source=$7
fi

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
# The source is `typed`, or `option:<id>:<by>`: an ID of 1 to 32 characters of a-z 0-9 _ -, and the
# one who wrote the option.
option_id=
option_by=
case $source in
typed) ;;
option:*:*)
    rest=${source#option:}
    option_id=${rest%%:*}
    option_by=${rest#*:}
    case $option_id in
    '' | *[!a-z0-9_-]*) die "the source names an option ID that is not 1 to 32 characters of a-z 0-9 _ -" ;;
    esac
    [ "${#option_id}" -le 32 ] || die "the source names an option ID of more than 32 characters"
    case $option_by in
    controller | workflow | seat) ;;
    *) die "the source names an option written by neither the controller, the workflow nor the seat" ;;
    esac
    ;;
*) die "the source is neither typed nor option:<id>:<by>" ;;
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

# The stop file may be gone already: an operator can remove it after reading it. A stop path of
# `-` is a park with no file: there is nothing to quote or rename.
have_stop=no
no_file=no
if [ "$stop" = - ]; then
    no_file=yes
elif [ -e "$stop" ] || [ -L "$stop" ]; then
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
    if [ -n "$option_id" ]; then
        printf '\n## %s — %s — stage %s — claim %s — option %s, written by the %s\n\n' \
            "$request" "$time_utc" "$stage" "$generation" "$option_id" "$option_by"
    else
        printf '\n## %s — %s — stage %s — claim %s\n\n' "$request" "$time_utc" "$stage" "$generation"
    fi
    if [ "$no_file" = no ]; then
        printf '%s\n' "$stop_text" | sed 's/^/> /'
        printf '\n'
    fi
    printf '%s\n' "$text"
} >"$entry" || die "cannot compose the entry"

cat -- "$entry" >>"$rulings" || die "cannot append to the rulings file"
if [ "$have_stop" = yes ]; then
    mv -f -- "$stop" "$stop.answered-$request" || die "cannot rename the stop file"
fi

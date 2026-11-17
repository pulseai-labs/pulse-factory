#!/bin/sh
# stranded.sh <worktree> <branch> <base_sha> <unit_id> <stop_file>
#
# The close stage's `no_signal.run` command. The controller runs it only when the close seat's
# turn ended, the nudge was spent, and the item PR is still not open. So it always writes the stop
# file: one stop that says the close seat ended before the item PR opened and what the worktree
# holds, so the operator can answer it and the stage runs again. A seat that did no work leaves a
# stop that says so.
#
# It reads the worktree with four git reads and writes nothing but the stop file:
#   git diff --cached --name-only   the staged files
#   git status --porcelain          unstaged and untracked paths (without an optional lock)
#   git rev-list <base>..HEAD       the commits past the base
#   git ls-remote origin <ref>      whether `origin` holds the branch
# It never runs `git add`, commit or push, and never calls the forge.
#
# Exit 0 when it wrote the stop file. Exit 1 when it cannot read the worktree or cannot write the
# stop file; the controller's own park for the stage then applies. The stop file's answer hook is
# `rule.sh`, which quotes every line, so the text here can hold a seat's file names safely. Nothing
# here passes a text to a shell or to `eval`.
set -eu

die() {
    printf 'stranded.sh: %s\n' "$*" >&2
    exit 1
}

[ "$#" -eq 5 ] || die "needs <worktree> <branch> <base_sha> <unit_id> <stop_file>"
worktree=$1
branch=$2
base=$3
unit=$4
stop=$5

[ -n "$branch" ] || die "the branch is empty"
[ -n "$unit" ] || die "the unit ID is empty"
[ -n "$stop" ] || die "the stop file path is empty"
[ -d "$worktree" ] || die "the worktree is not a directory: $worktree"
inside=$(git -C "$worktree" rev-parse --is-inside-work-tree 2>/dev/null) || inside=
[ "$inside" = true ] || die "the worktree is not a git work tree: $worktree"

# Refuse a stop path that is a link or not a plain file: a write must land only where the stage
# reads. A missing file is the normal case.
if [ -e "$stop" ] || [ -L "$stop" ]; then
    [ -f "$stop" ] && [ ! -L "$stop" ] || die "the stop path is not a plain file: $stop"
fi
[ -d "$(dirname -- "$stop")" ] || die "the directory of the stop file does not exist"

# The first line of a message, at most 200 characters. A git error is data, not a command.
first_line() {
    printf '%s\n' "$1" | head -n 1 | cut -c 1-200
}

# plural <n> <word> prints "1 word" or "<n> words".
plural() {
    if [ "$1" -eq 1 ]; then printf '%s %s' "$1" "$2"; else printf '%s %ss' "$1" "$2"; fi
}

# join_first <n> <list>: the first <n> lines of a list, joined with ", ".
join_first() {
    printf '%s\n' "$2" | head -n "$1" | awk 'NR > 1 { printf ", " } { printf "%s", $0 } END { printf "\n" }'
}

# ---- Staged files.
staged=$(git -C "$worktree" diff --cached --name-only 2>/dev/null) || staged=
staged_count=0
[ -z "$staged" ] || staged_count=$(printf '%s\n' "$staged" | wc -l | tr -d ' ')

# ---- Unstaged and untracked paths. `--no-optional-locks` keeps `status` from refreshing the index.
status_out=$(git --no-optional-locks -C "$worktree" status --porcelain 2>/dev/null) || status_out=
# A line is `XY path`. A path counts here when its worktree column (Y) is not blank, or it is
# untracked (`??`). The path starts at column 4.
unstaged=$(printf '%s\n' "$status_out" | awk 'length($0) > 3 && (substr($0, 2, 1) != " " || substr($0, 1, 2) == "??") { print substr($0, 4) }')
unstaged_count=0
[ -z "$unstaged" ] || unstaged_count=$(printf '%s\n' "$unstaged" | wc -l | tr -d ' ')

# ---- Commits past the base. A base that is not a hex SHA never reaches git: it could be an option.
commits_note=
commits=0
case $base in
'' | *[!0-9a-fA-F]*) commits_note="commits not read: the base is not a SHA" ;;
*)
    if rev_out=$(git -C "$worktree" rev-list "$base..HEAD" 2>&1); then
        [ -z "$rev_out" ] || commits=$(printf '%s\n' "$rev_out" | wc -l | tr -d ' ')
    else
        commits_note="commits not read: $(first_line "$rev_out")"
    fi
    ;;
esac

# ---- Does origin hold the branch? The full ref name is matched exactly, because `ls-remote` matches
# the tail of a ref and a tag named like the branch would answer.
origin_note=
origin_has=no
if ls_out=$(GIT_TERMINAL_PROMPT=0 git -C "$worktree" ls-remote origin "refs/heads/$branch" 2>&1); then
    if printf '%s\n' "$ls_out" | awk -v ref="refs/heads/$branch" '$2 == ref { found = 1 } END { exit !found }'; then
        origin_has=yes
    fi
else
    origin_note="origin not read: $(first_line "$ls_out")"
fi

# ---- Compose the stop text. It is built in memory: the stop file is the only file written.
compose() {
    printf 'close of %s on branch %s: the close seat ended before the item PR opened.\n' "$unit" "$branch"

    if [ "$staged_count" -eq 0 ] && [ "$unstaged_count" -eq 0 ] && [ "$commits" -eq 0 ] \
        && [ -z "$commits_note" ] && [ "$origin_has" = no ] && [ -z "$origin_note" ]; then
        printf 'nothing staged, no commit past the base, no branch on origin\n'
    else
        if [ "$staged_count" -eq 0 ]; then
            printf 'nothing staged\n'
        elif [ "$staged_count" -le 10 ]; then
            printf 'staged: %s: %s\n' "$(plural "$staged_count" file)" "$(join_first 10 "$staged")"
        else
            printf 'staged: %s; the first 10: %s\n' "$(plural "$staged_count" file)" "$(join_first 10 "$staged")"
        fi
        if [ "$unstaged_count" -eq 0 ]; then
            printf 'no unstaged change\n'
        elif [ "$unstaged_count" -le 10 ]; then
            printf 'unstaged: %s: %s\n' "$(plural "$unstaged_count" path)" "$(join_first 10 "$unstaged")"
        else
            printf 'unstaged: %s; the first 10: %s\n' "$(plural "$unstaged_count" path)" "$(join_first 10 "$unstaged")"
        fi
        if [ -n "$commits_note" ]; then
            printf '%s\n' "$commits_note"
        elif [ "$commits" -eq 0 ]; then
            printf 'no commit past the base\n'
        else
            printf '%s past the base\n' "$(plural "$commits" commit)"
        fi
        if [ -n "$origin_note" ]; then
            printf '%s\n' "$origin_note"
        elif [ "$origin_has" = yes ]; then
            printf 'origin has the branch\n'
        else
            printf 'no branch on origin\n'
        fi
    fi
    printf 'answer to run close again; the ruling is appended to rulings.md\n'
}
text=$(compose) || die "cannot compose the stop text"

printf '%s\n' "$text" >"$stop" || die "cannot write the stop file: $stop"

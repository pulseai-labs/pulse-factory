#!/usr/bin/env bash
# The shared paper env and chat wrapper of the start-from-chat demos: d22 (`start-api.sh`), the card's
# journey (`ui-start.sh`) and w3's d23 all run it first.
#
#   PF_BIN=... PF_REPLAY_BIN=... PF_STATE_DIR=... PF_SHARE_DIR=... scripts/demo/start-env.sh <dir>
#
# `<dir>` is an empty directory under the caller's temp directory. `PF_START_FIXTURE` names the recorded
# chat (default: `crates/pf-adapter-claude/fixtures/start_proposal.jsonl`). The script builds nothing,
# calls no `gh`, prints no secret and writes only under `<dir>` and `$PF_SHARE_DIR/workflows/`. It makes:
#
#   <dir>/paper                      a copy of scripts/demo/paper-d20 with the shipped rule.sh in
#                                    workflow/scripts/, as d20 makes it
#   <dir>/paper/forge.git            a bare forge with `main` and `spine-s1` on one base commit
#   <dir>/spine, <dir>/worktrees     the spine directory and the item worktrees
#   <dir>/ledger.md                  the run ledger
#   $PF_SHARE_DIR/workflows/paper-spine-d20
#                                    the paper workflow, installed by name
#   <dir>/claude-wrapper             the chat process. A caller sets `PF_CHAT_CLAUDE_BIN` to it on the
#                                    controller: that setting points the proposal at the paper package.
#
# The wrapper answers `--version` by `exec`ing the replay. Otherwise it starts one background child, then
# `exec`s the replay of the recorded chat. The child names `PF_STATE_DIR` and `PF_SHARE_DIR` itself,
# because the adapter passes no `PF_` name. It reads its session from `PULSE_FACTORY_CHAT` and waits (at
# most 60 s) until `session items` of that session holds an `operator_message`. Then, from below the chat,
# it runs `propose` (the call the recorded `Bash` call stands for) and an `arm` with the same inputs, which
# the controller must refuse. Each verb writes `<dir>/chat/<verb>.{rc,out,err}`.
#
# The close file is `<dir>/spine/closed.flag`: paper-d20's `end_when` waits for it, so a script that
# writes it records the operator's close, and the armed run ends.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"

fail() { echo "start-env: FAIL: $*" >&2; exit 1; }

[ "$#" -eq 1 ] || fail "usage: start-env.sh <dir>"
dir="$1"
for var in PF_BIN PF_REPLAY_BIN PF_STATE_DIR PF_SHARE_DIR; do
  [ -n "${!var:-}" ] || fail "$var is not set"
done
for tool in git cp chmod mkdir; do
  command -v "$tool" >/dev/null || fail "$tool is not on PATH (this script needs it)"
done
abs() {
  local p
  p="$(command -v "$1")" || fail "$1 is not an executable"
  case "$p" in /*) printf '%s' "$p" ;; *) printf '%s' "$PWD/$p" ;; esac
}
pf_bin="$(abs "$PF_BIN")"
replay_bin="$(abs "$PF_REPLAY_BIN")"
fixture="${PF_START_FIXTURE:-$repo_root/crates/pf-adapter-claude/fixtures/start_proposal.jsonl}"
src="$here/paper-d20"
shipped_scripts="$repo_root/workflows/ossify-spine/scripts"
[ -f "$fixture" ] || fail "no fixture at $fixture"
[ -d "$src/workflow" ] || fail "no paper workflow at $src"
[ -f "$shipped_scripts/rule.sh" ] || fail "no rule.sh at $shipped_scripts"
if [ -e "$dir" ] && [ -n "$(ls -A "$dir" 2>/dev/null)" ]; then fail "$dir is not empty"; fi
mkdir -p "$dir"
dir="$(cd "$dir" && pwd)"
case "$PF_SHARE_DIR" in /*) share="$PF_SHARE_DIR" ;; *) share="$PWD/$PF_SHARE_DIR" ;; esac
case "$PF_STATE_DIR" in /*) state="$PF_STATE_DIR" ;; *) state="$PWD/$PF_STATE_DIR" ;; esac

# ------------------------------------------------------------------ the paper env and its forge
paper="$dir/paper"
mkdir -p "$paper" "$dir/spine" "$dir/worktrees" "$dir/chat"
cp -R "$src/workflow" "$paper/workflow"
cp -R "$src/seats" "$paper/seats"
chmod +x "$paper"/seats/*.sh
# The answer hook of a stop file is the shipped one: the demo runs the package's own `rule.sh`.
mkdir -p "$paper/workflow/scripts"
cp "$shipped_scripts/rule.sh" "$paper/workflow/scripts/rule.sh"
: > "$dir/ledger.md"

base="main"
branch="spine-s1"
forge="$paper/forge.git"
git init -q --bare -b "$base" "$forge"
git init -q -b "$base" "$dir/seed"
echo "base" > "$dir/seed/README.md"
git -C "$dir/seed" add README.md
git -C "$dir/seed" -c user.name=paper -c user.email=paper@example.invalid commit -q -m "base root"
git -C "$dir/seed" push -q "$forge" "$base"
git -C "$dir/seed" push -q "$forge" "$base:refs/heads/$branch"

# The package, installed by name: `propose` takes a name, never a path.
mkdir -p "$share/workflows"
rm -rf "$share/workflows/paper-spine-d20"
cp -R "$paper/workflow" "$share/workflows/paper-spine-d20"

# ------------------------------------------------------------------------- the chat's wrapper
call_args=(s1 --workflow paper-spine-d20 --ledger "$dir/ledger.md" --paper "$paper"
  --param "spine_branch=$branch" --param "spine_dir=$dir/spine" --param "worktree_root=$dir/worktrees"
  --param "base_branch=$base")
wrapper="$dir/claude-wrapper"
cat > "$wrapper" <<EOF
#!/bin/sh
case "\$1" in --version) exec "$replay_bin" --fixture "$fixture" "\$@" ;; esac
(
  export PF_STATE_DIR="$state" PF_SHARE_DIR="$share"
  i=0
  until "$pf_bin" session items "\$PULSE_FACTORY_CHAT" --json 2>/dev/null | grep -q '"kind": *"operator_message"'; do
    i=\$((i + 1))
    [ \$i -lt 600 ] || exit 0
    sleep 0.1
  done
  "$pf_bin" propose ${call_args[*]@Q} --json > "$dir/chat/propose.out" 2> "$dir/chat/propose.err"; echo \$? > "$dir/chat/propose.rc"
  "$pf_bin" arm ${call_args[*]@Q} --yes > "$dir/chat/arm.out" 2> "$dir/chat/arm.err"; echo \$? > "$dir/chat/arm.rc"
) < /dev/null > /dev/null 2>&1 &
exec "$replay_bin" --fixture "$fixture" "\$@"
EOF
chmod +x "$wrapper"

echo "start-env: ok (paper $paper, wrapper $wrapper, close file $dir/spine/closed.flag)"

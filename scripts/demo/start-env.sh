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
#   <dir>/paper                      a copy of scripts/demo/paper-d20 (or of `$PAPER_SRC`, when set) with
#                                    the shipped rule.sh in workflow/scripts/, as d20 makes it
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
#
# `PF_WAKE_SETTLE=1` (d35, `orchestrator-settles.sh`) changes the wrapper and nothing else. The wrapper
# counts its launches in `<dir>/chat/launches`. Launch 1 does what the paragraphs above say. Launch 2
# and later `exec` the replay of `PF_WAKE_FIXTURE` (default `plain_turn.jsonl`: the replay has one turn
# for each fixture, and the start fixture's turn was used by launch 1) and start `<dir>/wake-child.sh`
# below the chat. That child waits (at most 240 s) until `session items` of its session holds a `notice`
# item with the code `wake`. From below the chat it runs `settle <R-n> --rev <k> --act clear --reason
# "<text>"` for each request line of the block, and then the same call for each other open request that
# `requests --json` lists (the question, which no wake block holds). It writes
# `<dir>/chat/settle-<R-n>.{rc,out,err}`. Then it waits (at most 240 s) for `<dir>/chat/after-revoke`
# and runs the stall's settle once more, into `<dir>/chat/settle-again.{rc,out,err}`. It needs `jq`.
# With the variable unset, the script does what it did before.
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
# `PAPER_SRC` names another paper package to copy (d32 passes `scripts/demo/paper-options`). The default is
# `paper-d20`, so d22, d23 and `ui-start.sh` do not change.
src="${PAPER_SRC:-$here/paper-d20}"
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
if [ "${PF_WAKE_SETTLE:-}" = 1 ]; then
  wake_fixture="${PF_WAKE_FIXTURE:-$repo_root/crates/pf-adapter-claude/fixtures/plain_turn.jsonl}"
  [ -f "$wake_fixture" ] || fail "no wake fixture at $wake_fixture"
  jq_bin="$(abs jq)"
  # The child of launch 2 and later: it settles below the chat, as a woken model would.
  cat > "$dir/wake-child.sh" <<EOF
#!/bin/sh
export PF_STATE_DIR="$state" PF_SHARE_DIR="$share"
chat="$dir/chat"
i=0
block=""
while :; do
  block="\$("$pf_bin" session items "\$PULSE_FACTORY_CHAT" --json 2>/dev/null \\
    | "$jq_bin" -r '[.. | objects | select(.body? and .body.kind? == "notice" and .body.code? == "wake")][0].body.text // empty' 2>/dev/null)" || block=""
  [ -z "\$block" ] || break
  i=\$((i + 1))
  [ \$i -lt 2400 ] || exit 0
  sleep 0.1
done
printf '%s\n' "\$block" > "\$chat/wake-block.txt"
settle_one() {
  "$pf_bin" settle "\$1" --rev "\$2" --act clear --reason "\$4" > "\$chat/\$3.out" 2> "\$chat/\$3.err"
  echo \$? > "\$chat/\$3.rc"
}
# Each request line of the block: the stall.
printf '%s\n' "\$block" | grep '^request=' | while IFS= read -r line; do
  r="\${line#request=}"
  r="\${r%% *}"
  rev="\$(printf '%s' "\$line" | sed -n 's/^request=[^ ]* revision=\([0-9][0-9]*\) .*/\1/p')"
  [ -s "\$chat/stall-request" ] || echo "\$r \$rev" > "\$chat/stall-request"
  settle_one "\$r" "\$rev" "settle-\$r" "paper demo: the stall is cleared"
done
# Each other open request: the question, which the block does not hold.
"$pf_bin" requests --json 2>/dev/null \\
  | "$jq_bin" -r '.requests[] | select(.status == "open") | "\(.id) \(.revision)"' 2>/dev/null \\
  | while read -r r rev; do
    [ ! -e "\$chat/settle-\$r.rc" ] || continue
    settle_one "\$r" "\$rev" "settle-\$r" "paper demo: follow the text of the stall"
  done
# After the grant is revoked, the stall's settle once more.
i=0
until [ -e "\$chat/after-revoke" ]; do
  i=\$((i + 1))
  [ \$i -lt 2400 ] || exit 0
  sleep 0.1
done
read -r r rev < "\$chat/stall-request" || exit 0
settle_one "\$r" "\$rev" settle-again "paper demo: settle after the revoke"
EOF
  chmod +x "$dir/wake-child.sh"
  cat > "$wrapper" <<EOF
#!/bin/sh
case "\$1" in --version) exec "$replay_bin" --fixture "$fixture" "\$@" ;; esac
n=0
[ ! -s "$dir/chat/launches" ] || n=\$(cat "$dir/chat/launches")
n=\$((n + 1))
echo \$n > "$dir/chat/launches"
if [ \$n -ge 2 ]; then
  "$dir/wake-child.sh" < /dev/null > /dev/null 2>&1 &
  exec "$replay_bin" --fixture "$wake_fixture" "\$@"
fi
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
else
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
fi
chmod +x "$wrapper"

echo "start-env: ok (paper $paper, wrapper $wrapper, close file $dir/spine/closed.flag)"

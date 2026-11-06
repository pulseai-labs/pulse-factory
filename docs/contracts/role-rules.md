# Role rule files contract

A **role rule file** says what one role of a workflow may do beyond the base rules. It holds the least
privilege for a seat: a role gets exactly what it lists and nothing more. `pf_workflow::rules::RoleRules`
is the parsed type. `r0.s1.w3`'s `pf-rules` reads these files and adds the deny list.

## Location

One file per role, at the path that the role's `rules` field names in the workflow file. The package
puts them in `rules/<role>.yaml`.

## Format

```yaml
version: 1
role: close
allow:
  - delete_own_scratch
  - push_own_branch
  - write_own_report_and_run_files
```

| Field | Required | Meaning |
|---|---|---|
| `version` | yes | The integer `1`. |
| `role` | yes | The role's name. It must equal the file name without its extension. |
| `allow` | yes | A list of allow values. It may be empty. A value may appear once. |

## Allow values

The set is closed. There are five values:

| Value | Meaning |
|---|---|
| `delete_own_scratch` | The seat may delete files in its own scratch directory. |
| `delete_own_build_target` | The seat may delete its own build target directory. |
| `push_own_branch` | The seat may push the branch of its own unit. |
| `push_own_tag` | The seat may push a tag of its own unit. |
| `write_own_report_and_run_files` | The seat may write its report and the files in its own unit's directory under the run directory (`out/<unit>`, and `out/<seed>` for a repeat of a repeating seed). |

A role that does not list a value does not get it. A role that lists no push value may not push.

## Strict reading

The parser is as strict as the workflow file's. It refuses:

- an unknown field;
- a `version` other than `1`. The error names the supported version. The version is read first;
- an unknown allow value. The error lists the known values;
- a repeated allow value;
- a `role` that does not match the file name, so `implementer.yaml` must say `role: implementer`;
- a duplicate key and a missing required field.

Each error names the line and the column of the text it blames.

## Link to the stage

`StageDef.rules` carries the role's name with `prompts_off: true`. The engine passes it to the agent
port (`apply_rules`). The adapter loads the file and builds the real rules.

## What `pf-rules` does with the allow values

`pf_rules::decide` reads a shell command by parsing it (ADR-0005). This is how the allow values meet
a command. A rule name is what the audit line and a deny's reason show.

| Command | Rule | Who |
|---|---|---|
| `rm`, `rmdir`, `unlink` of a path inside the worktree | `delete-in-worktree` | every role with a worktree, listed or not |
| the worktree itself, a path under its `.git`, `rm -rf *` or `.*` in the worktree | `delete-outside-own-paths` (deny) | every role |
| a delete in the scratch | `delete-own-scratch` | `delete_own_scratch` |
| a delete in the build target | `delete-own-build-target` | `delete_own_build_target` |
| `rm "$x"` where the whole call assigns `x` only from `mktemp` or `""` | `delete-own-scratch` | `delete_own_scratch` |
| any other variable, `$x/..`, or a relative path after `cd "$x"` | `delete-unreadable` (deny) | every role |
| a bare `cargo clean` | `cargo-clean` (deny) | every role |
| `find -delete`, `git clean`, `git worktree remove` | as a delete, with scratch or build target only | per listing |
| `trap 'body' SIG` | the body is judged as a command | every role |
| a push of the seat's own branch, from the worktree or the push directory | `push-own-branch` | `push_own_branch` |
| a push whose directory or branch is one quoted variable, or follows a `cd`, `git switch` or `git checkout` to one | `own-branch-unverified` | `push_own_branch` |
| a push from a directory outside the worktree and the push directory | `push-outside-worktree` (deny) | every role |
| a push of another branch | `push-other-branch` (deny) | every role |
| `gh pr review`, `gh pr comment`, `gh pr edit`, `gh pr close`, `gh pr reopen`, `gh pr ready`, `gh pr lock`, `gh pr unlock`, `gh pr update-branch`, `gh issue comment`, `gh issue edit`, `gh issue close`, `gh issue reopen`, `gh issue lock`, `gh issue unlock`, `gh issue delete` | `gh-pr-write` (deny) | every role |
| any `gh` command that is not a read (`gh pr create`, `gh issue create`, `gh release create`, `gh workflow run`, …) | `gh-forge-write` (deny) | the `reviewer` role only |
| `git config <key> <value>` (any scope, `set`, `--add`, `--replace-all`), `git -c <key>=<value>` and `git --config-env <key>=<var>` of a key that redirects git; `git config --rename-section <old> <new>` into one of their sections; `git config --edit`, `-e` and `edit` | `git-config-trust` (deny) | every role |

`git-config-trust` names the keys that redirect git: `core.hooksPath`, `remote.<name>.receivepack`,
`remote.<name>.uploadpack`, `remote.<name>.vcs`, `remote.<name>.pushurl`,
`remote.<name>.proxy`, `url.<base>.insteadOf`, `url.<base>.pushInsteadOf`,
`include.path`, `includeIf.<condition>.path`, and the transport keys that the controller's push reads
from the global config: `core.sshCommand`, `core.gitProxy`, `http.proxy`, `http.<url>.proxy`,
`http.sslVerify`, `http.<url>.sslVerify`, `credential.helper` and `credential.<url>.helper`. They
redirect git, so a seat may not set them, whatever the value: a bare path, a bare host and an empty
value each redirect. A read, a listing and a removal of such a key stay allowed, but only in a shape the rules read exactly:
a key alone (`git config <key>`); one read or removal flag (`--get`, `--get-all`, `--get-regexp`,
`--get-urlmatch`, `--list`, `-l`, `--unset`, `--unset-all`, `--remove-section`); or the verb `get`,
`list`, `unset` or `remove-section` in first place. Scope flags (`--global`, `--local`, `--system`,
`--worktree`) and the options that take a value (`--file <f>`, `--comment`, `--type`, …) may come
before the key. Every other call is denied when any word of it names a key that redirects git: the
rules do not guess which later word is a flag, and a `--` anywhere in such a call is outside every
shape. So a value that equals such a key is denied too. The editor opens a config whose writes the
rules cannot read, so `--edit` is denied too. Git stops reading flags at the first operand, so
`core.hooksPath --unset` and `core.hooksPath --` write the value `--unset` and `--`; the rules read
them as writes. A short flag counts inside a cluster (`-ze`). A write into the repository's
`.git/config` or a hooks directory through a shell redirection or `cp` is not read. That is the known
limit of the redirection paragraph below.

A relative path means the working directory and the directory of every literal `cd` earlier in the
call. The `push_dir` of the seat's context is a second place a push may run from. A literal half of a
push is judged as itself: `git -C "$wt" push origin other` is a push of `other`, and
`git switch main && git push` is a push to `main`.

## The `reviewer` role

The `reviewer` role reads an item PR and writes its verdict (`workflows/ossify-spine/rules/reviewer.yaml`).
It lists one value, `write_own_report_and_run_files`, and no push value. Its stage sets no `worktree`,
so the seat's worktree is empty and matches no path: a write tool inside the item's worktree is
denied (`write-outside-worktree`), although the seat's working directory is that worktree, for
reading. A write tool to its report under `<run>/out/<unit>/` is allowed. Every denial of every seat
holds for it: a merge, a push, a branch delete, `--no-verify`, an AI trailer and the forge writes of
the table above. On top of those, the `reviewer` may run only `gh` reads: `gh pr|issue|run|release|repo|workflow|label|ruleset`
with `view`, `list`, `diff`, `checks` or `status`, `gh search`, `gh status`, `gh auth status`,
`gh version` and `gh api` with the GET method. Any other `gh` command is denied with
`gh-forge-write`, `gh pr create` included. The other roles keep `gh pr create`, which `close` needs to
open the item PR and the spine PR; they meet the `gh` denials of every seat and no more. A forge
write that goes through no `gh` command (`curl`, `git`) is judged by the rules for those commands.

**Known limit (ADR-0005): shell redirection is not guarded by path.** The rules read the words of a
command, not the file a `>` or `>>` opens, so `echo x > <worktree>/file` is allowed for the
`reviewer` as it is for every role. The reviewer's brief (`briefs/review.md`) forbids a redirection
into the worktree. A test (`reviewer_role.rs`) fails if the rules start to guard redirection, so this
paragraph is corrected then. The same holds for the other shell writes, such as `sed -i`, `git add`
and `git commit`: the rules deny a push, not a local change. The write tools of the `reviewer` reach
only `out/<unit>` of the run directory. A shell redirection into another unit's directory is still
this known limit, so the brief is the guard there.

## The grammar of the shell that `pf-rules` reads

`pf-rules` reads a closed set of shell forms (ADR-0005, `SPINE.md` grill lock G11). A command line
reaches the guards only when each part of it is a form in the first table. A form that is not listed
is denied with the rule `unparsed-command`, and the reason names the form. So a shell form that
nobody has seen yet fails closed with no new code. The list is `pf_rules::grammar::ALLOWED` in code,
and a test keeps this page and that list in step.

The list says what the rules can read, not what is safe. A capability gates only its own
operations, such as a push of the seat's own branch or a delete of its scratch. Any other command
line passes when every form in it is listed and no guard denies it. So a form that joins the list
widens what a role may run, and the guards are the only check on it (ADR-0005, known limits amended
2026-11-05).

**The guards run first.** A line that a guard denies keeps the rule of that guard (`gh-pr-merge`,
`force-push`, `delete-outside-own-paths`, and the others). The grammar deny applies only when no
guard denied the line, and it applies before any capability allow. So `git push origin work/u1;
echo ${x:=y}` is denied although the push alone is allowed. The inner text of a `$(…)`, a backtick pair, a `<(…)` or `>(…)`, a `trap` body,
an `eval` string and a `sh -c` string is a command line under the same grammar, and passes the same
guards. A `( … )` subshell, a function body and a `coproc` body are checked the same way.

### Allowed forms

| Form | Shape |
|---|---|
| `simple command` | words, NAME=value and NAME+=value before it, and redirections |
| `pipeline` | simple or compound commands joined by \| or \|&, and ! before a pipeline |
| `list` | pipelines joined by &&, \|\|, ;, a newline or a trailing & |
| `if` | if, then, elif, else, fi |
| `for` | for NAME in WORDS; do …; done |
| `while and until` | while …; do …; done and until …; do …; done |
| `case` | case WORD in PATTERN) …;; esac, with ;& and ;;& |
| `group` | { …; } |
| `subshell` | ( … ), with a body in the grammar |
| `function` | function NAME { … } and NAME() { … }, with a body in the grammar |
| `coproc` | coproc followed by { … }, ( … ), while, until, if, for or case |
| `array literal` | NAME=(…), NAME+=(…), declare -a NAME=(…) and local -a NAME=(…) |
| `conditional` | [[ … ]] |
| `test` | [ … ], test … and every other builtin as a simple command |
| `time` | time and time -p before any command, simple or compound |
| `wrapper` | timeout, nice, ionice, nohup, setsid, stdbuf, command, builtin, sudo, doas, env, xargs, flock and find -exec |
| `literal word` | literal text, single quotes, double quotes, brace expansion and globs |
| `variable` | $NAME, ${NAME}, ${#NAME} and the parameters $? $$ $! $# $@ $* $- $0 to $9 |
| `default word` | ${NAME:-word}, ${NAME-word}, ${NAME:+word}, ${NAME+word}, ${NAME#word}, ${NAME##word}, ${NAME%word} and ${NAME%%word} |
| `arithmetic expansion` | $((…)) holding only names, numbers, spaces and + - * / % < > = ! & \| ( ) |
| `command substitution` | $(…) and backticks, with an inner text in the grammar |
| `process substitution` | <(…) and >(…), with an inner text in the grammar |
| `comment` | # to the end of the line |
| `redirection` | > >> >\| < <> <& >& &> &>> with an optional file descriptor, and a word as the target |
| `heredoc` | <<WORD, <<-WORD and <<<WORD fed to a command that is not an interpreter |
| `trap` | trap 'body' SIGNAL, with a body in the grammar |
| `eval` | eval with literal arguments, read as a command line |

### Denied forms

Each of these is denied as `unparsed-command`, with the form named, unless a guard denied the line
first. A form that is in neither table is denied the same way.

| Form | Shape |
|---|---|
| `$[…]` | the old arithmetic expansion |
| `((…))` | the arithmetic command, and the C-style for ((…;…;…)) |
| `$((…)) outside its list` | $((…)) holding $, [ ], a command substitution or another character |
| `other ${…}` | ${x:=y}, ${x/a/b}, ${!x}, ${x:1:2}, ${x[1]}, ${x@P} and every other form |
| `$'…'` | ANSI-C quoting |
| `$"…"` | locale quoting |
| `select` | select NAME in WORDS; do …; done |
| `exec` | exec, with or without a command |
| `source` | source FILE and . FILE |
| `heredoc to an interpreter` | <<WORD or <<<WORD fed to python, perl, ruby, node and the like |
| `unread wrapper` | taskset, chrt, strace, su, watch, parallel and the like |
| `parse error` | an unclosed quote or substitution, coproc without a body, a redirection without a target, and any text the parser cannot read |

### Long options by unique prefix

Git reads a long option by its exact name or by any unique prefix of it, so `git push --forc` is a
force push. `pf-rules` writes a prefix in full before any guard reads it, for each option a guard
reads (`crate::options`, one constant per command with the man page section as its source). A prefix
that two guarded options share, such as `git push --f`, is denied as `unparsed-command`, because git
refuses it too. `gh` and `cargo` read long options by exact name, and no guard reads a long option of
`curl`, `wget`, `rm` or `find`, so they need no list.

### Known limits of the grammar (ADR-0005: the rails stop mistakes, not an attacker)

The rules read the words of a command line. They do not read what a command later evaluates. These
forms pass the grammar and are not read inside:

- **A name in `$((…))`, `let` and `[[ … -eq … ]]`.** Bash evaluates the value of a variable named
  there as arithmetic, and an arithmetic subscript can run a command substitution. So
  `x='a[$(cmd)]'; echo $((x))` runs `cmd`, and the rules read neither the value of `x` nor `cmd`.
- **A pipe to an interpreter.** The heredoc and herestring forms are denied for an interpreter. A
  pipe into one (`printf … | python3`, `make -f -`) is not. The interpreter list is closed: `make`,
  `sed -f -`, `awk -f -`, `sqlite3` and `ssh` are not on it.
- **A wrapper's long option by a prefix.** `timeout`, `nice`, `ionice`, `time`, `stdbuf`, `sudo`,
  `env`, `xargs` and `flock` read their long options by exact name, and the commands accept a unique
  prefix. So `timeout --kill-aft 5 10 gh pr merge 1` leaves `10` as the command the guards see.

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
| `write_own_report_and_run_files` | The seat may write its report and its files under the run directory. |

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

A relative path means the working directory and the directory of every literal `cd` earlier in the
call. The `push_dir` of the seat's context is a second place a push may run from. A literal half of a
push is judged as itself: `git -C "$wt" push origin other` is a push of `other`, and
`git switch main && git push` is a push to `main`.

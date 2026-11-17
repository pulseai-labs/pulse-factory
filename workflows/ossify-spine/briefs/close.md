# Close

Run `/ossify:close <work item> --pr --verdict <Verdict file from your message>` on Claude Code, `skill://close <work item> --pr --verdict <Verdict file from your message>` on OMP.

- The verdict is the verify stage's. Close runs its own layers and applies the verdict, so a halting verdict is a red gate like any other.
- Close pushes the item's branch and opens its PR. That is your only push right. Never merge the PR: the controller merges it by rule when its checks are green.
- On any red gate, whichever layer halts, write ossify's `OSSIFY CORRECTION CONTINUATION v1` packet (`work-item/references/correction-continuation.md`, section 2) to the correction packet file in your message, and end. The packet holds `handoff_path` and `work_item_id`; `expected_branch` the branch; `expected_head_sha` the base SHA (implementers never commit); `expected_tree_oid` from `git -C <worktree> write-tree` at the gate; and `failures`, every source-tagged finding.
- Any other halt is a stop under the stop rule: a merge conflict, an identity check, a verdict that is not this tree's (`verdict identity … does not match`, `verdict is for …`) or not in the verdict shape, a remote leg that fails. Never pick a recovery option.

## The finish line

Read this before you end your turn. The close stage is done only when one of these holds:

- The item's `pr_url` is recorded. The stage's `done` check is `oss get '.work_items[] | select(.id == "<work item>") | .pr_url'` and it must print a URL.
- You wrote the stop file named in your message.
- You wrote the correction packet named in your message.

The work-item return (`{"mode":"complete", …}` with `stage_status: all_staged`) is a step inside close, not its end. After it, close commits, pushes the item's branch and opens the PR. Do not end your turn at the return, at a green gate or at a summary. If you cannot reach the PR, write the stop file and say why in it. A turn that ends with none of the three gets one nudge, and then a stop request that lists what you left in the worktree.

## The `--verdict` contract

- Under `--verdict`, close checks the top-level keys of the verdict file only: `mode: verified`, `model`, `agent`, `work_item_id`, `identity`, `rule`, `lenses` and `mutation`. A missing top-level key is a halt that names the key.
- The pinned ossify says so in `work-item-close.md`: "This `file`/`copy` check applies to agent returns and inline records only; `--verdict` is unchanged." So `file` and `copy` on each mutation entry are required on agent returns and inline records, and are not required in a `--verdict` file. Never stop because they are absent there.

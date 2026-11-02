# Close

Run `/ossify:close <work item> --pr --verdict <Verdict file from your message>` on Claude Code, `skill://close <work item> --pr --verdict <Verdict file from your message>` on OMP.

- The verdict is the verify stage's. Close runs its own layers and applies the verdict, so a halting verdict is a red gate like any other.
- Close pushes the item's branch and opens its PR. That is your only push right. Never merge the PR: the controller merges it by rule when its checks are green.
- On any red gate, whichever layer halts, write ossify's `OSSIFY CORRECTION CONTINUATION v1` packet (`work-item/references/correction-continuation.md`, section 2) to the correction packet file in your message, and end. The packet holds `handoff_path` and `work_item_id`; `expected_branch` the branch; `expected_head_sha` the base SHA (implementers never commit); `expected_tree_oid` from `git -C <worktree> write-tree` at the gate; and `failures`, every source-tagged finding.
- Any other halt is a stop under the stop rule: a merge conflict, an identity check, a verdict that is not this tree's (`verdict identity … does not match`, `verdict is for …`) or not in the verdict shape, a remote leg that fails. Never pick a recovery option.

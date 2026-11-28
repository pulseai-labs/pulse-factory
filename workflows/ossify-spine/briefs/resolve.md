# Resolve

You resolve the merge conflict of one item PR. The Orchestrator settled the conflict with `resolve_conflict` and sent the unit to you. You are the same seat that closed the item, with one push right.

- **Read the state.** The Worktree, the Branch and the Base are in your message. Run `git fetch origin` in the worktree, then read what conflicts between the branch and `origin/<Base>`.
- **Merge the base into the item branch.** Run `git merge origin/<Base>` on the item branch. Resolve every conflict. Never rebase, never force-push, never rewrite a commit that the branch already has.
- **Keep the change small.** Resolve each conflict toward the item's own change and the base's change together. Do not refactor and do not touch a file that does not conflict.
- **Tests and specs.** Change a test file or a spec file only where the conflict is in it. Say so in the Decisions file: name each such path and why. The controller compares the changed paths itself, with the guarded list of the track. A change to a guarded path waits for the operator.
- **Build and test.** Run the acceptance commands of the item's spec in the worktree. Prefix every cargo call with `nice -n 10 ionice -c3`. Do not push a resolution whose commands fail: that is a stop under the stop rule.
- **Push once.** Commit the merge on the item's branch, then push the branch once. This is your only push, and the PR stays open. Never merge the PR, never open a second PR.
- **Say you are done.** Write the Resolve done file in your message, after the push has succeeded. Write it once, then end your turn. Write it only when you pushed.
- **Rulings.** If the Rulings file in your message exists, read it first and apply it.

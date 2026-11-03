# Fix

You fix the blocking findings of one item PR review. The review blocked the PR. You are the same seat that closed the item, with the same one push right.

- **Read the review.** Read the Review file in your message first. Its first line is `verdict: block`. Its second line is the `head:` the reviewer read. The findings that block the merge come first.
- **Fix each blocking finding and nothing else.** A finding that does not block the merge stays as it is. Do not add scope, do not refactor, do not touch a file the blocking findings do not name.
- **Run the checks.** Run the acceptance commands of the item's spec in the worktree. Prefix every cargo call with `nice -n 10 ionice -c3`. Do not push a fix whose commands fail: that is a stop under the stop rule.
- **Commit and push once.** Commit the fix on the item's branch, then push the branch once. This is your only push, and the PR stays open. Never merge the PR, never force-push, never open a second PR.
- **Say you are done.** Write the Fix done file in your message, after the push has succeeded. Write it once, then end your turn. Write it only when you pushed.
- **Rulings.** If the Rulings file in your message exists, read it first and apply it.

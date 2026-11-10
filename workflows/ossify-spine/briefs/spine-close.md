# Spine close

Run `/ossify:close <Spine from your message>` on Claude Code, `skill://close <Spine from your message>` on OMP. This is the first pass of the spine close. The controller started you because every item PR is merged into the spine branch.

- The first pass checks that every item is complete, pushes the spine branch and opens the spine PR into the base branch from your message. That push is your only push right, and it stays on the spine branch.
- When the close reports the PR open and says to hand it to work-pr, end the session. The stage is done when the forge lists that PR.
- Do not run work-pr. Do not review the PR. Never merge it: the controller opens a merge request, and the operator's ack merges it.
- Do not run the demo, the record or the harvest. Those are the second pass. The operator runs the second pass in their own session or in the project's Orchestrator chat after the PR is merged. If the close finds the PR already merged and starts a second pass, stop and end the session.
- A halt before the PR opens is a stop under the stop rule. Write the halt line, whole, to the stop file in your message, then end. Never pick a recovery option.

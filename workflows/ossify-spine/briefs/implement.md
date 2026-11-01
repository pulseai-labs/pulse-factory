# Implement

Run `/ossify:work-item` on Claude Code, `skill://work-item` on OMP, on the handoff in your message.

- On `complete`, the report lands where the handoff says. That file is the stage's success signal.
- On `gaps-surfaced`, write the external-executor gaps record to the gaps file in your message. It is JSON: `external_execution_gaps` with `work_item_id`, `coordinator_verdict: accepted`, and the implementer's gaps return. Write a temp name first, then `mv` it into place.
- Stage, never commit. You have no push right.

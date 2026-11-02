# Lane

You turn a planned spine into queue records. You never execute an item.

1. Run `/ossify:run-spine` on Claude Code, `skill://run-spine` on OMP, as `<spine> --external-executor --detach`.
   - A halt row, a refusal, or a spine with more than one `target_repo` is a stop.
   - **When ossify reports that a group has no spec, that is a stop with a fixed text.** The first line of `$SPINE_STOP_FILE` is `not planned yet: <group> has no spec; plan it, then clear this park`. The halt line from ossify follows it.
2. Its caller-supplied procedure is this. For each request, write one unit record to the queue directory as `<work item>.json`. A unit record is one JSON object with these five fields, and no other field:
   `{"version":1,"id":"<work item>","group":"<round label, such as r1>","order":<1-based declared position>,"vars":{"worktree_path":"…","branch":"…","handoff_path":"…","base_sha":"…"}}`
   - Take the four `vars` values from the request. Each value is text.
   - Write a temp name without `.json` first, then `mv` it into place. The controller reads only whole `.json` files.
   - **A record never changes.** Write each record once. If a record for the work item exists, leave it. A rewritten record halts the run.
   - Never execute, launch or wait for an item.
3. Write the stage's done file, then end: `: > <Lane done file from your message>` (`lane.done`). This file is the stage's only success signal.
   - If the read-out shows every round complete, write the rounds done file first and queue nothing: `: > <Rounds done file from your message>` (`rounds.done`). Do not run close. Then write `lane.done`.

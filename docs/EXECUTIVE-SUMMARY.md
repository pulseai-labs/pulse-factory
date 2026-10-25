# Pulse Factory — executive summary

**What it is.** Pulse Factory runs a planned piece of software work to completion with no
supervising model session alive. It drives interactive coding-agent sessions (OMP first, then Claude
Code and Codex) through a workflow. A workflow is data that names the stages, roles, models, briefs,
done signals and merge rules. The first workflow is the ossify spine.

**Why.** An operator's main cost when working with coding agents is supervision, not decisions. A
session kept alive to watch a run re-reads its whole context on every wake. In one measured run, the
cheap worker sessions cost $3.25, while the watching session took about half of all the strong-model
spend. Pulse Factory moves the waiting into code, which costs nothing. A model session starts only
where judgment is needed.

**Who it is for.** One operator on one Linux host, who plans work in their own agent session and
wants it done while they are away.

**How it works:**
- **Arm and leave.** The operator arms a planned spine through the `pulse-factory` CLI and closes
  their session.
- **Code drives the seats.** Cheap model sessions implement, close and merge each item on green CI.
- **Halts reach the operator.** A question, a halt or the final merge ack becomes a request. The
  operator answers it later from any agent session that takes the orchestrator claim.

**What the first release delivers.** At Release 0 close, the operator can:
1. arm a planned one-round ossify spine from their own session, then close that session;
2. later, answer one merge-ack request through the claim;
3. walk the spine's demo in that same claim session;
4. see the spine PR merged into `main`, with each session's cost shown.

No model session is alive in between, except the cheap seats doing the work. The CLI is the only
surface in Release 0.

**What comes next:**
- a read-only run view;
- strong-model seats for round planning and plan review;
- spines of more than one round;
- an idle spine driver for halts;
- the demo walked before the merge.

# ADR-0006 — Every node ends in a recorded status, and every halt becomes a request

- **Status:** Accepted, 2026-11-05, at the Release 0 close
- **Date:** 2026-10-25
- **Amended:** 2026-11-17, at the close of spine `r2.s1`: a seat that ends with no done signal may be
  nudged first, and a stage's commands may explain its park. Nothing above changes.

## Context
- Every halt must reach someone who can answer it, with no live supervisor.
- Monitoring stays outside agent conversations, and missing telemetry is shown, not hidden.
- A broken view must be told apart from a dead session.

## Decision
- Every node has one status: running, done, failed, stopped, waiting on you, or parked. Every end
  status carries a one-line reason.
- Halts in Release 0 are:
  - a stop file;
  - an implementer's pre-flight `gaps-surfaced` return. The request carries each gap. The claim's
    answer is written into the handoff's `## Clarifications`, and the item is dispatched again, which
    counts toward ossify's cap of 3;
  - a red close gate;
  - a stall (no new turn, file, commit or PR within the stall limit; one relaunch, then park);
  - a merge conflict;
  - a failed or refused launch;
  - a seat lost across a controller restart.
- Each halt opens a request, and `pulse-factory requests` lists it.
- These must never end silently:
  - a seat that ends with no done signal;
  - a refused launch;
  - a failed merge;
  - a store write failure. On a store write failure, the controller stops admitting work. It writes the
    failure to a health file outside the store and to its journal. `status` then reports the
    controller as degraded, with the reason, and no longer reads the store.
- The controller writes a log. The log is for diagnosis. `status` and `requests` are the visibility
  channel.

Rejected alternatives:
- **Logs as the visibility channel.** Nobody reads them during a run.
- **A supervising session.** That is the anti-goal this product exists to remove.

### Unverified claims
- ossify 1.26.0's work-item re-reads `## Clarifications` on every dispatch, whoever wrote them
  (`work-item/SKILL.md:90-95`). Nothing has yet tested a claim-written answer under
  `--external-executor --detach`. The skeleton spine verifies it. ossify issue #914 makes it a
  contract.

## Consequences
- Release 0 sends no notification, so the operator runs `requests` to see what is waiting. The feature
  map has "request notifications" for later.

## Amendment, 2026-11-16 (Proposed): each park reason and request kind is above or below the line

**Status: Proposed; ruled by the operator on 2026-11-16.** It takes effect when the code that builds
it lands, and that work's close marks it Accepted. It goes with ADR-0005's amendment of 2026-11-16.

- **The class is a function of the cause and of the track's merge.** A request is `below` or `above`,
  and the engine computes the class when a reader asks. No request stores it. The merge is the
  track's rule and its base, read from the run's pinned copy of the workflow, never from the package
  on disk. `below`: a stall, a seat lost across a restart, a host that does not answer, and a red
  check or a merge conflict on a track that needs no ack and does not merge into the default branch
  of the forge. A failed launch is below the line only once an adapter marks its cause as
  transient, and none does today. Every other cause is `above`, and so is a request whose class
  cannot be computed: the class fails closed. The cause `resolution_changed_guarded`, which is `above`,
  parks a unit whose conflict resolution changed a path that the track guards (ADR-0005).
- **No request kind carries a class.** The class is per request, from its cause. Every request kind
  a plugin adds is `above` (ADR-0017).
- **A `below` request may be settled by the clear grant** or by the claim. An `above` request is
  settled by the claim only.
- **Every halt still becomes a request**, and every end status still carries its reason. A settled
  request records who settled it: the claim's holder or the clear grant's session.

## Amendment, 2026-11-17 (Accepted): a seat that ends with no done signal may be nudged first, and its park may be explained

**Status: Accepted, 2026-11-17, at the close of spine r2.s1.** The operator ruled the calls of spine
r2.s1 on 2026-11-17. The spine built it (PR #72, merged at `abef5de`), and the d29 sandbox walk at its
close passed it.

- **The "never end silently" line for a seat that ends with no done signal stands.** A stage may now
  do two recorded things before the park. Neither lets the seat end silently, and the park still opens
  a request.
- **A stage may nudge the seat up to its cap first.** A stage that declares `no_signal` with
  `nudges` above 0 gets the controller's message in the same session when the turn ends with no done
  signal. The cap is 3 at most and is counted per session. Each nudge writes one ledger line (run,
  unit, stage, role, model, session, `nudge <n> of <m>`), and `arm` names the cap before the run
  starts. Under `halt` no nudge is delivered. A delivery that fails goes on to the park at once.
- **A stage's `run` commands may turn the park into a stop request.** The controller runs them before
  it parks, each under the bound of any command. A stop file that one of them writes makes the park a
  `stop` request with that text and the stop file's answer hook, even when that command or a later
  one then fails or times out. A command that fails or times out stops the list and is a note. The
  controller then reads the stage's signals again in their usual order: the stop file first, then a
  `park_on` file (a park with that file's kind and reason), then the `done` conditions (the stage is
  done when they now hold). Only when none of these holds is the park the one it was before: "seat
  ended with no done signal", kind `park`.
- **Every halt still becomes a request**, and every end status still carries its reason. A stage with
  no `no_signal` parks as before. The engine knows no nudge and no command: the controller's driver
  carries both out (ADR-0001), and the workflow file declares them (ADR-0004).

## Amendment, 2026-11-21 (Accepted): a request carries a cause and a decision block

**Status: Accepted, 2026-11-23, at the close of spine `r2.s11`; ruled by the operator on 2026-11-21** (spine r2.s11, calls Q1 to Q17).

- **Every park names a cause** from a closed list, set in code at the site that makes the halt. No code
  reads the reason text to find it. A park stored before this amendment has no cause and reads as no
  block.
- **A request holds a decision block** in its body: the cause, a summary the controller writes, the
  options, the recommendation with its source, the IDs of the project, run, unit, stage and PR, the
  attempt, who asked, and whether a text would reach a seat. The block is data for readers. The engine
  derives it when it decides the park command, so a replay of the command ID derives nothing.
- **No verb changes with the block.** `answer`, `clear`, `ack` and the clear grant act as before. An
  option names an act that an existing verb already does. Who may settle a request does not change.
- **The line class of the 2026-11-16 amendment attaches to the cause.** A cause is `below` or `above`
  the line. Four of the five reasons that amendment lists as below the line carry a `controller`
  recommendation: a stall, a seat lost, a host that does not answer, and a red check. A failed launch
  with a transient cause is below the line and carries none. A recommendation never changes a class.
- **Every halt still becomes a request**, and every end status still carries its reason. A seat's text
  stays in the request's body as data (ADR-0005): the summary never holds it, and a seat's stop options
  are redacted and bounded before they are stored.

## Amendment, 2026-11-21 (Accepted): a notice is derived from a request, acknowledged per target and sent again

**Status: Accepted, 2026-11-23, at the close of spine `r2.s11`; ruled by the operator on 2026-11-21** (spine r2.s11, calls Q1 to Q17, work item
`r2.s11.w3`).

- **Every request event becomes a notice.** The controller derives one notice from each request that
  opens, each revision that a tick sees while the request is open, and each close. A request that closed
  before the controller saw it open gets none. The notice is data about the request: no verb changes with
  it, and no notice answers, clears or acknowledges a request.
- **A notice is delivered on two channels**, the paired devices and the project's Orchestrator chat. It
  starts no turn, no run and no launch, so `halt` does not stop one. A controller whose commands are
  stopped writes none, and `status` says so.
- **A notice is acknowledged per target and sent again until then.** A target acknowledges a position,
  and the position covers every notice of its channel at or below it. A notice that its target has not
  acknowledged is sent again on each reconnect and on a timer that doubles up to 15 minutes, until the
  target acknowledges it or the request closes. A closed request ends the re-send of its earlier notices.
  A `closed` notice is sent once and owes no acknowledgement.
- **The text is the block's summary**, redacted once and cut to the channel's word cap. It never holds a
  seat's words (ADR-0005), the reason, the stop text or the gaps.
- **No give-up while the request is open** is a known limit: a client that never acknowledges gets each
  waiting notice again at most every 15 minutes.

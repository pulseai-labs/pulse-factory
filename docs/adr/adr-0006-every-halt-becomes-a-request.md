# ADR-0006 — Every node ends in a recorded status, and every halt becomes a request

- **Status:** Accepted, 2026-11-05, at the Release 0 close
- **Date:** 2026-10-25

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

- **Each park reason carries a class**, `below` or `above`, set in code. `below`: a stall, a failed
  launch with a transient cause, a seat lost across a restart, a host that does not answer, a red
  check that a re-run may fix. Every other reason is `above`.
- **Each request kind carries a class.** A core kind is `below` only when every answer it allows is an
  act of the clear grant's closed list (ADR-0005, amended). Every request kind a plugin adds is
  `above` (ADR-0017).
- **A `below` request may be settled by the clear grant** or by the claim. An `above` request is
  settled by the claim only.
- **Every halt still becomes a request**, and every end status still carries its reason. A settled
  request records who settled it: the claim's holder or the clear grant's session.

# ADR-0007 — Pinned runs, forward-only migrations, and no restart survival

- **Status:** Accepted, 2026-11-05, at the Release 0 close
- **Date:** 2026-10-25

## Context
- Runs stay pinned to their contracts, and a changed contract needs a new run.
- Start-up is reconciliation: it does not replay, and it does not start every recorded executor by
  default.
- A binary rolls back only while schema and protocol stay compatible.
- The operator decided on 2026-10-25 that Release 0 gives no restart guarantee.

## Decision
- At `arm`, each run pins its workflow file's version, and a content-hash manifest of the workflow file
  and every file it references: role briefs and rule sets. A seat is launched only from the pinned
  content. A change to any pinned file needs a new run.
- Store migrations are forward-only (ADR-0003).
- A binary rolls back only to one with the same store schema version and the same CLI-to-controller
  protocol version. Otherwise the fix goes forward.
- On start-up, the controller enters reconciliation:
  - every unit in `running` or waiting on a seat's next signal whose seat process is gone is parked,
    and a request is opened for it. Units already done, failed, stopped or parked are left as they
    are;
  - nothing relaunches by default;
  - the claim decides each relaunch.
- To back out a bad release, reinstall the earlier binary while no run is active.

Rejected alternatives:
- **Restart survival now** (one owner process per seat). It is on the feature map: "seats survive
  controller restart".
- **Event-sourced replay.** The store design rules it out.

## Consequences
- A controller crash costs each in-flight seat its work since that seat last committed or staged.
- The operator decides each relaunch through the claim.
- **Known limit, a remote effect with no ledger line (amended 2026-11-05, at the Release 0 close;
  `r0.s3` audit finding 8).** The controller's base push can land on `origin` while its ledger write
  fails. The unit parks, and the park reason says the push was done but its ledger line was not
  written. On the retry, `origin` already has the branch, so the controller skips the push and no
  ledger line for it is ever written. The park reason is the only record. Revisit it with restart
  survival and ledger durability.

## Amendment, 2026-11-16 (Proposed): restart survival in two tiers

**Status: Proposed.** It is not in force. "No restart survival" holds until the operator accepts this
amendment and the session hosts of ADR-0001 (amendment of 2026-11-16) exist.

**Decision.**
- **Tier 1: a controller restart stops no session.** On start, before it admits new work, the
  controller checks the host of every session whose last state is live:
  - **survived:** the host's `hello` names the session, the incarnation and the launch-record hash
    that the store holds. The controller rebinds the adapter and reads the output from its last
    acknowledged cursor. No request is opened;
  - **lost:** no host job exists, or its exit file is there. The controller records the exit, and the
    session is lost (tier 2);
  - **unreachable:** the job exists but gives no valid `hello` in time. The controller parks the
    session with the reason "host unreachable" and never launches a second copy.
- **Tier 2: a lost session comes back from its launch record, never from defaults** (ADR-0003):
  - **a chat** resumes at the operator's next open, on the record's runtime and runtime session ID. A
    failed resume shows "context lost", and the operator chooses (ADR-0014);
  - **a seat** parks with a request, as before. A relaunch from its record needs the claim or the
    clear grant (ADR-0005, amendment of 2026-11-16), and counts against the run's armed launch
    budget. The request says whether the relaunch resumes the runtime session or starts fresh with the
    unit's handoff;
  - **a terminal** starts a new shell at the next open, in the record's working directory with the
    record's environment. Its scrollback is lost, and the tab says so.
- **Settings at a resume.** A resume uses the record's values, except a key whose registry entry
  says `applies: current` (ADR-0016, amendment of 2026-11-16).
- **Upgrades.** A host upgrade does not touch running hosts: each keeps its binary until its session
  ends, and old host binaries stay installed while a live record needs them. A runtime upgrade applies
  to new sessions. A resume whose recorded runtime copy is gone asks before it uses the new one.
- Pinned runs, forward-only migrations and the rollback rule above do not change.

**What it replaces.** The rejected alternative "restart survival now" is now chosen. "Nothing
relaunches by default" stays for seats. "The claim decides each relaunch" becomes "the claim or the
clear grant".

**Consequences.** A controller crash no longer costs a seat its work. A host crash or a reboot still
does, and the relaunch is a recorded act. The known limit of the base push with no ledger line stays;
its revisit comes with ledger durability.

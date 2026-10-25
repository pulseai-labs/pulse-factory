# ADR-0005 — Seat trust and destructive operations

- **Status:** Proposed
- **Date:** 2026-10-25

## Context
- Seats are cheap model sessions that run unattended.
- A halt scan of 2026-09-25 to 10-25 found two things:
  - permission prompts halted only Claude Code seats;
  - most halts needed only a role rule.
- There is no sandbox, and direct shell actions stay possible. The rails stop mistakes, not an
  attacker.
- The operator decided on 2026-10-25 that Release 0 uses local rails only. Login, cookies, CSRF and
  TLS come with the run view.

## Decision
**Untrusted input.** These are untrusted input:
- everything a seat writes: reports, stop files, ledger lines and PR bodies;
- PR review comments;
- CI logs.

Pulse Factory parses them as data in strict formats and never executes their text.

**Seats:**
- Every seat runs with permission prompts off.
- The `hook` command applies the role's rule set. It answers only allow or deny, never ask.
- The rule set parses commands. It does not match words inside quoted text, heredocs or `grep`
  patterns.
- Denied for every seat: merges, force-pushes, pushes to the default branch, branch deletion,
  `--no-verify`, and AI trailers.
- Allowed per role:
  - deleting the seat's own scratch and build targets;
  - pushing the seat's own branch or tag;
  - writing the seat's own report and run files.
- Seats have no ask tool and no self-wake tools.

**Irreversible operations:**
- launching a seat, which spends money;
- pushing and merging;
- deleting worktrees and scratch.

Only code merges, and only under the workflow's merge rule:
- item PRs merge into the spine branch on green CI, in the declared order;
- the spine PR merges into `main` only on the claim's ack.

**Authority:**
- Only a session that holds the claim can answer a request.
- A session that Pulse Factory launched can never claim. Pulse Factory launches every seat with a
  seat token in its environment. `claim` refuses when one is present, and so does the controller when
  the caller's process descends from a seat it launched.

**Credentials.** In Release 0, seats run as the operator's OS user with the operator's `gh` and git
credentials. There is no separate seat identity. So the rule set denies forge writes on every path
it can parse:
- `gh pr merge`;
- `gh api` with any method other than `GET`;
- `git push` to the default branch, or with force;
- `curl` or `wget` to the forge's API host.

A command it cannot parse, such as a script file the seat wrote, is the known limit below.

Rejected alternatives:
- **Route approval prompts to the operator.** Unanswered prompts were the main halt cause in the halt
  scan.
- **Full operator authentication in Release 0.** The operator chose local rails only, because Release
  0 has no network surface.

## Consequences
- A gap in a rule set lets a seat do harm with no prompt. The `seat-permission-rules` risk gate covers
  this.
- With no sandbox, a seat with shell access can still act outside its rules. The rules make that a
  mistake, not a routine.
- **Known limit:** with a shared token, least privilege holds only at the command level. A seat that
  writes a script calling the forge API can merge or delete. A separate seat identity, with a token
  that cannot merge, is on the feature map.

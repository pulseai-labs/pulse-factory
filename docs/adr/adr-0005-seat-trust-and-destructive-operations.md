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
- the spine PR merges into `main` only on the claim's ack. The ack binds to the current claim
  generation, the exact request ID and revision (ADR-0009), and the PR's head SHA. Code merges with
  that SHA pinned, so a moved head or a superseded claim cannot merge.

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
- **Known limit, a push target the rules cannot read (stated departure, `r0.s1.w3`):** the real push
  of the `ossify` package is `git push -u origin "$wi_branch"`, so its branch is a shell variable.
  `pf-rules` allows a push whose branch is one quoted variable only for a role with `push_own_branch`,
  and it audits the call with the rule `own-branch-unverified`. The rules cannot check the value. The
  adapter's post-push check of the remote's default branch guards it after the fact. `"+$b"` (force),
  `":$b"` (delete) and a variable without quotes are denied.
- **Amended 2026-11-01, `r0.s1.w5`: what the rules read of a delete and a push.** The real `ossify`
  blocks (`inv="$(mktemp)"`, `rm -f "$inv"`, a `trap` that names a variable, `git -C "$wt" push`) have
  to pass, because a deny in one parks the seat. These changes follow, and each has a known limit:
  - **A delete inside the worktree is a seat's normal work** for every role with a worktree. `rm`,
    `rmdir` and `unlink` of a path under `worktree` are allowed, as `git rm` is. The worktree itself,
    anything under its `.git`, and a glob that may take most of it (`*`, `.*`) stay denied. `find
    -delete`, `git clean` and `git worktree remove` still need a target in the scratch or the build
    target, and the role's listing. A bare `cargo clean` is denied for every role, because it wipes a
    shared target; `cargo clean -p` is not a delete.
  - **A variable that the whole call assigns only from `mktemp` or the empty string** is deletable by
    a role with `delete_own_scratch`, when the word is that variable alone. A `for`, a `read`, an
    `export`, `printf -v`, `${x:=…}`, `+=` or any other value ends it, wherever it sits in the call.
    *Known limit:* a script file, `source` and a function body that the call does not contain are
    not read, and `declare -n` makes every variable unknown.
  - **A relative path resolves against the working directory and every literal `cd` earlier in the
    call.** It must hold in all of them, because a subshell is not told apart. After a `cd` to a
    variable, a relative path is denied.
  - **A `trap` body is parsed again as a command**, like `eval`.
  - **A push has a directory and a branch.** The directory is a literal `git -C` or `cd`, else the
    working directory, and it must lie inside `worktree` or `push_dir`. The branch is a literal
    refspec, or for a bare push the branch of a literal `git switch` or `git checkout` earlier in the
    call, else the current branch. A literal half is judged as itself even when the other half is a
    variable. A directory or branch that is one quoted variable, or that follows a `cd`, `switch` or
    `checkout` to one, is `own-branch-unverified` for a role with `push_own_branch` and denied for
    any other. *Known limit:* the rules cannot check the value. The adapter measures the push in git
    and reads the default branch again after it (`docs/contracts/agent-adapter.md`).
    `git -C $dir push` with an unquoted variable is denied (`push-unreadable`).

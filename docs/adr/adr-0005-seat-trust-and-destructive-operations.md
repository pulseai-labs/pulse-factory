# ADR-0005 — Seat trust and destructive operations

- **Status:** Accepted, 2026-11-05, at the Release 0 close
- **Date:** 2026-10-25
- **Amended:** 2026-11-09, at `r1.s4.w1`: a paired device may hold the claim, and the confirm code is
  its claim-holder confirm; chats are refused on the claim verbs and `arm`, with a marker variable for
  a detached child.

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

*Amended 2026-11-04, `r0.s3.w1`:* code pushes a base branch only where a stage names it with
`base_push`, only when `origin` lacks the branch, never with force and never to the forge's default
branch, and each push has a ledger line (`docs/contracts/workflow-file.md`, `base_push`).

Only code merges, and only under the workflow's merge rule:
- item PRs merge into the spine branch on green CI, in the declared order;
- the spine PR merges into `main` only on the claim's ack. The ack binds to the current claim
  generation, the exact request ID and revision (ADR-0009), and the PR's head SHA. Code merges with
  that SHA pinned, so a moved head or a superseded claim cannot merge.

*Amended 2026-11-05, at the Release 0 close:* the `git-effects` gate's "human confirm" is a
**claim-holder confirm** (operator, 2026-11-04). The control is met when the session that holds the
claim acks the `merge_ack` request, which names the PR, the base and the head SHA. The claim carries
the operator's authority. The operator may delegate it to an orchestrator session, as for the d5
walk on `r0.s17`. The ledger records the claim generation, not whether a person or a delegate typed
the ack.

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
- **Push the spine branch at `arm`, or at the lane's cut time** (added 2026-11-05, for `r0.s3.w1`'s
  base push). The branch does not exist at `arm`: ossify's lane cuts it after the run starts. The
  lane is a seat, so a push at cut time would give a seat a second own branch and widen its push
  rights.
- **A static `pf-rules` allow for the named spine ref** (added 2026-11-05). It would let the close
  seat's own push create the base. `pf-rules` judges one command line and cannot read remote state,
  so it cannot allow the push only when `origin` lacks the branch. The controller can, and a seat's
  push rights stay unchanged.

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
    *Known limit:* a script file and a function body that the call does not contain are not read,
    and `declare -n` makes every variable unknown. (`source` and `.` are denied since
    2026-11-03; see the amendment below.)
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
- **Amended 2026-11-03, `r0.s2.w5`: the rule set reads a closed grammar of shell forms (grill lock
  G11).** The guards used to allow whatever no guard denied, so each shell form that the parser read
  wrongly needed its own denial, and the next spelling opened the same gap. Now the parser accepts a
  closed list of forms (`pf_rules::grammar`, `docs/contracts/role-rules.md`), and every other form is
  denied as `unparsed-command` with the form named. The guards run first, so a line that a guard
  denies keeps that guard's rule. These known limits close:
  - `${x:=y}`, `${x/a/b}`, `${!x}`, `$'…'`, `$"…"`, `$[…]`, `((…))`, a C-style `for`, `select`,
    `exec`, `source`, `.`, and a heredoc that feeds an interpreter are denied, and so is `$((…))`
    that holds anything but names, numbers, spaces and `+ - * / % < > = ! & | ( )`.
  - The word of `${x:-word}` and the body of an unquoted heredoc are read for commands and
    expansions, as a double-quoted word is. A `$(…)` in either was not read before.
  - A long option that git reads by a unique prefix (`--forc`, `--no-verif`) is read as the option.
    A prefix that two guarded options share is denied (canonical issue #8).
  *Still a known limit:* the rails stop mistakes, not an attacker. A script file that the seat
  wrote and then runs, and a program in a file or a `-c` string of an interpreter, are not read.
- **Amended 2026-11-05, at the Release 0 close: known limits that the `r0.s2` and `r0.s3` close
  audits named.** None changes a decision above.
  - **The grammar is a parse-list, not a list of safe forms.** Every form that the parser reads
    reaches the guards, and the guards deny what they name. Only an unreadable form is denied for
    being unreadable. So "a role runs only the shell forms the rules read" means the rules read every
    line they allow. It does not mean that every readable form is safe (`r0.s2` audit finding 1).
  - **The claim session's second pass runs outside `pf-rules`.** After the ack, `/ossify:close
    <spine>` runs in the claim session, which Pulse Factory did not launch. No role rule and no
    `hook` audit covers it. The claim holder owns what it runs (`r0.s2` audit finding 3).
  - **Ambiguous ancestry fails closed.** The caller check refuses when a read of the process chain
    fails or the chain does not end (`docs/contracts/requests.md`). A child that a seat detaches from
    its tree, with the seat token unset, passes the check: that is the stated known limit there.
    Revisit it with the separate seat identity, or when a network surface appears (`r0.s2` audit
    finding 6).
  - **A reviewer model switch needs a full first review.** Review rounds 2 and later read only the
    fix diff and each earlier finding's closure. When the reviewer model changes, as at the quota
    switch, the next review of an open PR is a full round 1 (`r0.s2` audit finding 9).
  - **The controller's base push trusts the local ref's content.** It checks the destination and
    the ref name, not who moved the ref. A seat that repoints the local spine branch (`git
    update-ref`, `git branch -f`) before the push makes the controller push that tip as the base.
    The item PR's review and the spine PR's ack still read the result (`r0.s3` audit finding 1).
  - **Git config in the repo directory is trusted by the base push.** `url.<base>.insteadOf` and
    `pushInsteadOf` rewrite the URL at transport time, and `core.hooksPath` and a `pre-push` hook
    run under the controller (`r0.s3` audit finding 2, PR #20's C9).
    (Closed by the amendment of 2026-11-06, below.)
- **Amended 2026-11-06, `r1.s1` (seat-trust hardening): five known limits close.** None changes a
  decision above.
  - **Closed:**
    - The base push trusts no seat-written git config (C9). The controller reads only names and
      objects from the seat's repository. `ls-remote` and the push run in its own bare repository,
      which borrows the seat's objects, with hooks and the file-system monitor off. Each
      seat-written setting that it ignores has a `note:` ledger line that names the key, not the
      value.
    - The ref is matched by its full name. The `ls-remote` output must hold `refs/heads/<b>`
      exactly, and the push names the remote ref by its full name (a glob from a repository that
      holds one branch). So `refs/tags/refs/heads/<b>` neither hides a missing branch nor takes the
      push.
    - A URL rewrite cannot move the push. The URL that the controller checks is the URL that its own
      repository uses. A seat-written `insteadOf` or `pushInsteadOf` present at the check is refused
      by name, and one written later changes nothing that the push uses.
    - Every user-info form of a `scheme://` URL is redacted in park reasons, requests, the ledger,
      the audit log and `arm`'s refusal.
    - A seat's write tools reach only its report directory and its own unit's directory of the run
      (`out/<unit>`, and `out/<seed>` for a repeat of a repeating seed). So a seat of one unit
      cannot write another unit's `review.md`.
    - `pf-rules` denies a seat that sets a key that redirects git, in any scope and through
      `git config`, `git -c` or `git --config-env` (`git-config-trust`).
  - **Evidence:** d9, `scripts/demo/seat-tamper.sh`, runs a paper seat that tries each of these.
  - **Still known limits:**
    - The controller trusts the content of the local ref. A seat that repoints the spine branch
      before the push makes the controller push that tip. The 2026-11-05 bullet stands, and d9 does
      not claim it.
    - A shell write (`>`, `>>`, `cp`, `tee`, `sed -i`, `ln -s`) is not read by path. A seat can
      still write another unit's run files, `.git/config` or a hooks directory that way. The brief
      and the review are the guard there. "Shell writes checked by path" is on the feature map.
    - The operator's global and system git config stay trusted, because the push credential lives
      there. `git-config-trust` denies a seat's write of the guarded keys there too, and a shell
      write to that file is the limit above.

## Amendment, 2026-11-07 (`r1.s2.w2`): a paired device is a principal that acts as the operator

ADR-0012 adds a second kind of caller beside the operator's OS user and the seats.

- **A device is a principal.** A device that paired with a one-use code holds a token, and the
  controller's API takes the token as the operator's authority for the verbs on the allowlist. The
  claim carries the same authority for `answer`, `clear` and `ack`, and a device has none of those:
  no ack, answer, claim, clear, arm or halt is on the allowlist. A device has no scope of its own
  in this spine (ADR-0009, amended). The human confirm of a merge-ack on a device comes with
  `r1.s4`.
- **Seats cannot pair or revoke.** `device pair` and `device revoke` refuse when `PF_SEAT_TOKEN` is
  in the CLI's environment. The controller checks the caller again, as it does for `claim`: a caller
  that is, or descends from, a seat is refused, and a failed `/proc` read refuses. The API offers
  neither verb, so a device cannot let another device in or cut one off.
- **No secret in a log.** No token, pairing code or private key is written to a log, a ledger line,
  a request, a park reason or an error. The token shows in the one answer of `POST /v1/pair`, and
  the code shows in the output of `device pair`.
- **A revoke takes effect at once.** The API reads the store on each call, so the revoked device's
  next call is refused.
- **Still a known limit:** a stolen device token acts as the operator on the allowlist until the
  operator revokes the device. The idle expiry (30 days), the rate limit and the per-request audit
  line come with `r1.s2.w5`.

## Amendment, 2026-11-09 (`r1.s4.w1`): a device holds the claim through a confirm code, and a chat is refused like a seat

- **The claim's holder may be a paired device.** Its label is `device D-n (<name>)`. No token exists
  for a device's claim: the store keeps the SHA-256 of 32 random bytes that nobody holds, so no CLI
  token can match it. A CLI holder label that starts with `device ` is refused, so a label cannot
  pose as a device.
- **The claim-holder confirm on a device is the confirm code.** A code is one-use, lasts 120 seconds,
  and binds the device, one request, its revision, one verb with its parameters, the claim generation
  current at issue and, for an ack, the PR's head SHA (`requests.md`, "Confirm codes"). A device
  acts only with a code, for `answer`, `clear` and `ack` alike.
- **The claim moves at the device's final click, not when it asks for a code.** `confirm_code` moves
  no claim. The spend fences the holder its code saw and acts, in one admission. A claim that moved
  since the issue refuses the spend (`claim_moved`). The first device to act wins. A fenced CLI
  holder is refused with `the claim moved to device D-n`.
- **Chats are refused like seats.** `claim`, `answer`, `clear`, `ack`, `arm`, `device pair` and
  `device revoke` refuse a caller that is, or descends from, a live chat process. `halt` stays open
  to every caller.
- **The `PULSE_FACTORY_CHAT` marker.** The controller sets `PULSE_FACTORY_CHAT=<session ID>` last in
  every chat's environment, so a child that a chat detaches (a double fork, `setsid -f`, `nohup`) is
  refused too: the caller check reads the caller's environment for it.
- **Known limit, and its owner.** The marker is a speed bump of the same class as the seat's
  `PF_SEAT_TOKEN` and the chat's deny list, and no boundary. `env -u PULSE_FACTORY_CHAT` with a double
  fork, `env -i`, `systemd-run --user`, a rewrite of the process's own `/proc/<pid>/environ` and the
  operator's `gh` login (a chat keeps `HOME`) pass it. The Release 2 authority-rules spine owns the
  residue.
- **Still a known limit:** a code proves the device, the freshness and the head, not that a person
  pressed the button.

## Amendment, 2026-11-10 (`r1.s4.w2`): a device arms with a confirm code, and the second pass runs in a chat

- **A device arms a run proposed from a chat, and an arm takes no claim.** `arm` answers no request,
  and the operator's CLI `arm` needs no claim, so the device's arm takes no claim either: the confirm
  code for an arm binds the proposal and the hash of its plan, never a claim generation, and the arm
  moves no claim. `claim_moved` never applies to an arm. The claim still governs answers.
- **A chat still cannot arm.** A chat process, and a process below one, is refused on `arm` and on
  the claim verbs. It runs `pulse-factory propose`, which starts nothing: the controller builds the
  plan, checks it with the dry run and posts a start card into the calling chat. Only a paired
  device's confirm arms it. The caller of `propose` is found by its process ancestry alone. The
  `PULSE_FACTORY_CHAT` marker only ever refuses. All of this is a speed bump, not a boundary, and the
  Release 2 authority-rules spine owns the residue.
- **The second pass of the spine close runs in the operator's own session or in the project's
  Orchestrator chat.** The close needs no claim, so the waiting text no longer names a "claim
  session". The Orchestrator is a chat Pulse Factory launched, not a seat: no `pf-rules` role applies
  to it. Its deny list does, with the known limit of ADR-0014.

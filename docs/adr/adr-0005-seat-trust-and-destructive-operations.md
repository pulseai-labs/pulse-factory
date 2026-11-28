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
  no ack, answer, claim, clear, arm or halt is on the allowlist
  (superseded by the amendment of 2026-11-21). A device has no scope of its own in this spine
  (ADR-0009, amended). The human confirm of a merge-ack on a device comes with
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

## Amendment, 2026-11-16 (Proposed): the clear grant, the caller's session read from its host, and bypass plus personas

**Status: Proposed; ruled by the operator on 2026-11-16.** It takes effect when the code that builds
it lands, and that work's close marks it Accepted. Until then the rules above hold. The caller check below
needs the session hosts of ADR-0001 (amendment of 2026-11-16).

**Why.** Most halts in a run are mechanical: a stall, a failed launch, a seat lost across a restart, a
flaky check, a merge conflict on a branch that is not the default. Today only the claim can settle
them, so each one waits for the operator. A launched session must still never answer for the operator.

**Decision: two grants.**
- **The claim** is unchanged. It carries the operator's decisions. Only the operator's CLI or a paired
  device holds it. A session that Pulse Factory launched can never claim.
- **The clear grant** settles halts below the line, and nothing else. The project's Orchestrator agent
  holds it, on one server. A new session of the same agent (after a handoff) acts under the same grant.

**The clear grant's acts.** A closed list in code. No setting, plugin or workflow adds to it.
1. Clear a park whose reason has the class `below` (ADR-0006, amendment of 2026-11-16).
2. Resume a parked unit from its launch record (ADR-0003, amended), inside the run's armed caps. On a
   park with the cause `host_unreachable`, the act
   attaches to the recorded host again before any relaunch.
3. Re-run the failed checks of a PR, at most `authority.rerun_checks_per_head` times for one head SHA.
4. Dispatch a seat again, inside the dispatch cap and the run's armed launch count. It never raises
   either.
5. Send a seat to resolve a merge conflict on an item branch or a spine branch, never on a default
   branch. If the resolution changes a guarded path, the unit waits for the operator:
   the controller compares the paths the resolution changed with the track's `guarded` globs,
   and parks the unit with the cause `resolution_changed_guarded`, which is `above`, when one matches
   or a read fails. Otherwise the unit goes to the track's `then` stage, so a reviewer sees the
   resolution before the merge.
6. Close a request that a newer revision of the same request replaced. `close_stale` has no object,
   because a revision replaces its request in the same row, so no stale request exists.

Each act is an admitted command with a command ID and one ledger line that names the grant, its
generation, the acting session, the request or unit, and the reason.
An act with an effect outside the store commits first and runs its effect after: the effect's result is its own command,
`settle-result:<R-n>:<revision>`. Until the result is stored, no launch or merge of the unit is
admitted, and a start-up reconcile closes a result that was never stored as `unknown` and parks the
unit again, so an effect never runs twice.

**The verb.** The grant acts through one verb of its own, `settle`, which
names a request at the revision the caller read, never a bare unit, and gives one act and a reason.
The list is closed per cause: each cause admits only its own acts, and `--to` never names a later stage
than the one the unit parked in. It is not a claim verb: `answer`, `clear`, `ack`, `arm` and the rest still refuse every
chat and seat. `settle` refuses every caller except a session that the controller identifies, from
its host, as the grant's holder for the project of the request. Giving and ending the
grant, `grant clear` and `grant revoke`, pass the caller check that `arm` passes and take no claim.
They run on the CLI and, with a confirm code, on a device (ADR-0012, amendment of 2026-11-16).

**Above the line, for the claim only:** the merge ack into a default branch; an answer that changes a
spec, an ADR, a rule set or a permission, including an implementer's clarification; a raise of a cap
or a launch budget; installing or removing a plugin; every settings write; a demo verdict; every
request kind a plugin adds.

**Above the line, for the operator, with no claim:** `arm`, and giving or ending the clear grant. Each
passes the caller check that refuses a seat and a chat, and on a device each needs a confirm code.

**Granting.** The grant is off until the operator grants it for a project, on the CLI or with a device
confirm code. The operator revokes it at any time. A revoke ends its generation, and the next grant takes
the next one, so an act under an old generation is refused. A revoke commits even when the
controller's commands are stopped, and its ledger line follows at the next start. A removed project's
grant is not live. A device's confirm code comes with the device verbs. `halt` does not revoke the grant, and a halted run refuses every `settle`. The setting `authority.clear_acts` (ADR-0016, amended) can narrow the list for a
project or an agent. It can never widen it.

**The caller's session, read from its host.** An environment marker may only refuse; it never grants.
- On Linux the controller reads the caller's PID from the socket's peer credentials (a pidfd) and
  walks the caller's ancestry to a live chat session's process. Under the `systemd` launcher it also
  needs the caller's cgroup, read from `/proc`, to be the cgroup of that session's host unit: the
  ancestry and the host's check must both name the same chat session. It checks again at the end
  that no process of the chain exited and no PID was used again. A double fork stays in the cgroup.
  So under `systemd` the grant verbs refuse a caller in a chat host unit's cgroup even when it has
  no chat ancestor and an empty environment. Under `detached` that refusal does not exist: the
  ancestry is the whole check, and the gap is a stated residue. The cgroup is not proof against a
  hostile session: any process of the user can write
  itself into another cgroup of the user's own systemd tree, so it can pose as another session, or as
  the controller to a host.
- On macOS there are no cgroups. The refusals stay the marker and the process ancestry, from the
  host's PID, until a test run verifies a stronger mechanism. Until then `settle` is refused on every OS but Linux:
  the grant may be issued there, but none of its acts is admitted on a marker.
- **Known limit, unchanged in kind.** A same-user process can leave its cgroup (`systemd-run --user`),
  move itself into another cgroup of the user's systemd tree, write the store file, or call the
  operator's socket as the operator. The clear grant adds no new path: a session that poses as the
  operator already reaches more than the grant. The real boundary is a separate OS user, a system unit
  with `User=`, or a `bwrap` sandbox that hides the operator's state directory (its socket, store and
  claim token) and the user's runtime directory (the systemd user bus) from the session. A user unit's
  own sandbox options are not enough: on a host whose AppArmor restricts unprivileged user namespaces,
  they silently do nothing. Inside such a sandbox a seat's hook needs a channel of its own to the
  controller. It is the direction, not this amendment.

**Bypass plus personas** (operator, 2026-11-08).
- Every launched session runs with its harness's permission prompts off. No harness prompt ever
  decides authority.
- A persona is an agent row (ADR-0015) with an authority profile: a role's rule set for a seat, a deny
  list for a chat, and any grant. Pulse Factory enforces the profile at its own points: the seat's tool
  hook, the chat's deny list at launch, and the controller's caller checks. A persona never carries the
  claim. Only built-in personas exist; own personas and own rule sets are later.
- **A seat's tool hook fails closed in every harness.** A hook that crashes, times out or answers in a
  shape the harness does not read must block the call. A harness whose hooks fail open by default is
  launched with its block-on-failure setting. A seat loads no setting source the project or the
  operator controls, because such a source can switch hooks off.

**What it changes.** "Only a session that holds the claim can answer a request" becomes "only a
session that holds the claim can answer a request above the line; the clear grant may settle one
below it" (ADR-0006, amendment of 2026-11-16). "A session that Pulse Factory launched can never claim",
the seat denials and the merge rule do not change.

**Consequences.** A mechanical halt no longer waits for the operator once the grant is given. Every
act of the grant is in the ledger and visible to the operator.

## Amendment, 2026-11-21 (Accepted): a confirm code binds an option, and a seat's options are data

**Status: Accepted, 2026-11-23, at the close of spine `r2.s11`; ruled by the operator on 2026-11-21 (`r2.s11`, Q1 to Q17, with Q16's audit).**
It goes with ADR-0004's and ADR-0012's amendments of the same date.

- **A confirm code binds an option.** A code for an option binds the request, its revision, the verb and
  the option's ID, in place of a text hash, a stage and a head of its own. The controller checks at issue
  that the option exists at that revision and that its effect fits the verb. At the spend the call's option
  must be the code's, and the request is read again: a revision between the card's render and the press is
  `subject_moved`. No new refusal word exists. `approve` is the `ack` verb under the same fresh code, so a
  merge from a device still needs the human confirm.
- **A seat's option is data.** A seat writes the options of a stop file, so its words reach an admitted
  answer when the operator presses one. The residue of the planning audit (row 3) is this: the text of the
  seat's option becomes the text of an answer that the operator's press admitted, and the operator's
  press is the control. The note a seat reads says so: `rule.sh` writes ` — option <id>, written by the
  seat` in the entry's heading, so a seat reads the seat's option, pressed under claim `g`, and not an
  operator's sentence. Seat text stays data: it is stored, shown with control characters replaced and
  never run or rendered as a template.
- **A text that no hook would deliver is refused.** An answer to a unit's request is stored only when a
  hook would deliver it to a seat. Before this amendment it was stored and reached no seat.
- **The sentence of the amendment of 2026-11-07, "no ack, answer, claim, clear, arm or halt is on the
  allowlist", no longer holds** (canonical #42 C9). `answer`, `clear`, `ack` and `arm` are on the device
  allowlist, each with a confirm code, since `r1.s4.w1` and `r1.s4.w2`. `claim` and `halt` are not.

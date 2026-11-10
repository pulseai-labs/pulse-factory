# Requests and the claim contract

A **request** is something the operator can see and answer. Every halt opens one (ADR-0006). A
**claim** makes one session the only one that may answer, and a new claim fences the old one out
(ADR-0005, ADR-0009). The verbs are in `cli.md`. This file holds the records, the IDs, the rules
and the token file.

## The request record

One row in the store's `requests` table (migration `0002_requests.sql`).

| Field | Type | Meaning |
|---|---|---|
| `id` | string | `R-<n>`. One sequence for the whole store, across runs. |
| `revision` | integer (from 1) | Rises when the request's facts change while it stays open. |
| `run_id` | string | The run the request belongs to. |
| `unit_id` | string or null | The unit it halts. A run-level request has none. |
| `stage` | string | The stage the unit parked in, `merge` for a unit parked at its merge with every stage done, or empty. |
| `kind` | string | One of the kinds below. |
| `reason` | string | One line. It is redacted and cut like a park reason. |
| `body` | JSON object | The facts of the request. `gaps` and `stop` hold `reason`, and the facts of the seat's file (see "The body of a `gaps` and a `stop` request"). The other park kinds hold `reason`, `stage` and `park_generation`. |
| `status` | string | `open`, `answered` or `withdrawn`. |
| `opened_at` | string | UTC time, to the millisecond. |
| `answered_at` | string or null | When the request left `open`. |
| `answer` | JSON object or null | `{"verb":"answer","text":…}`, `{"verb":"clear","to_stage":…}` or `{"verb":"withdraw","reason":…}`. |
| `answer_generation` | integer or null | The claim generation that answered. |

A unit has at most one open request. The store enforces it with a unique index. A request leaves
`open` once: it is answered, or withdrawn because the run ended.

### Kinds

The set is closed. The kind comes from the code path that makes the halt. The engine never reads
the reason text to find it.

| Kind | Opened by |
|---|---|
| `park` | Any park whose source is not one of the others. It includes the park of a stage-outcome cap: `<stage>: <to> cap <max> reached` (`workflow-file.md`, "`outcomes`"). |
| `gaps` | A stage's `park_on` file named `gaps.json`. |
| `stop` | A stage's stop file. |
| `red_gate` | A stage's `park_on` file named `correction.txt`. |
| `stall` | A seat that stalled again after its one relaunch. |
| `merge_failed` | A merge that did not land. The unit parks through the same command as any park. |
| `seat_lost` | Start-up's reconcile, for a running unit whose seat is gone (ADR-0007). |
| `merge_ack` | The merge of a track with the rule `claim_ack` (`workflow-file.md`, "`merge`"): its PR is green and clean, and the merge waits on the claim's ack. A unit park cannot open it. |

### The body of a `gaps` and a `stop` request

The controller reads the seat's file when it opens the request. The text is seat-written, so it is
data: redacted, bounded, stored as JSON strings, and never rendered or run (ADR-0005).

- `gaps`: the `park_on` file, read to at most 1 MiB, as JSON. The gap list is at
  `external_execution_gaps.implementer_return.gaps` (the shape `briefs/implement.md` asks for), or in
  a top-level `gaps` array. The body gains `"gaps": [{"section", "question", "severity"}, …]`, each
  string cut to 2,000 characters. A file that is missing, over 1 MiB, not JSON or not that shape
  leaves `{"reason":…}` and adds `"gaps_error": "<cause>"`.
- `stop`: the body gains `"stop_text"`, the stop file's text, at most 64 KiB. A stop file that cannot
  be read adds nothing.
- `show` prints both, one line for a gap and an indented block for the stop text, with every control
  character replaced.

### The body of a `merge_ack` request

The merge step opens one `merge_ack` request for the unit, in one commit with the unit's park at the
stage `merge`. A replay opens no second one. The body is data:

- `pr`: the PR as `<repo>#<n>`. The engine has no PR URL; the forge adapter reads the PR by this
  form. `requests` shows it in the reason.
- `repo`, `base` and `head_branch`: what the PR was when the request opened.
- `head_sha`: the SHA the ack will merge.

The reason starts with the unit's track name: `<track> merge waits on the claim's ack: <repo>#<n>
<head branch> -> <base> at <short SHA>`. `show` prints the body and lists `ack` as the only verb.
`answer` and `clear` refuse a `merge_ack` request and name `ack`.

`ack` (`cli.md`) is admitted when the request is open and at the revision given, under the current
claim. The controller reads the PR again first:

- a head SHA other than `head_sha` revises the request (the body gets the new SHA, the revision
  rises by one, the reason names the new short SHA) and refuses the ack, and the refusal names the
  new revision and SHA;
- checks that are pending or red, a conflict, or a repo, base or head branch that differs from the
  body refuse the ack with the cause, and the request stays open at its revision;
- otherwise the answer is `{"verb":"ack","head_sha":…}`, and the PR merges with that SHA pinned
  through the same merge path as any other (command IDs `merge:` and `merge-result:`). A merge
  that lands ends the unit `done`. A merge that does not land parks the unit with `merge_failed`,
  and `clear` sends it back to the merge, which opens a new `merge_ack`.

Each ack writes the ledger line `ack <R-n>@<revision> by claim <generation>: verb=ack unit=<unit>
pr=<repo>#<n> base=<base> branch=<head branch> sha=<head SHA>`. Each refusal after the request is
known writes `ack <R-n>@<revision> by claim <generation> refused: <cause>`. The token is in neither.

### The revision rule

`answer` names the request ID and the revision it read with `show`. The engine admits the answer
only when the request is `open` and the revision is its current one. Another revision is refused,
and the refusal names the current one. `ReviseRequest` raises the revision by one and replaces the
body.

## Command IDs

Every change is an admitted command. Its ID is in the admissions table, and a replay with the same
content returns the stored receipt. The same ID with other content is a conflict.

| Command | ID |
|---|---|
| Open a request | `request:<run>:<unit or ->:<kind>:<n>` |
| Revise a request | `revise:<id>:<revision>` |
| Answer a request | `answer:<id>:<revision>` |
| Ack a `merge_ack` request | `ack:<id>:<revision>` |
| Record a refused ack | `ack-refused:<id>:<revision>:<n>` (n counts the refusals for the request and revision) |
| Withdraw a request | `withdraw:<id>` |
| Record a run of an answer hook | `answer-hook:<id>:<revision>:<n>` (n counts the runs for the request and revision) |
| Claim | `claim:<generation>` |
| Issue a confirm code, or act with one from a device | the device's own `command_id` |
| Clear a unit | `clear:<unit>:<park generation>` |
| Park a unit | `park:<unit>:<park generation>` |
| Merge a unit | `merge:<unit>:<park generation>` |
| Record a merge result | `merge-result:<unit>:<park generation>` |

The **park generation** counts the times a claim resumed the unit (from 0). It is engine-owned
unit detail (see `unit-record.md`), and it makes a second park of the same unit a new command.

## The claim

- One claim is current: the highest generation with no `fenced_at`. A new claim fences the old one
  with a fresh generation (from 1, strictly rising).
- The controller makes the token: 32 bytes from `/dev/urandom`, shown as 64 hex digits. It fails
  closed. When the source cannot be opened, gives a short read or gives only zeros, there is no
  token and no claim.
- The store keeps the SHA-256 of the 32 bytes as 64 hex digits, the holder label and the times.
  It never keeps the token.
- A fenced token is refused with `claim <old> was fenced by claim <new>`. A token that matches no
  claim is refused without repeating it.
- Each claim writes the ledger line `claim <generation> by <holder>`. Each answer writes
  `answer <R-n>@<revision> by claim <generation>: <what was done>`. For `clear` the text is
  `clear <unit> --to <stage>`. For `answer` it is the first 200 characters of the text, folded to
  one line and redacted.
- **The claim is one for the whole store.** The `claims` table has no run or project column, and at
  most one row has no `fenced_at`. A claim taken for any run fences the holder of every other run.
- **A holder may be a paired device.** The holder label is then `device D-n (<name>)`, with the name
  cut to one line. No token exists for a device's claim: the store keeps the SHA-256 of 32 random
  bytes that nobody holds, so no CLI token can match it. A CLI holder label that starts with
  `device ` is refused (`claim refused: the holder label "device …" is kept for paired devices`),
  so a label cannot pose as a device.
- **The claim moves at the device's act.** `confirm_code` changes no claim. The admission that
  spends the code (`device_answer`, `device_clear` or `device_ack`) fences the holder the code saw
  and acts under the new generation, in one admission, under the controller's claim lock. Rule: the
  first device to act wins. The spend decides the claim from the generation the code bound at issue:
  - this device already holds the current claim: no claim effect, and the act runs under that
    generation (`from=held`);
  - the current generation is the one the code bound, or both are none: the current claim is fenced
    and the device holds the next generation (`from=<n>` or `from=none`);
  - the claim moved to someone else since the issue: the spend is refused `claim_moved`, names the
    new holder, and moves nothing.
- A device act writes one `controller.ledger` line, `device_<verb> D-n <R-n>@<revision>
  generation=<g> from=<n>|held|none admitted command=<id>` (an ack adds `pr=`, `base=`, `head=`). A
  code issue writes `confirm_code D-n request=<R-n>@<revision> verb=<verb> generation=<g>|none
  command=<id>`. The run ledger keeps the engine's `answer`/`ack … by claim <generation>` line. A
  device claim has no `claim <n> by <holder>` run-ledger line: its record is the `controller.ledger`
  line and the `claims` row.
- **The fenced holder's refusal.** A CLI token that a device fenced is refused with `claim <old> was
  fenced by claim <new>: the claim moved to device D-n (<name>) at <time>; claim again with
  pulse-factory claim`. After a revoke, a claim that the revoked device holds stays current but no
  one can use it. `requests` shows the holder with `revoked: true`. The operator's next `claim` fences
  it.

### The token file

`claim` returns the token once, to the client. The client writes it to `<state dir>/claim-token`
with mode 0600 and replaces any older file. It prints only the generation and the path. `answer`,
`clear` and `ack` read the file named by `--claim-token-file`, else the default. A missing or empty file
is refused, and the refusal names the path.

The token reaches no log, ledger line, `status`, request body, `requests`, `show` or error text.

### The caller check

On every `claim`, `answer`, `clear`, `ack`, `arm`, `device pair` and `device revoke`, and on the
session verbs, the controller:

1. reads the peer's PID from the socket (`SO_PEERCRED`);
2. refuses a peer whose `/proc/<pid>/environ` holds `PF_SEAT_TOKEN`;
3. refuses a peer whose `/proc/<pid>/environ` holds `PULSE_FACTORY_CHAT` (the chat marker, below);
4. walks the parent chain through `/proc/<pid>/stat` and refuses when any process on it is a seat
   process or a live chat process the controller launched and still tracks
   (`AgentPort::process_id`, see `agent-adapter.md`). The refusal reads `<verb> refused: the caller
   is, or descends from, chat process <pid>`.

Any read that fails refuses, and so does a chain that does not end. The client refuses too when
`PF_SEAT_TOKEN` is set. A refusal records nothing. `arm` runs only through the peer-aware path: with
no peer it is refused (`arm refused: the controller cannot tell which process called`). `halt` and
the reads stay open to every caller, because the kill switch must work from anywhere.

**The chat marker.** The ancestry walk stops at the first parent with a PID of 1 or less. A child
that a chat detaches by a double fork, `setsid -f` or `nohup … &` from a subshell is reparented to a
subreaper, so no chat PID is on its chain. It keeps the chat's environment. So the controller sets
`PULSE_FACTORY_CHAT=<session ID>` (for example `S-1`) in the environment of every chat it opens, a
resume included. It inserts the variable last: neither an agent row's `launch_env` nor the
`PF_AGENT_ENV_<AGENT>` setting can remove or replace it, and a setting pair of that name is dropped
with one warning line. The refusal reads `<verb> refused: PULSE_FACTORY_CHAT is in the caller's
environment, so it runs under chat session <S-n>`. The name is not a `PF_` name, because the Claude
Code adapter reserves that prefix. The value is no secret.

**Known limit (ADR-0005: the rails stop mistakes, not an attacker).** The check follows process
ancestry and the caller's initial environment. A child that a seat detaches from its tree
(`env -u PF_SEAT_TOKEN setsid -f pulse-factory claim …`) has no seat ancestor and no token, so the
check passes it. The role rules do not deny `pulse-factory claim`, `answer`, `clear` or `ack`.
The same holds for a chat. The marker is a speed bump against a chat that detaches a child by
accident or by a plain recipe, and it is no boundary. It does not stop `env -u PULSE_FACTORY_CHAT …`
with a double fork, `env -i`, `systemd-run --user`, an `at` or `cron` job that does not inherit the
environment, a process that rewrites its own `/proc/<pid>/environ`, or keystrokes sent into the
operator's terminal. A chat also keeps `HOME` and the operator's credentials, so it can merge with
the operator's `gh` login and skip the controller. These residues are known limits. The owner is the
Release 2 authority-rules spine. A plain `setsid` with no fork keeps its parent, so the walk still
refuses it.

## Confirm codes

A paired device acts on a request with a **confirm code** (ADR-0005, ADR-0012). The code is
generic over its subject: `r1.s4.w2` adds the `arm` variant and nothing else changes shape.

- **Subject.** A code binds one request, its revision, one verb and the verb's parameters, the
  claim generation current at issue (or none), the device, and for a `merge_ack` the PR's head SHA.
  The verbs are `answer`, `clear` and `ack`. An `answer` code binds the SHA-256 of the answer text
  (`text_sha256`: the device sends the hash at issue, and the text never enters the code table). A
  `clear` code binds the target stage `to`, or its absence.
- **The `arm` subject** (`r1.s4.w2`). A code can bind a start card instead of a request: the
  proposal (`sp-…`) and its `plan_sha256`. An arm code takes no claim: it binds no generation, the
  spend reads no claim, and the arm leaves the claim with its holder (W6). It is refused
  `subject_moved` at issue when the proposal is unknown, withdrawn, failed, being armed or already
  armed, or when the plan hash differs from the card's. An arm code spent on `answer`, `clear` or
  `ack`, and a request code spent on `arm`, are each `code_subject`. The spend builds the plan
  again from the proposal's package snapshot and compares `plan_sha256` before anything runs
  (`api.md`, `arm`): a plan that changed since the card is `subject_moved`, and the card is
  `failed`.
- **Code text.** 16 random bytes from the controller's random device, shown as `pfc1-` and 32
  lower-case hex digits. With no random bytes there is no code and no admission. The answer of
  `confirm_code` is the only place the text appears. The controller keeps only its SHA-256.
- **Issue.** `confirm_code` is an admission under `@controller` with the caller's command ID and no
  effect. It moves no claim. A replay of the command ID answers the first record with `replayed:
  true` and no code: the same `expires_at`, `generation`, `holder` and `subject` the first answer
  gave, whatever moved since. The controller keeps that record as long as it keeps the code; after a
  restart, or 10 minutes past the code's expiry, a replay carries the facts the store still has, with
  `expires_at` and `generation` null.
- **Lifetime.** 120 seconds (`PF_CONFIRM_TTL` can shorten it, see `cli.md`). A device has at most 4
  live codes: a fifth issue voids the oldest. Codes live in memory only, and a restart voids all of
  them (ADR-0007).
- **Spending.** One presentation is one use. In order, a code is refused when: the table never
  held it (`code_unknown`; every live code of the calling device is then voided); it is another
  device's (`code_foreign`; that code is spent); it was spent (`code_used`); it was voided by the
  cap or by a revoke (`code_voided`); it is past its time (`code_expired`); its request, verb or
  parameters differ from the call's (`code_subject`). A code that passes these is spent at once.
  Then the verb checks the request (`subject_moved`: the request is closed or its revision moved,
  or for an ack the PR's head moved) and the claim (`claim_moved`, see "The claim"). Each of these
  spends the code too. The device asks for a new code after any refusal.
- **Tombstones.** An issued code stays in the table as a tombstone after it is spent, voided or
  expired, until 10 minutes past its time (`Config.confirm_tombstone`) or until the device holds more
  than 64 tombstones. So presenting a voided or expired code never voids the device's other live
  codes, and a tombstone keeps its device, so another device that shows it gets `code_foreign`. Only
  a hash the table never held, or one already dropped, is `code_unknown`.
- **The refusal words** are a closed list: `code_unknown`, `code_foreign`, `code_used`,
  `code_voided`, `code_expired`, `code_subject`, `subject_moved`, `claim_moved`, `device_revoked`.
  Each refusal's text names its cause and never repeats the code.
- **Revoke.** `device revoke` voids the device's live codes, and its next API call is 401. An act
  already past the code check completes, so an ack in flight still merges. This is a known limit.
- **What it proves.** The code proves the device, the freshness and the head SHA. It does not prove
  that a person pressed the button.
- **Socket.** `confirm_code`, and `answer`, `clear` or `ack` with `code` and no `token`, are refused
  on the Unix socket: `confirm codes are for paired devices; on this host use pulse-factory claim`.

## Resuming a unit

`clear` and `answer` on a request that has a unit resume the unit:

- the unit is `parked`, and the request is its open request;
- the caller holds the current claim;
- the stage is a stage of the unit's track (`clear --to`); the default is the stage it parked in;
- the pins of the run's whole manifest are checked first, and a changed file refuses the resume,
  names the file and changes nothing.

An `answer` (not a `clear`) first runs the workflow's `on_answer` hook for the request, if it
declares one (`workflow-file.md`, "Answer hooks"). If the hook fails, the request is answered and
the unit parks again with a new `park` request whose reason is `answer hook failed: <cause>`; the
unit does not resume. The hook's run is a ledger line, `answer hook <R-n>: rc <n>`.

The unit becomes `running` at that stage with no session, and its park generation rises. The next
tick launches the stage through the normal path (capacity, pins, `before`). A unit that parked
after its PR was recorded keeps its PR. A halted run accepts the resume and launches nothing. The
resume does not remove a stop file or a correction packet. The operator does, or the workflow's
hook retires it (`workflow-file.md`, "Answer hooks"): the `ossify-spine` hooks rename a stop file
to `stop.answered-<R-n>` and a correction packet to `correction.txt.answered-<R-n>`. So after a
`clear` the operator moves the packet away first, or the stage parks on it again. A `park` whose
cause is a cap (`<stage>: <to> cap <max> reached`) has no hook: its stage has ended, and `clear` is
the way on.

An answer's text is data. The engine stores it and shows it, and never runs or renders it as a
template.

## Known limit

The stall counter counts failed `stalled:` stage nodes. A cleared unit keeps them, so a second
stall after a clear parks at once, without the relaunch.

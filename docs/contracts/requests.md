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
  be read adds nothing. A stop file that ends with an options block (see "A stop's options") also
  gains `"stop_options"`, `[{"text","recommended"}]` in file order, or `"stop_options_error"`, the
  cause in engine words, when the block breaks a rule. `stop_text` holds the whole file in both cases.
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

## The decision block

Every park names a **cause** from a closed list, and the engine stores a **decision block** in the
request's body under the key `decision`. The block is data for a reader: it says why the work stopped,
what each possible answer does, and what a fresh session needs to pick the request up. Nothing acts on
an option. Every verb works as before, and no verb changes with the block (ADR-0006, amended
2026-11-21).

The command carries inputs only: the cause, and the facts in its `detail`. The engine derives the block
when it decides the command, so a replay of the command ID returns the stored request and derives
nothing. A park command with no cause (a command stored before the cause existed) opens a request with
no block, and its content bytes hold no `cause` key. `decision` is a reserved body key: a fact of that
name in a park's `detail` cannot replace the block. A revision of a `merge_ack` request (see "The
revision rule") stores a block built from the new facts, at the new revision.

| Field | Meaning |
|---|---|
| `v` | The version of the block's shape. It is 1. A reader that finds a larger number reads no block. |
| `cause` | The cause code (the table below). |
| `summary` | One sentence the controller writes from the cause and its own facts: the unit, the stage, the attempt, a cap's number, a PR. It never holds a seat's words: the seat's text stays in `stop_text` and `gaps`. At most 300 characters. |
| `options` | 0 to 4 objects `{id, label, detail, effect}`. `effect` is `{"verb":"answer","text":…}`, `{"verb":"resume"}`, `{"verb":"resume","to":<stage>}` or `{"verb":"ack"}`. |
| `recommended` | Absent, or `{option, by, why}`. `by` is `controller`, `workflow` or `seat`. |
| `refs` | `{project, run, unit, stage, pr}`. A fact that does not exist is left out: `project` for a run armed from the CLI, `stage` when empty, `pr` before a PR is recorded. |
| `attempt` | The number of stage nodes of the parked stage for the unit. It is 0 for a park at the merge. |
| `asked_by` | `{"kind":"seat","role":…,"unit":…}` for `stop_file` and `park_file`, where the seat's own file opened the request. `{"kind":"controller"}` for every other cause. |
| `about` | `{"role":…,"unit":…}`, the seat the halt concerns. Present only for `stalled`, `no_done_signal`, `seat_lost` and `host_unreachable`, and only when a stage node of the unit exists. |
| `options_error` | Present only for `stop_file`, when the stop file's options could not be read: the cause from `stop_options_error`. |
| `takes_text` | True when the controller says the request's own file declares an `on_answer` hook, so a typed answer reaches a seat. False otherwise. |

A seat's option text may be in `label` and in the `answer` effect's `text`: it is the seat's own line,
redacted and bounded. Every other string of the block is written by the controller and holds no seat
text. `show` prints a label with every control character replaced.

### The causes

The cause, the kind of the request it opens, the options and the recommendation. A recommendation is
`controller` only for the four causes that ADR-0006's amendment of 2026-11-16 lists as below the line.

| Cause | Kind | Options | Recommendation |
|---|---|---|---|
| `stop_file` | `stop` | The seat's options (see "A stop's options"), `s1` to `s4` in file order, each `answer` with the option's text. None when the file names none. | The seat's mark, `by: "seat"`. |
| `park_file` | `gaps`, `red_gate` or `park`, by the file's name | None when `takes_text` is true: the typed answer is the act. Otherwise `resume`, and its `detail` says the file must be gone first or the unit parks again. | None. |
| `no_done_signal` | `park` | `resume` | None. |
| `stalled` | `stall` | `resume` | `resume`, `by: "controller"`. |
| `launch_refused` | `park` | `resume` | None. |
| `launch_failed` | `park` | `resume` | None. The agent port's launch error does not say that its cause is transient. |
| `outcome_cap` | `park` | `again` (`resume` to the capped target stage, from the facts `cap_to` and `cap_max`) and `go_on` (`resume`) | None. |
| `hook_failed` | `park` | `resume` | None. |
| `answer_hook_failed` | `park` | `resume` | None. |
| `pins_changed` | `park` | `resume` | None. |
| `base_push_failed` | `park` | `resume` | None. |
| `base_moved` | `park` | `resume` | None. |
| `render_failed` | `park` | `resume` | None. |
| `decisions_too_large` | `park` | `resume` | None. |
| `no_pr` | `park` | `resume` | None. |
| `merge_conflict` | `merge_failed` | `resume` | None. |
| `merge_checks_red` | `merge_failed` | `resume` | `resume`, `by: "controller"`. |
| `merge_refused` | `merge_failed` | `resume` | None. |
| `merge_error` | `merge_failed` | `resume` | None. A forge error, and an outcome no other merge cause fits. |
| `seat_lost` | `seat_lost` | `resume` | `resume`, `by: "controller"`. |
| `host_unreachable` | `seat_lost` | `resume` | `resume`, `by: "controller"`. |
| `merge_waits_ack` | `merge_ack` | `approve` (`ack`) | Never. |

The outcome cap's `again` and `go_on` say that a resume grants a new budget: the outcome counters
start again. The merge summary names the first 12 characters of the head SHA, so a revision changes the
block.

### A stop's options

A stop file may end with this block, and nothing but blank lines after it:

```text
Options:
- <one line of text>
- [recommended] <one line of text>
```

- The line `Options:` stands alone at the start of a line. The last such line of the file opens the block.
- Two to four lines follow, each starting with `- `. At most one carries `[recommended] `.
- Each option's text is redacted first, then must hold 1 to 200 characters and no control character.
  The text is all there is: no label, no stage, no verb.
- A block that breaks a rule yields no option, and the body gains `stop_options_error` with the cause in
  engine words: an `Options:` line that is not alone on its line, a line that does not start with `- `,
  text after the block, an empty option, an option over 200 characters, a control character, fewer than
  two or more than four options, or more than one `[recommended]` mark. The stop itself stands, with
  its text as before, and the request's block gains `options_error`.
- A valid block gives `stop_options`, and the block's options are `s1` to `s4` in file order. The marked
  option is the recommendation, `by: "seat"`.
- A stop file with no `Options:` line gives neither `stop_options` nor an error.

**Known limit:** the controller reads at most 64 KiB of a stop file. A file over that bound that holds
an `Options:` line gives `stop_options_error` (its block may be cut). A file over the bound whose
options block starts after the first 64 KiB is not read, and gives no error.

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
| Add a notice (`@controller`) | `notice:<R-n>:<revision>:<event>` |
| Record a send of a notice | `notice-send:<position>:<target>:<n>` (n is the stored count plus 1) |
| Acknowledge a notice, or join a target | `notice-ack:<device>:<position>` for a device, `notice-ack:<project>.<agent>:<position>` for an agent |
| Supersede a delivery | `notice-supersede:<position>:<channel>` |

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

## Notices

A request that waits for an answer is of no use if nobody knows. The controller derives a **notice**
from each request event and delivers it on two channels (ADR-0006's amendment of 2026-11-21). The
**app** channel sends a `notice` frame to each paired device with a stream open (`api.md`, "The
stream") and waits for its `notice_ack`. The **orchestrator** channel writes a `notice` item into the
project's Orchestrator chat and carries the waiting notices into the operator's next message. A notice
starts no turn and no run, so `halt` does not stop one. A notice that its target has not acknowledged
is sent again until the target acknowledges it or the request closes.

### The events

A notice records one event of one request, and `(request, revision, event)` is unique:

| Event | When the controller adds it |
|---|---|
| `opened` | An open request has no `opened` notice. The revision is the request's revision at that tick. |
| `revised` | An open request is at a revision above the highest revision any notice of it holds. A revision that came and went between two ticks gets none, as it gets no `request` frame. |
| `closed` | A request is answered or withdrawn, has an `opened` notice and has no `closed` one. |

A request that closed before the controller saw it open gets nothing: nobody was told it opened. So
the first tick after an upgrade writes notices for the requests that are open then, and none for the
history. The controller derives notices at the end of each tick and after each request verb, from the
store alone. A crash between a request and its notice loses nothing: the next tick adds the notice. A
write that fails is printed once and tried again at the next tick. A controller whose commands are
stopped writes no notice, and `status` says so in `warnings`.

A notice is one admitted command, `notice:<R-n>:<revision>:<event>`, under the admission scope
`@controller`. A replay returns the first record. The store gives each notice its **position**, one
sequence for the whole store from 1. The ledger line is:

```
notice <position> request=<R-n>@<revision> event=<event> run=<run> project=<P-n|-> app=<state> orchestrator=<state> command=<id>
```

`<state>` is `waiting`, or `superseded:<why>`. The line holds no text.

### The text

The controller writes one text for each channel when it adds the notice. The source is the request's
decision block: its `summary`, which holds no seat's words (see "The decision block"). A request with
no block gets `Request <R-n> of run <run> is open.` The reason, the stop text and the gaps never enter
a notice. The text is redacted once, then cut to the channel's word cap: words are split at white
space, whole words are kept, and a cut text ends with `…`. A `revised` text starts `Changed: `. A
`closed` text is `Request <R-n> closed: <answered|withdrawn>.` The caps are the settings
`notify.app.max_words` (5 to 200, built-in 30) and `notify.orchestrator.max_words` (10 to 500, built-in
100). The IDs are not part of the capped text. The frame carries them as fields, and the chat item and
the carry add them after the text.

### The channel states

Each notice has one **delivery** for each channel, and a delivery is `waiting` or `superseded`. The
one change a delivery allows is `waiting` to `superseded`, with a reason:

- `channel_off`: the setting `notify.<channel>.on` is 0 when the notice is written;
- `no_project`: the orchestrator channel of a run that has no project;
- `request_closed`: the notice is a `closed` notice (it never waits), or its request closed while the
  delivery waited.

The reasons are tried in this order. When a request closes, each delivery of its earlier notices that
still waits is superseded with `request_closed`. Each one is a command,
`notice-supersede:<position>:<channel>`, with the line
`notice_superseded <position> channel=<channel> why=request_closed`.

### Targets and positions

A **target** is `device:<D-n>` on the app channel, or `agent:<P-n>:orchestrator` on the orchestrator
channel (the built-in Orchestrator only). A target has one **position**, `acked`, which only rises. A
target acknowledges a position, and that acknowledges every notice of its channel at or below it.

- A device's row is made at its first stream after this item, at the newest position, with the
  outcome `joined`. It is told of the open requests by the `requests` frame, not by old notices. A
  device that pairs later joins the same way.
- An agent's row is made with the first notice of one of its project's runs, just below that notice,
  so that the notice waits for it.
- A target's **waiting set** is the deliveries of its channel that are `waiting`, above its `acked`
  and, for an agent, of its own project.
- Each acknowledgement is one command, with the line
  `notice_ack <target> position=<n> outcome=<outcome>`. The outcome is `joined`, `shown`, `blocked` or
  `carried`. A position at or below the stored one admits nothing: the controller checks first and
  commits nothing, and the store refuses it as well. A position above the newest notice is refused.

### The app channel and the re-send rule

A device with a stream open gets a waiting notice as a `notice` frame at the tick that writes it, and
a new stream gets each notice of its waiting set after the `requests` frame, oldest first. Each send is
one command, `notice-send:<position>:<target>:<n>`, with the line
`notice_send <position> target=<target> attempt=<n>`. A device with several streams gets the frame on
each, as one send.

A notice that the device has not acknowledged is sent again when
`min(900, notify.resend_seconds × 2^(sends − 1))` seconds have passed since its last send. The built-in
`notify.resend_seconds` is 60, so the waits are 60, 120, 240, 480 and then 900 seconds. The value is
read at each re-send. A reconnect sends at once. The timer runs only while the device has a stream
open: an absent device costs no send and no line. The time comes from a clock that a test can move.
There is no give-up while the request is open.

A `closed` notice is sent once to each connected device and never again, with no acknowledgement owed.

### The orchestrator channel

Each notice of a project's run is written once as a `notice` item (`request_opened`, `request_revised`
or `request_closed`) into the project's newest Orchestrator session that has not ended. The text of
the item is the channel's text, then `(request <R-n>, run <run>, unit <unit>, stage <stage>, PR <pr>)
Read it: pulse-factory show <R-n>`, with each ID that exists. With no such session the item waits, and
it is written into the session that next carries the notice. A send row means the item is stored.

When the operator sends a message to an Orchestrator session and no turn is open, the controller builds
a block from the agent's waiting set: one line for each open request, newest first, at most ten, and
then `and <n> more: pulse-factory requests`. The block starts
`[Pulse Factory notice. The controller wrote this, not the operator.]` and ends `[End of notice]`. The
controller sends the block, a blank line and the typed text as one `Operator` send (`chat-adapter.md`).
The stored `OperatorMessage` holds the typed text alone. A message that is sent while a turn is open
carries nothing. Each carry is one `notice-send` command for each notice it lists.

The live chat remembers the highest position that the send carried. When the next `turn_result` item
with the cause `operator` is **stored**, the controller acknowledges that position for the agent, with
the outcome `carried`. A send that fails, a session that ends first and a controller restart in the
turn each leave the notices waiting for the next message. No notice starts a turn, and the channel has
no timer.

### Settings

Five keys rule the channels (`cli.md`, `settings`): `notify.app.on`, `notify.orchestrator.on`,
`notify.app.max_words`, `notify.orchestrator.max_words` and `notify.resend_seconds`. The context of a
notice is its run's project, or the server scope with none. `on` and `max_words` are read when a notice
is written, and `resend_seconds` at each re-send. A chat or a seat cannot set them.

### Known limits

- A client that connects and never acknowledges (an app from before this contract) gets each waiting
  notice again at most every 15 minutes while its request is open, and each send is one admission.
  Nothing gives up while the request is open. The admission-rows debt line covers the growth.
- A stored turn result proves that the input arrived. It does not prove that the model heeded it.
- A carry can repeat after a controller restart in the turn.
- The text of an `opened` notice is the summary of the block as it was when the notice was written.
  A later change of a cap or a setting changes no notice that is written.

## Known limit

The stall counter counts failed `stalled:` stage nodes. A cleared unit keeps them, so a second
stall after a clear parks at once, without the relaunch.

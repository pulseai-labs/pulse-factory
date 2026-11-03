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
| `merge_ack` | The spine merge waits on the claim. A later work item opens it. |

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
| Withdraw a request | `withdraw:<id>` |
| Record a run of an answer hook | `answer-hook:<id>:<revision>:<n>` (n counts the runs for the request and revision) |
| Claim | `claim:<generation>` |
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

### The token file

`claim` returns the token once, to the client. The client writes it to `<state dir>/claim-token`
with mode 0600 and replaces any older file. It prints only the generation and the path. `answer`
and `clear` read the file named by `--claim-token-file`, else the default. A missing or empty file
is refused, and the refusal names the path.

The token reaches no log, ledger line, `status`, request body, `requests`, `show` or error text.

### The caller check

On every `claim`, `answer` and `clear` the controller:

1. reads the peer's PID from the socket (`SO_PEERCRED`);
2. refuses a peer whose `/proc/<pid>/environ` holds `PF_SEAT_TOKEN`;
3. walks the parent chain through `/proc/<pid>/stat` and refuses when any process on it is a seat
   process the controller launched and still tracks (`AgentPort::process_id`, see
   `agent-adapter.md`).

Any read that fails refuses, and so does a chain that does not end. The client refuses too when
`PF_SEAT_TOKEN` is set. A refusal records nothing.

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
resume does not remove a stop file. The operator does, or the workflow's stop hook retires it
(`workflow-file.md`, "Answer hooks"): the `ossify-spine` hook renames it to `stop.answered-<R-n>`.

An answer's text is data. The engine stores it and shows it, and never runs or renders it as a
template.

## Known limit

The stall counter counts failed `stalled:` stage nodes. A cleared unit keeps them, so a second
stall after a clear parks at once, without the relaunch.

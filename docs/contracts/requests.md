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
| `answer` | JSON object or null | `{"verb":"answer","text":…}` (it gains `"to_stage"` when the answer named a stage, and `"option":{"id","by","recommended"}` when it took an option: see "Acting on an option"), `{"verb":"clear","to_stage":…}` (it gains the same `option` key), `{"verb":"ack","head_sha":…}` (with the same `option` key) or `{"verb":"withdraw","reason":…}`. |
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

- `park_file` with declared options: the body gains `"park_options"`, `[{"label", "answer", "to",
  "recommended"}, …]` in file order, from the `park_on` entry that opened the request (an absent `answer`
  or `to` is left out), and `takes_text`, as every `park_file` request holds it. A `gaps` request gains no
  `park_options`, whatever its file declares.

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
what each possible answer does, and what a fresh session needs to pick the request up. An act can name
one of its options (`--option`, see "Acting on an option"): the engine reads what the option does from
the stored block, never from the caller. An act with no option works as before (ADR-0004, ADR-0006,
amended 2026-11-21).

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
| `options` | 0 to 4 objects `{id, label, detail, effect, by}`. `effect` is `{"verb":"answer","text":…}` (it may hold `"to":<stage>`: resume there), `{"verb":"resume"}`, `{"verb":"resume","to":<stage>}` or `{"verb":"ack"}`. `by` says who wrote the option: `controller`, `workflow` or `seat`. The builder writes `seat` for a stop file's option, `workflow` for an option that a `park_on` entry declared (see "A workflow's options") and `controller` for every other. A block stored before `by` existed has none: a reader takes `seat` when the cause is `stop_file` and `controller` otherwise. |
| `recommended` | Absent, or `{option, by, why}`. `by` is `controller`, `workflow` or `seat`. The builder writes `workflow` for the mark of a `park_on` entry's option, with the fixed reason `The workflow marked this option.` |
| `refs` | `{project, run, unit, stage, pr}`. A fact that does not exist is left out: `project` for a run armed from the CLI, `stage` when empty, `pr` before a PR is recorded. |
| `attempt` | The number of stage nodes of the parked stage for the unit. It is 0 for a park at the merge. |
| `asked_by` | `{"kind":"seat","role":…,"unit":…}` for `stop_file` and `park_file`, where the seat's own file opened the request. `{"kind":"controller"}` for every other cause. |
| `about` | `{"role":…,"unit":…}`, the seat the halt concerns. Present only for `stalled`, `no_done_signal`, `seat_lost` and `host_unreachable`, and only when a stage node of the unit exists. |
| `options_error` | Present only for `stop_file`, when the stop file's options could not be read: the cause from `stop_options_error`. |
| `takes_text` | True when a typed answer reaches a seat: the request's own file declares an `on_answer` hook, or the parked stage declares a fallback `on_answer` (`workflow-file.md`, "Answer hooks"). False otherwise, and always for a park at the stage `merge`. The `park_file` option rule reads the file's own hook alone. |

A seat's option text may be in `label` and in the `answer` effect's `text`: it is the seat's own line,
redacted and bounded. Every other string of the block is written by the controller and holds no seat
text. `show` prints a label with every control character replaced.

### The causes

The cause, the kind of the request it opens, the options and the recommendation. A recommendation is
`controller` only for four of the five reasons that ADR-0006's amendment of 2026-11-16 lists as below the
line: `stalled`, `seat_lost`, `host_unreachable` and `merge_checks_red`. A failed launch with a transient
cause is below the line and carries no recommendation. Which side of the line a cause is on is in
"The line": the class is a function of the cause and of the track's merge, and this table does not
repeat it.

| Cause | Kind | Options | Recommendation |
|---|---|---|---|
| `stop_file` | `stop` | The seat's options (see "A stop's options"), `s1` to `s4` in file order, each `answer` with the option's text. None when the file names none. | The seat's mark, `by: "seat"`. |
| `park_file` | `gaps`, `red_gate` or `park`, by the file's name | The entry's declared options (see "A workflow's options"), `w1` to `w4` in file order, each `by: "workflow"`, and no controller `resume` beside them. With none declared: none when `takes_text` is true, the typed answer being the act; otherwise `resume`, and its `detail` says the file must be gone first or the unit parks again. | The workflow's mark, `by: "workflow"`. None when no option is marked. |
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
| `resolution_changed_guarded` | `park` | `resume` | None. The unit resumes at the stage after the conflict stage. |
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

### A workflow's options

A `park_on` entry of the workflow file may declare `options` (`workflow-file.md`, "`options` of a
`park_on` entry"). The park command carries them as the fact `park_options` in its `detail`, as a stop's
options ride as `stop_options`, so the engine derives the block when it decides the command and a replay of
the command ID returns the stored request. The facts of a park come from the one entry that opened the
request: its hook and its options are of one entry. A `gaps` request gets no such fact, whatever the file
declares. The fact under any other cause adds nothing.

- **The block's options are exactly the declared ones,** `w1` to `w4` in file order, each `by: "workflow"`.
  An ID keeps the entry's place in the file. The builder reads the fact as data: it skips an entry with no
  label or with neither an answer text nor a stage, reads at most 4, and writes only `answer` and `resume`
  effects from it, whatever keys an entry holds. A workflow's option can never carry an `ack`.
- **Three effects.** An option with `answer` has the effect `{"verb":"answer","text":<answer>}`, with
  `"to":<stage>` when the option names a stage. An option with `to` alone has `{"verb":"resume","to":<stage>}`.
  The `detail` is written by the controller: `Answer with the workflow's text.`, `Answer with the workflow's
  text, then resume the unit at stage <stage>.` or `Resume the unit at stage <stage>. The park file must be
  gone first, or the unit parks again.`
- **Declared options replace the controller's `resume`.** A `clear` runs no hook, so the file would park the
  unit again. A workflow that wants a resume declares a stage-only option. With no option declared, the block
  is as it was: no option when the file's own hook takes a text, and otherwise one `resume` by the controller
  with the warning that the park file must be gone first.
- **The recommendation.** The first option marked `recommended: true` gives `{"option":"w<n>",
  "by":"workflow","why":"The workflow marked this option."}`. The reason is that fixed text, as a seat's is
  "The seat marked this option." A workflow's file writes no reason. No mark gives no recommendation.
- `takes_text`, `asked_by` (the seat whose file opened the request), `summary`, `refs` and `v` do not change.
  The typed answer stays beside the options whenever `takes_text` is true.
- **Acting on one** is round 2's act path, which does not change ("Acting on an option"). `answer --option w1`
  runs the entry's hook, or else the stage's fallback, with `{answer.source}` as `option:w1:workflow`, so the
  note says that the workflow wrote the words. `clear --option <id> --rev <k>` takes a stage-only option. A
  device's confirm code binds the option's ID. The engine still refuses a stage outside the unit's track at
  the act.

### Acting on an option

`answer`, `clear` and `ack` take `--option <id>` (`cli.md`). The act names the option **in place of** its
own parameters, and the engine reads the option from the request's stored block: its effect decides what
the act does. The caller never says what an option means. An option ID is 1 to 32 characters of `a-z`,
`0-9`, `_` and `-`; any other ID is refused before a read.

| Verb | The option's effect must be | The act |
|---|---|---|
| `answer --option` | `answer` | The answer's text is the effect's `text`, and its stage is the effect's `to`, if any. |
| `clear --option --rev` | `resume` | The unit resumes at the effect's `to`, or at the stage it parked in. `--rev` is required and must be the request's current revision: an option belongs to one revision. |
| `ack --option` | `ack` | Today's ack, with its PR read. Only a `merge_waits_ack` request holds an `ack` option. |

- **A wrong verb is refused and names the right one:** `option "resume" of request R-3 resumes the unit:
  take it with `clear``.
- **An act with an option carries no parameters of its own:** a text, a stage or a `to_stage` beside an
  option is refused (`an act with an option carries no parameters of its own`), and nothing changes. An
  unknown option is refused (`request <R-n> holds no option "<id>" at revision <k>`). So is an option on a
  request with no block, on a block of another version, and on a request with no unit.
- **`answer --text … --to <stage>`** resumes at that stage. A stage outside the unit's track is refused,
  and so is a stage on a request with no unit.
- **The stored answer** gains `"option": {"id", "by", "recommended"}` beside `text`, `to_stage` and
  `head_sha`. An option's `answer` stores the resolved text, so a reader needs no second lookup. A typed
  answer stores no `option` key. `recommended` is true when the block's `recommended.option` names the
  option.
- **The ledger line** of a typed act does not change. With an option it reads `answer <R-n>@<revision> by
  claim <g>: option <id> by <by>[, recommended]: <what was done>`, where "what was done" is today's text
  (`clear <unit> --to <stage>`, or the first 200 characters of the text). An answer with a stage ends
  ` --to <stage>`. An ack's line gains ` option=<id>` at its end.
- **Command IDs do not change** (`answer:<id>:<revision>`, `clear:<unit>:<park generation>`,
  `ack:<id>:<revision>`), so one request takes one act, with an option or without. A stored `ClearUnit` or
  `AckRequest` with no option holds no `option` key in its content, so a replay of a typed act is as before.
- A halted run admits an option act as it admits the typed one: the unit is left with the reason `run
  halted`, and nothing launches. An ack on a halted run is refused as today.

## The line

Every request has a **line class**, `below` or `above` (ADR-0006, amended). `below` means that the
operator's clear grant may settle the request (see "The clear grant"), and `above` means that the
claim alone may. `settle` acts under the grant, and only on a `below` request (see "Settle"). Who may
`answer`, `clear` and `ack` a request is as before, for both classes.

The engine computes the class from the request's cause and from its track's merge, with one
function, `line_of(cause, free_merge)`. One exhaustive `match` over the causes gives each cause a
row, so a new cause does not compile until it has one:

| Cause | Class |
|---|---|
| `stalled`, `seat_lost`, `host_unreachable` | `below` |
| `merge_checks_red`, `merge_conflict` | `below` when the track's merge is free (below), else `above` |
| `resolution_changed_guarded` | `above` |
| every other cause, `launch_failed` included | `above` |
| no cause (a park stored before the cause existed, a request with no block) | `above` |

A failed launch is below the line only once an adapter marks its cause as transient. No adapter
does today, so `launch_failed` is `above`.

**A track's merge is free** only when all of these hold: the unit's track has a merge; its rule is
not `claim_ack`; the caller gave a forge repo and its default branch; the track's `repo`, rendered
for the unit, is that repo; and the track's `base`, rendered for the unit, is another branch than
the default branch. Anything else makes it not free: a `repo` or a `base` that does not render, an
empty `repo` or `base` (it "declares nothing"), a unit whose merge renders another repo, and a
default branch that is not given. A unit whose merge renders another repo than the run's forge repo
has no known default branch, so its merge causes are `above`.

- **One renderer.** The class renders `repo` and `base` with the merge step's own function, so the
  class and the merge cannot name different branches. A track whose `head_branch` does not render
  makes that function fail, and a failed render gives `above`.
- **Branch names are normalized before the compare.** One leading `refs/heads/` is cut from each
  name, so `refs/heads/main` equals `main`. Nothing else is folded: the compare is exact and
  case-sensitive.
- **The workflow is the run's own.** The class reads the track's merge
  from the run's pinned copy of the workflow (`<run dir>/package`, which `arm` made), never from
  the package on disk. A change to the source package after `arm` changes the class of no request
  of that run, also after a controller restart.
- **The class fails closed.** Every read that fails gives `above`: a body that is not JSON, a block
  of another version, a unit the run does not hold, a unit detail that does not read, and a run
  whose view cannot be read. A request with no unit is `above`. The engine names no repo and no
  branch of its own: the controller passes the run's forge repo and its default branch on every
  call.
- **The class is computed and never stored in a request.** No request body, command or block holds a
  class key. An open request and a closed one read the class at the read, from the cause in the
  stored block. One reader is the exception: a settled request reads the line that its settle
  recorded in its answer (see "Settle"), so a later change of the computed class does not change
  what the request showed when it was settled.

`show`, `requests` and the `request` and `requests` frames carry it as `line` (`cli.md`, `api.md`).

## The clear grant

A project's **clear grant** is a stored row that says the Orchestrator may settle the project's
requests that are below the line. The grant is off until the operator gives it, and the operator
can end it at any time. `settle` acts under the grant (see "Settle"). This section is the record
and the rules that `settle` reads.

- **The row.** One row for each project and generation, in the table `clear_grants`: the project's
  ID, the generation (an integer from 1), the agent that holds the grant (`orchestrator`), who
  gave it and when, and, once the operator ends it, who ended it and when. Each change is an
  admitted command under `@controller` with the caller's command ID.
- **The generation rule.** A new row's generation is the project's highest generation plus 1. A
  revoke ends its generation, and the next grant takes the next one, so an act under an old
  generation is refused. At most one row of a project has no revoke, so at most one grant of a
  project is live. A revoke is set once, on its row, and a row is never deleted. The database
  holds these rules as guards, so no caller can break one.
- **Who gives and ends it.** Only the operator: `grant clear` gives the grant, `grant revoke` ends
  it, and `grant list` reads it (`cli.md`, `grant`). The two that change it pass the caller check
  of "The caller check" and take no claim: a chat or a seat that calls one is refused, and neither
  reads a claim token. No grant verb is on the device allowlist.
- **A revoke commits when the controller's commands are stopped.** The controller refuses every
  command when its ledger cannot be appended to, except a device revoke and a grant revoke. Those
  commit to the store at once, and the ledger line waits in the outbox for the next start. A new
  grant is refused there like every other command.
- **The ledger lines**, in `controller.ledger`, one for each admitted command:
  `grant_clear <P-n> agent=orchestrator generation=<g> by=cli command=<id>` and
  `grant_revoke <P-n> generation=<g> by=cli command=<id>`. A replay of a command ID returns the
  first record and writes no line.
- **Refusals.** `grant clear` is refused for a project the store does not hold, for a removed
  project, and for a project whose grant is live (`project <P-n> already holds the clear grant
  (generation <g>)`). `grant revoke` is refused for a project the store does not hold and for a
  project with no open row (`project <P-n> holds no clear grant`). `halt` revokes nothing.
- **A removed project's grant is not live.** `project remove` writes no revoke. Every reader of
  "is the grant live" skips a removed project: `grant list` marks none of its rows live, and
  `requests` holds none of its grants. No code makes a removed project live again, so its grant
  cannot come back. `grant revoke` still ends the open row of a removed project, so the operator
  can always end what the store still holds.
- **The precondition.** A store commit can require that a project holds a live grant at one
  generation (`Effect::RequireLiveGrant`). The effect writes nothing. The store checks it inside
  the commit's transaction and rejects the commit, with one line and nothing written (no
  admission, no outbox line and no other effect of the commit), when the project has no open
  row, when the open row is of another generation, and when the project is removed or unknown. It
  is admitted in every scope, a run's included. Commits are serial, so a revoke is either before a
  commit that carries the precondition, which is then rejected, or after it. A revoke through the
  stopped-ledger path counts: it commits to the store at once.

## Settle

`settle` is the one verb that acts under the clear grant (ADR-0004, ADR-0005, amended). A live
session of the project's Orchestrator calls it to settle a park that is below the line:

```
pulse-factory settle <R-n> --rev <k> --act <act> [--to <stage>] --reason <text> [--json]
```

- **The verb.** `settle` names a request at the revision the caller read, never a bare unit. It
  takes one act and a reason. It reads no claim token and never joins the device allowlist. An
  admitted settle resumes the parked unit and closes the request `answered`. It grants no new
  budget: the run's outcome counters stay as they are, and the run's armed launch count bounds it
  (see "The launch bound").
- **The acts.** There are six, in this order: `clear`, `resume`, `rerun_check`, `redispatch`,
  `resolve_conflict` and `close_stale`. Each cause admits only its own acts, from one exhaustive
  `match` over the causes, so a new cause does not compile until it has a row:

  | Cause | Acts |
  |---|---|
  | `stalled` | `clear`, `redispatch` |
  | `seat_lost`, `host_unreachable` | `resume` |
  | `merge_checks_red` | `rerun_check`, `redispatch` |
  | `merge_conflict` | `resolve_conflict` |
  | `resolution_changed_guarded`, every other cause, and a request with no cause | none |

- **Which acts this server builds.** Every act but `close_stale`. `clear` and `redispatch` on
  `stalled`; `rerun_check` and `redispatch` on `merge_checks_red`; `resume` on `seat_lost` and on
  `host_unreachable`; `resolve_conflict` on `merge_conflict`. The controller refuses `close_stale`
  before it builds a command, with `settle refused: close_stale has no object: a revision replaces
  its request in the same row, so no stale request exists`.
- **`--to`.** Only `redispatch` takes it, and `redispatch` needs it: a redispatch with no `--to`
  would be a `clear`. It names a stage of the unit's track at or before the stage the unit parked
  in. A unit parked at its merge has every stage before it. A later stage is refused: `--to <stage>
  is after the stage the unit parked in (<parked>): a settle never skips a stage`. A stage that the
  track lacks is refused with the track's stages, as `clear --to` is. A redispatch drops the done
  mark of every stage from `--to` on.
- **The checks, in order, and where they run.** Each refusal starts `settle refused: ` and writes
  no admission under the settle's own ID.
  - the controller, before the command: the OS (only Linux admits a settle); the caller (see "The
    caller of a settle"); the arguments (an act of the six, `--to` as above, a reason of 1 to 300
    characters on one line with no control character); who (the session is live and is of the
    agent `orchestrator`, the request is known, its run has a project, and that project is the
    session's); the grant (a live row for the project, of one generation; a removed project has
    none); the act list (`authority.clear_acts`, read fresh, must hold the act); `close_stale`; the
    run's pins (a pinned file that changed after `arm` refuses and
    names the file); and the reason, redacted once and then bounded. For `rerun_check` the
    controller also reads `authority.rerun_checks_per_head` for the project and the agent
    `orchestrator`, fresh, and hands it to the engine as the limit;
  - `Engine::settle`, under the engine's lock: a degraded engine refuses; the replay lookup comes
    first (see "The replay"); the request must be open at the named revision; its class must be
    `below`; its cause must admit the act; then the line record is built and the command is
    submitted. The lock is held from the class read to the commit, so the class that is read is
    the class of the request that the commit closes;
  - the decision: it checks the request and the unit again, and a halted run, the cause, `--to`,
    the launch bound, and it builds the effects. A halted run refuses every settle, for every act:
    `run <run> is halted: a halted run refuses every settle`;
  - the store, inside the commit: the commit carries the precondition that the grant is live at the
    generation the controller read (`Effect::RequireLiveGrant`). A revoke that commits first,
    through either path, makes the store reject the whole commit: nothing is written, the request
    stays open and the unit keeps its park.
- **The refusal texts of a moved request.** A request revised before the settle is refused with the
  word `subject_moved` and `request <R-n> is at revision <n>, not <k>`. A request that is not open
  is refused with the same word and `request <R-n> is not open any more`. These are the two texts of
  the confirm path. An `above` request is refused with `request <R-n> is above the line: only the
  claim may settle it`, with no command built. A cause that does not admit the act is refused with
  `the cause <cause> admits <acts>, not <act>`.
- **The replay.** The command ID is `settle:<R-n>:<revision>`. The canonical content of a settle
  holds the run, the command ID, the act and `--to`, and nothing else. A retry with the same act
  and `--to` returns the first record (`replayed: true`) and writes no line, also after the request
  closed, and whatever the retry says as its reason, session or generation. Another act or another
  `--to` on the settled revision is refused: `revision <k> of <R-n> was settled with another act`.
  The controller does not read the admission first. A retry passes the controller's checks again,
  so a retry that a check now refuses is refused and audited, and the first record stands. That
  holds after a revoke, a narrowed `authority.clear_acts`, a moved pin and an ended session.
- **The launch bound.** The run row holds the run's armed launch count, which `arm` stores once
  (`run-launch-bound-<run>`, ledger line `run_launch_bound <run> most_launches=<n> command=<id>`).
  A plan whose most launches are unknown stores none, and its run refuses every launching act
  (`clear`, `resume`, `redispatch` and `resolve_conflict`): `run <run> holds no armed launch count,
  so the grant cannot launch in it`. Otherwise the decision counts the run's **launches so far**:
  every admitted `launch` of the run, and every `running` unit that a `clear`, an `answer` or a
  launching settle resumed and that has not launched since. An `ack` adds nothing. What each act
  adds, and where its count ends: `clear` and `redispatch` add their unit from the commit until
  the stage's `launch` is admitted. `resolve_conflict` does the same, for the conflict stage's
  launch. `resume` on `host_unreachable` adds its unit from the commit until its result: `attached`
  frees the room again, because nothing launched; `failed` and `unknown` park the unit, so it is no
  longer `running` and leaves the count; `gone` keeps the unit in the count until the relaunch's
  `launch` is admitted, so it is counted once from the commit on. `rerun_check` is not a launching
  act and adds nothing. A launching
  settle is refused when that number has reached the count: `the run's launches so far (<n>, <a>
  admitted and <p> resumed and waiting to launch) have reached its armed launch count (<m>); the
  grant cannot launch more in this run; the operator can still clear`. The request stays open and
  the unit keeps its park. The claim's `clear` and `answer` are not bound. The workflow's own
  launches after a redispatch are not reserved: the unit's outcome caps bound them.
- **The stored answer** of the request holds `{"verb":"settle","act":…,"to_stage"?:…,"reason":…,
  "grant":{"project","generation","agent","session"},"line":{…}}`, and `"head_sha"` for `rerun_check`
  and `resolve_conflict`: the head of the unit's PR that the engine read before the commit. The `line` record holds the
  class `below`, the cause and, for a merge cause, the rule, the base, the repo and the default
  branch as the class compared them. `answer_generation` stays empty: it is the claim's. A settled
  request reads its `line` from this record and not from a new computation, and `settled` names
  the act, the reason, the session, the project and the generation (`cli.md`, `api.md`).
- **The ledger line**, in the run's ledger: `settle <R-n>@<revision> by grant <P-n>#<g> session
  <S-n>: act=<act>[ head=<sha>][ --to <stage>] reason=<text>`. The `head=` part is there for
  `rerun_check` and `resolve_conflict`. The reason is redacted before it is stored or
  logged.
- **Audited refusals.** Every refusal after the OS check is one refused admission in
  `controller.ledger`, kind `settle`, under an ID the controller makes and never under the settle's
  ID. The bucket of a refusal window is the request ID when the request is in a run the
  controller holds, else `other`. The caller is the session when the check named it, else
  `pid:<n>`, or `unknown` with no peer. Repeated refusals in the window coalesce. A replay writes
  no line.
- **The OS rule.** A controller whose OS is not Linux refuses every settle: `settle refused: this
  server runs on <os>; the clear grant's acts are admitted on Linux only (the grant can still be
  given and ended here)`. `settle` is refused on every OS but Linux.

### An act with an effect

An act with an effect commits first and runs its effect after. Two acts have an effect outside the
store: `rerun_check` asks the forge to run a PR's failed checks again, and `resume` on
`host_unreachable` attaches to the recorded host again. `resolve_conflict` has no effect of its own:
its effect is the normal launch of the conflict stage, and it has no result command.

- **The pending mark.** It is not a field of any row. A unit has a *pending settle* when its newest
  admitted `settle` is of an act with an effect and no admitted `settle-result` names the same
  request and revision.
- **The result command.** After the commit, the controller runs the effect before it answers the
  verb, and the engine stores what the effect did as one command, `settle-result:<R-n>:<revision>`.
  Its content is the request, the revision, the outcome, one redacted line of detail (at most 300
  characters) and, for `attached`, the process reference. The same content again is a replay; another
  outcome is a conflict. No verb sends it. A replay of the settle runs no second effect.
- **The five outcomes.** `ran` (the forge ran every failed check again): nothing else changes, and
  the unit runs on at its merge. `attached` (the host answered): a new stage node of the parked stage
  runs on the same process reference, with no `launch` admission, and the unit stays `running`.
  `gone` (the host is gone): the unit stays resumed at its parked stage, and the next tick launches
  the stage as a new incarnation. `failed` and `unknown`: the unit parks again, as a new request with
  the same kind and cause as the settled one, and a reason that names the settle
  (`<R-n>@<k>`). The park generation was raised by the settle, so the park is a new request.
  `unknown` means the result was never stored because the controller stopped.
- **The hold.** Until the result is stored, no launch of the unit is admitted, the driver starts no
  stage of it, and the merge step waits for it (`settle <R-n>@<k> waits for its effect's result`).
- **The halt after the commit.** A run halted between the commit and the effect runs no effect. The
  result is `failed` with the text `the run is halted: the effect did not run`, and the unit parks
  again.
- **The start-up reconcile.** Before tier 1, the reconcile closes every pending settle of the run with
  one `unknown` result, which parks the unit. The effect is never run a second time.
- **The ledger line of a result**, one per result, in the run's ledger: `settle-result <R-n>@<k>:
  rerun_check ran repo=<repo> pr=<n> sha=<sha> checks=<a,b>`; `… rerun_check failed repo=… pr=…
  sha=…: ran=<…> not_ran=<name: why; …>; unit <u> parked`; `… resume attached host=<unit> session=<S>
  stage=<stage> attempt=<n>`; `… resume gone host=<unit>: stage <stage> launches again`; `… resume
  failed: <why>; unit <u> parked`; `… unknown: <act> was not run again; unit <u> parked`. The
  `rerun_check ran` line is the audit line of the re-run. It is written only when the forge answered
  and ran every failed check again on the stored head. After a forge error its `ran=` list is empty
  and no `ran` line is written.

**`rerun_check`.** The engine reads the unit's PR before the commit, with no lock held, and only for
a settle that the class and the cause check admitted. A PR with no failed check is refused (`PR
<repo>#<n> has no failed check to run again`). A failed check that the forge cannot run again is
refused and named (`the failed check <name> of PR <repo>#<n> cannot be run again by this forge:
<why>`). A read that fails is refused (`the PR's checks cannot be read: <why>`). The head that the
read gave is stored in the answer as `head_sha`. The decision counts the unit's answered requests
whose stored answer is a `rerun_check` on that head. At or above `authority.rerun_checks_per_head`
(built-in 1) the act is refused: `the act rerun_check was admitted <n> times for head <sha> of this
unit's PR, and authority.rerun_checks_per_head is <limit>; redispatch, or the operator, remains`. A
new head starts the count at 0. The unit resumes at its merge, and the effect asks the forge to run
the failed checks again on the stored head only. A head that moved gets no re-run, and the result is
`failed`. The merge then waits while the checks are pending.

**The re-attach of `resume`.** On `host_unreachable`, `resume` attaches to the recorded host again
before any relaunch. The effect reads the unit's newest session with a process reference and asks the
agent to rebind it. A host that answers is `attached`, and its stage runs on. A host that is gone is
`gone`, and the stage launches again. With the cause `no session host`, liveness decides, as tier 1
does. A host that still does not answer, a session with no process reference and an agent error are
`failed`: the unit parks again. A re-attach runs the host's and the controller's attach checks (see
`session-host.md`, "The attach checks").

**`resolve_conflict`.** The unit's track must declare `merge.on_conflict` in the run's pinned
workflow, else the act is refused: `track <t> declares no on_conflict stage, so resolve_conflict has
no stage to send the unit to`. The engine reads the PR's head before the commit and stores it as
`head_sha`. The unit resumes at the conflict stage and the next tick launches it. When the stage is
done, the controller compares the paths that the resolution changed with the track's `guarded` globs
(see `workflow-file.md`, "The conflict stage"). A change of a guarded path, or a read that fails,
parks the unit with the cause `resolution_changed_guarded`, in the same commit as the stage's end.
Otherwise the unit goes to the track's `then` stage, and the merge rule runs only after it.

### The caller of a settle

The controller names the caller's session from its process and from its host. The socket gives it a
duplicate of the connection, and it reads the caller's pidfd (`SO_PEERPIDFD`) from it. A kernel with
no such option refuses every settle. A caller with no socket (the API path) is refused. The check, in
order, with every failed read a refusal:

1. the PID from the pidfd;
2. a seat refuses: `PF_SEAT_TOKEN` in the caller's environment, or a tracked seat on its chain;
3. the chain: each process from the caller up is pinned by its PID and its start time, up to the
   first live chat process, which names the session. No chat on the chain refuses, and so does a
   chain that is too long. A chat marker that names another session refuses. A marker never names
   a caller;
4. the host: a live host row of kind `chat` for that session;
5. the launcher: under `systemd` the caller's cgroup must equal the cgroup of the host row's unit,
   so the ancestry and the host's check must both name the same chat session. Under `detached` the
   ancestry is the whole check (a stated residue). The check opens no connection to a host and
   writes no attach line;
6. the end: the pidfd still names the same PID, and every pinned process still has its start time.
   A process that exited, or a PID that is used again, refuses.

Under `systemd`, `grant clear` and `grant revoke` also refuse a caller that runs in the cgroup of a
live chat host unit: `<verb> refused: the caller runs in the cgroup of chat host unit <unit>, so it
runs under a chat`. The operator's CLI runs in no chat host unit, so it passes. Under `detached` the
same caller is admitted: that is the stated residue of the detached launcher.

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
| Settle a request (the Orchestrator, under the clear grant) | `settle:<R-n>:<revision>` |
| Record the result of a settle's effect | `settle-result:<R-n>:<revision>` |
| Store a run's armed launch count (`@controller`) | `run-launch-bound-<run>` |
| Give or end a clear grant (`@controller`) | the caller's `--command-id`, random by default; the admission's unit is `<P-n>:<generation>` |
| Park a unit | `park:<unit>:<park generation>` |
| Merge a unit | `merge:<unit>:<park generation>` |
| Record a merge result | `merge-result:<unit>:<park generation>` |
| Add a notice (`@controller`) | `notice:<R-n>:<revision>:<event>` |
| Record a send of a notice | `notice-send:<position>:<target>:<n>` (n is the stored count plus 1) |
| Acknowledge a notice, or join a target | `notice-ack:<device>:<position>` for a device, `notice-ack:<project>.<agent>:<position>` for an agent |
| Supersede a delivery | `notice-supersede:<position>:<channel>` |
| Wake the project's Orchestrator (`@controller`) | `wake:<S-n>:<seq>` (`<seq>` is the `seq` of the wake's item) |
| Mark a request woken, at the woken turn's result (`@controller`) | `woken:<R-n>` |
| Hold a wake back (`@controller`) | `wake-held:<S-n>:<reason>:<position>` (`<position>` is the highest notice position in the wake set) |

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
  one line and redacted. An act that took an option adds the option's words (see "Acting on an
  option").
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
  command=<id>`. An act or an issue for an option adds ` option=<id>`. The run ledger keeps the
  engine's `answer`/`ack … by claim <generation>` line. A
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

On every `claim`, `answer`, `clear`, `ack`, `arm`, `device pair`, `device revoke`, `project add`,
`project remove`, `grant clear` and `grant revoke`, and on the session verbs, the controller:

1. reads the peer's PID from the socket (`SO_PEERCRED`);
2. refuses a peer whose `/proc/<pid>/environ` holds `PF_SEAT_TOKEN`;
3. refuses a peer whose `/proc/<pid>/environ` holds `PULSE_FACTORY_CHAT` (the chat marker, below);
4. walks the parent chain through `/proc/<pid>/stat` and refuses when any process on it is a seat
   process or a live chat process the controller launched and still tracks
   (`AgentPort::process_id`, see `agent-adapter.md`). The refusal reads `<verb> refused: the caller
   is, or descends from, chat process <pid>`.

Any read that fails refuses, and so does a chain that does not end. The client refuses too when
`PF_SEAT_TOKEN` is set. A refusal records nothing. `arm`, `grant clear` and `grant revoke` run only
through the peer-aware path: with no peer each is refused (`arm refused: the controller cannot tell
which process called`). `halt` and
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
  `clear` code binds the target stage `to`, or its absence. An `answer` code also binds `to`.
- **An option.** A code for an option binds the option's ID **instead of** a text hash, a stage and a
  head of its own (G8). At issue the controller checks that the option exists at that revision and that
  its effect fits the verb; a subject with an option and a `text_sha256` or a `to` is refused. A code
  for a text, or for an option whose effect is `answer`, on a request that takes no text is refused at
  issue with the words of "Resuming a unit", so a device never spends a code on it. At the spend the
  call's option must be the code's (`code_subject` otherwise), and the request is read again: a revision
  between the card's render and the press is `subject_moved`. A device's option act sends an empty
  `text`. The refusal words do not change. `approve` is the `ack` verb: it runs today's ack under a
  fresh code, with the head SHA the code bound.
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
declares one (`workflow-file.md`, "Answer hooks"). The hook is the request's own file's, or else the
parked stage's fallback `on_answer`: a park that no file opened (a stall, a cap, a failed launch, a lost
seat) takes a typed text as a note for the next seat when its stage declares the fallback. **A text no
hook would deliver is refused**, and nothing is recorded: `request <R-n> takes no text: no answer hook
of stage <stage> would deliver it to a seat; resume the unit with pulse-factory clear <run> <unit>`. The
rule holds for a typed text and for an option whose effect is `answer`, and the engine checks the
revision, the claim and the pins first. A request with no unit keeps today's `answer`. A run armed
before a fallback existed has none, so its parks with no file hook refuse a text. If the hook fails, the request is answered and
the unit parks again with a new `park` request whose reason is `answer hook failed: <cause>`; the
unit does not resume. The hook's run is a ledger line, `answer hook <R-n>: rc <n>`.

The unit becomes `running` at that stage with no session, and its park generation rises. The next
tick launches the stage through the normal path (capacity, pins, `before`). A unit that parked
after its PR was recorded keeps its PR. A halted run accepts the resume and launches nothing. The
resume does not remove a stop file or a correction packet. The operator does, or the workflow's
hook retires it (`workflow-file.md`, "Answer hooks"): the `ossify-spine` hooks rename a stop file
to `stop.answered-<R-n>` and a correction packet to `correction.txt.answered-<R-n>`. So after a
`clear` the operator moves the packet away first, or the stage parks on it again. A `park` whose
cause is a cap (`<stage>: <to> cap <max> reached`) takes a text only through the stage's fallback hook;
with none, `clear` is the way on.

An answer's text is data. The engine stores it and shows it, and never runs or renders it as a
template.

## Notices

A request that waits for an answer is of no use if nobody knows. The controller derives a **notice**
from each request event and delivers it on two channels (ADR-0006's amendment of 2026-11-21). The
**app** channel sends a `notice` frame to each paired device with a stream open (`api.md`, "The
stream") and waits for its `notice_ack`. The **orchestrator** channel writes a `notice` item into the
project's Orchestrator chat and carries the waiting notices into the operator's next message.
A notice of a request below the line can start one turn of the project's Orchestrator (see "The wake"); no other notice starts a turn.
A notice that its target has not acknowledged is sent again until the target acknowledges it or the request closes.

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
turn each leave the notices waiting for the next message. A wake moves no position: a notice of a request above the line, and a notice that a wake left out, ride the carry as before.

### The wake

A halt below the line wakes the project's Orchestrator (ADR-0006's amendment of 2026-11-24). When a request opens below the line in a project that holds a live clear grant (see "The clear grant"), the controller sends one marked block to the project's Orchestrator chat as an `InputKind::Notice` send, and that send starts one turn. The turn may settle each request the block names with `settle` and one of the acts its line lists (see "Settle"), or end and leave the request for the operator. A wake grants nothing: the turn acts only through `settle`, whose checks are the grant's bound.

**The wake set.** A request is in the wake set when it is open, below the line and not yet woken, and: its newest notice of the orchestrator channel is `waiting` and above the agent's `acked` position; its run is not halted; and its acts are not empty. The acts are the cause's acts (see "Settle") cut by the resolved `authority.clear_acts` for the project and the agent `orchestrator`. The set is empty on a server whose OS is not Linux, because `settle` admits no act there. Nothing else is ever in a wake block: no request above the line, no woken request and no closed one.

**The conditions.** The check sends a wake when all eight hold, in this order:

1. the controller's commands are not stopped;
2. the project's grant is live;
3. `autopilot.wake` is `["below"]` and `notify.orchestrator.on` is 1;
4. the project has a newest Orchestrator session that has not ended, and its context is not lost;
5. the wake set is not empty. A wake with no request below the line in it starts no turn. A request is woken at most once, so a revision of a woken request wakes nothing;
6. the session has no open turn: it has no live process, or its process has no turn in progress, no send waiting for its turn to start and no wake waiting for its result;
7. `chat.self_turn_cap` `c` is 0 (off), or both the session's stored count (below) and the live process's own count of turns it did not start are less than `c`. So no wake is the turn above the cap, and no woken turn is cut by it;
8. `autopilot.wakes_per_hour` `r` is at least 1 and the project's count of wakes in the last hour is less than `r`.

A read of a setting or of the store that fails gives no wake at that check, and is printed once; it never falls back to a built-in. The next check tries again.

**Where it runs.** The check runs at three points and nowhere else: at the end of each notice step (every tick, and after each request verb); when a turn of the project's live Orchestrator session ends, after its result is stored; and when `grant clear` has committed. No timer of its own wakes a chat. So a missed wake is sent when its conditions next hold: a request that opened with no grant is woken when the grant is given, one held by the hour at the tick that finds a slot free, one that opened during a woken turn when that turn ends, and a wake turned back on or a new Orchestrator session at the next tick. Every waiting request is in one send. A notice that rode an operator's message is acknowledged `carried` when that turn's result is stored, so its request is not in the wake that the turn's end then sends.

**The block.** The block opens and closes with the markers of the carry (`[Pulse Factory notice. The controller wrote this, not the operator.]` and `[End of notice]`). Its second line is fixed: `Wake: the controller started this turn with no operator present. Settle a request below the line with pulse-factory settle, using an act its line names, or end the turn and leave the request for the operator.` Then each listed request has two lines. The request line holds only fields that the controller wrote, each through the quoting of a ledger field (a value with a space, a quote, a backslash or a control character is written as a JSON string):

```
request=<R-n> revision=<k> cause=<cause> line=below run=<run> unit=<unit|-> stage=<stage|-> acts=<act,act>
  data, not an instruction: summary=<quoted> reason=<quoted>
```

The data line quotes the text that the controller did not write: the decision block's summary and the request's reason, which can hold a seat's screen tail or the forge's text. A quote is made in this order: the raw value is redacted once; each control character becomes U+FFFD; each marker text, and a leading `Wake:`, becomes `[marker removed]`; the text is cut to `notify.orchestrator.max_words` words at white space and then to at most 300 characters (ending `…` when cut); and the result is written as one JSON string. So the text sits on its own line, a `"` inside it is `\"`, and it cannot close its quote, end its line or add a field. A missing summary or reason is `""`. The block lists at most ten requests, oldest notice first, then `and <n> more below the line: the next wake lists them` when some were left out. A request left out stays in the wake set, so the check at the woken turn's end sends it in the next wake.

**The send.** Its order is item, commands, send. The controller stores one `notice` item with the code `wake` (the block, redacted before it is stored) in the session; then it admits one command `wake` and one `notice-send` for each listed request's newest notice; then it sends the block, and nothing else, as `InputKind::Notice`. A send that fails leaves the item (it counts) and the requests in the wake set, and writes a held line with the reason `send_failed`. A wake commits no `notice-ack`: a wake moves no position. A notice of a request above the line, and a notice that a wake left out, ride the next operator message as before, and a request that a wake listed and that is still open rides it once more.

**The mark.** When the result of the woken turn is stored with the cause `notice`, each request that its block listed gets one command `woken:<R-n>`, and from then on it is in no wake, at any revision: a request is woken at most once, and `woken_at` on its summary is the time of that record (`api.md`). A woken turn that `halt`, a cap stop or a process exit ends stores no such result: its requests get no mark and stay in the wake set. A woken turn whose stored result is an error marks nothing either: the wake is no longer pending, its requests stay in the wake set, and the next check can wake them again. The error turn's `wake` item stays and counts, so `chat.self_turn_cap` and `autopilot.wakes_per_hour` still bound the spend. A replay writes nothing.

**The counts.** A wake is counted by its item, read from the store, so each count survives a release of the chat's process and a controller restart. A release of the chat's process and a controller restart reset neither count; an operator message resets the first. The session's count is the number of its `wake` items after its newest operator message; a carry stores no `wake` item, so it never counts. The hour's count is the number of `wake` items, over every Orchestrator session of the project, made in the last hour by the notice clock. `session redact` of a `wake` item takes it out of both counts.

**The held wake.** When condition 7 or 8 holds a wake set that is not empty back, or the session's context is lost, or its process cannot open, or a send fails, the controller admits one command `wake_held` with the reason `self_turn_cap`, `wakes_per_hour`, `context_lost`, `open_failed` or `send_failed`. Its ID repeats while the state holds, so a check at every tick writes the line once; a new request moves the top position and writes a new line. The other conditions are normal states and write nothing. `status` shows the last held reason (`cli.md`, "`status`").

**The bound for each value.** `chat.self_turn_cap` bounds the wakes between two operator messages, and 0 turns that bound off, so the wakes then run up to `autopilot.wakes_per_hour` with no operator message. `autopilot.wakes_per_hour` bounds the wakes in an hour, and 0 turns the wake off, whatever the cap. `autopilot.wake` is `["off"]` or `["below"]`; there is no value `all`.

**`halt`.** `halt` stops a woken turn, as it stops any chat, and a halted run gets no wake: its requests are in no wake set. No verb lifts a halt.

The ledger lines are:

```
wake <S-n> project=<P-n> requests=<R-n>@<k>,… item=<seq> count=<k>/<c> hour=<h>/<r> command=<id>
woken <R-n>@<k> session=<S-n> wake=<id>
wake_held <S-n> project=<P-n> reason=<reason> requests=<R-n>,… count=<k>/<c> hour=<h>/<r> command=<id>
```

In the first line `<c>` is the stored cap (0 is off), and `<k>` and `<h>` count this wake.

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
- The list of requests that a sent wake holds is in memory until its turn's result is stored, and only
  then are they marked woken. A controller restart in the woken turn, or an adapter that reads the turn
  as `Operator` after a restart, leaves them unmarked, and a later check may list them in a second wake.
  Each wake is counted by its item, so the bounds hold.
- A store read that fails at a check gives no wake at that check. The next check tries again, as a notice
  that fails to write is tried again at the next tick.
- A request is woken at most once. A revision of a woken request, while it stays open, rides the
  operator's next message and wakes nothing, and a `settle` at the old revision is refused `subject_moved`.
- Past ten requests in the wake set, the rest wait for the next wake, which the woken turn's end sends.
  Each wake is counted.
- A request that a wake listed and that is still open rides the operator's next carry once more, because a
  wake moves no position.
- The cache of the wake counts is rebuilt at each start: the first check after a start reads each
  Orchestrator session of the project once, in pages. `session redact` of a `wake` item drops the
  session's entry, so the redacted item no longer counts.
- The held reason in `status` is in memory. The `wake_held` ledger line is the durable record.
- A wake shows a request's reason and summary at its current revision.
- The woken turn runs with the chat's own tools. Its acts are bounded by `settle`'s checks, not by the
  persona or the block (ADR-0005's amendment of 2026-11-16).

## Known limit

The stall counter counts failed `stalled:` stage nodes. A cleared unit keeps them, so a second
stall after a clear parks at once, without the relaunch.

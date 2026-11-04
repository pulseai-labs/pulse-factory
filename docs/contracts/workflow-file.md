# Workflow file contract (version 1)

A **workflow file** is one YAML document that says how a run works: which seats run, in which
stages, what ends a stage, and how finished units merge. `pf-workflow` reads it
(`crates/pf-workflow/src/file.rs`) and turns it into the value the engine runs (ADR-0004). The file
names no planning method. The method lives in the briefs and the commands that the file points at.

## Strict reading

The parser is strict. It refuses all of these:

- an unknown field, at any level;
- an unknown `version` (the error names the supported version, `1`);
- a duplicate key in one mapping;
- a missing required field;
- a placeholder that is not declared (see "Placeholders").

The version is read first, so a file of a future version gets the version error before any other
check. Each error names the line and the column of the text it blames, in this form:

```text
line 12, column 5: unknown field `modle` in role `lane`; the known fields are: runtime, model, rules
```

Line and column both count from 1.

## Top-level fields

| Field | Required | Meaning |
|---|---|---|
| `version` | yes | The integer `1`. |
| `name` | yes | The workflow's name. Use only `A-Z a-z 0-9 . _ -`. |
| `params` | no | The parameter names that `arm` must supply. They fill `{param.<k>}`. |
| `plugins` | no | A list of `{name, min_version}`. A pin below `min_version` is refused. |
| `roles` | yes | A mapping of role name to `{runtime, model, rules}`. |
| `settings` | no | `{model_sessions, cold_builds}`, each a whole number of 1 or more. |
| `plan_check` | no | A command that prints the item count as one whole number on stdout. `arm` runs it. |
| `roots` | no | The directories a seat's record may name as a working directory. |
| `unit_sources` | no | A list of `{dir, track, vars}`. Each names the track its units join. |
| `seeds` | no | A list of seed units (see below). |
| `end_when` | no | What must hold before the run ends (see "`end_when`"). |
| `tracks` | yes | A mapping of track name to `{stages, merge}`. |

### `roles`

Each role has three required fields:

- `runtime`: a free name such as `omp`. The engine knows no runtime. The adapter that serves this
  name launches the seat.
- `model`: the model name that the adapter passes on.
- `rules`: the path of the role's rule file, relative to the workflow file. See `role-rules.md`.

### `unit_sources`

`{dir, track, vars}`. `dir` is a template that may use `{run.dir}`, `{run.id}` and `{param.<k>}`.
`track` names a track of this file. `vars` lists the variable names that the records of this source
carry. A unit's track comes from the source that admitted it. The unit record format itself is in
`unit-record.md` and does not change.

### `seeds`

A **seed unit** is a unit the file declares. The engine admits it at the start of a run with no queue
record. Fields: `id`, `group`, `order`, `track`, `vars` (a mapping of name to text) and an optional
`repeat` or `start_when`. A seed carries one of them, never both; the file is refused otherwise.

`repeat` is `{when_units_done: <track>, until_file: <path>, max: <n>}`. All three are required. The
seed is admitted again under the ID `<id>.2`, then `<id>.3`, only when all of these hold:

1. the previous seed unit is `done`;
2. a unit on the named track came in after it;
3. every unit on the named track is `done`;
4. the `until_file` does not exist;
5. fewer than `max` seeds have run.

A `done` unit cannot park. So when the previous seed ended `done` and added no unit, or when `max` is
reached and the `until_file` is still absent, `pf_workflow::seeds` returns a halt reason, and the
controller calls `Engine::halt_for`.

`start_when` is `{file: <path>, units_done: <track>}`. Both are required. `file` is a template over
the run, and `units_done` names a track of this file. A seed with `start_when` is not admitted at
the start of the run. It is admitted once, on the first tick at which the rendered `file` exists and
every unit on `units_done` is `done`, and at least one such unit exists. A replayed admission is a
no-op, like any seed admission. A halted run admits nothing. The run's end check counts such a seed
as unfinished until its unit exists and is `done`. The `ossify-spine` package uses it to start the
spine close after the lane wrote `rounds.done` and every item is merged.

### `end_when`

`end_when` is `{when: [<done entries>], waiting: <text>, reason: <text>}`. All three are required,
and `when` holds one or more entries. An entry is a `file` or an `exit0` condition, as in a stage's
`done` list. `waiting` and `reason` are templates over the run.

A file with no `end_when` ends as before: when the seeds are finished and every unit is `done`, the
run ends with the reason `every item merged`. With `end_when`, the run ends with its `reason` only
when that check holds and every `when` entry holds too. Until then each tick notes `waiting` (one
line), and `status` shows the note. An entry that cannot be read does not hold, and the tick notes why.

### `tracks`

A track is `{stages, merge}`. `stages` is a list of one or more stages and runs in the order given.
`merge` is optional. A unit on a track with no `merge` never waits for a merge.

`merge` takes these fields:

| Field | Required | Meaning |
|---|---|---|
| `rule` | yes | `green_checks_in_declared_order` or `claim_ack` (see below). Any other value is refused, and the error names both. |
| `repo` | yes | The repository a unit's PR must be in, a template. The engine renders it for each unit and parks a unit whose PR names another repo. It names the repo on the forge, such as `{param.forge_repo}`, which is the name the forge puts in a PR URL. A local path for `git -C` is a different parameter. |
| `base` | yes | The branch the unit's PR must merge into, a template. The engine renders it for each unit and parks a unit whose PR has another base. |
| `head_branch` | no | A template: the branch the unit's PR must come from. Only the rule `claim_ack` takes it. A PR from another branch parks the unit with `merge_failed`. |
| `pr` | yes | A command that prints the unit's PR URL. |
| `head_check` | no | `true` or `false`; default `false`. When `true`, the engine's merge step compares the PR head with the last recorded push and parks the unit when there is none. |
| `after_merge` | no | A list of commands that run after the merge, on a unit already `done`. |

**The rule `claim_ack`.** The merge of a unit on such a track waits for the claim holder's ack
(ADR-0005). When the unit is next in the declared order and every stage is done, the merge step
reads the PR and checks the repo, the base and, when `head_branch` is set, the head branch. Pending
checks wait. Red checks, a conflict or a mismatch park the unit with `merge_failed`, as under the
other rule. A PR that is green and clean opens one `merge_ack` request for the unit, in one commit
with the unit's park at the stage `merge` (`requests.md`, "The body of a `merge_ack` request"). The
merge happens only on `pulse-factory ack`, with the head SHA of the request pinned. `head_check` is
allowed with `claim_ack`, but the acked SHA replaces it, and the `ossify-spine` package sets it to
`false` so that an operator's fix commit on the PR is normal. Each ack, refusal and merge writes a
ledger line.

## Stages

Each stage of a track has these fields:

| Field | Required | Meaning |
|---|---|---|
| `name` | yes | The stage name. Unique within the track. |
| `role` | yes | A role defined in `roles`. |
| `brief` | yes | A brief path, or a list of brief paths, relative to the workflow file. |
| `message` | yes | The first message to the seat, a template. |
| `cwd` | yes | The seat's working directory, a template. |
| `slots` | no | A list of `session` and `build`. Default `[session]`. |
| `before` | no | Commands that run before each launch. A failure parks the unit. |
| `after_first_turn` | no | Commands that run once, after the seat's first turn ended. |
| `stop_file` | yes | The file whose text, when present, parks the unit. A path, or `{file, on_answer}` (see "Answer hooks"). |
| `stall_minutes` | yes | Minutes without output before the seat counts as stalled. 1 or more. |
| `first_turn_seconds` | yes | Seconds the seat has to finish a first turn. 1 or more. |
| `skills` | no | The skill names the seat may use. |
| `settings` | no | A mapping with `env.<NAME>`, `push_dir`, `push_branch`, `report_dir`, `worktree`, `own_branch` and `decisions_file` keys. |
| `done` | yes | One or more conditions. All of them must hold. |
| `park_on` | no | A list of `{file, reason, on_answer}`. `on_answer` is optional. |
| `outcomes` | no | A list of `{when, to, max}`: where the unit goes when the stage ends in a way other than plain done (see "`outcomes`"). |
| `entered_by` | no | `outcome` is the only value in version 1. The stage runs only when an outcome sends the unit to it (see "`outcomes`"). It needs at least one `outcomes` entry. |

`before` holds the checks that cap launches. `after_first_turn` holds the count of dispatches. The
count runs after a first turn, not after a launch, so a launch that sits at a trust prompt does not
count.

`settings` keys other than `env.<NAME>` (`NAME` is `A-Z`, `0-9` and `_`), `push_dir`, `push_branch`,
`report_dir`, `worktree`, `own_branch` and `decisions_file` are refused here. Each of the six is a
template, rendered for the unit like the other settings. `report_dir` is the directory that holds the
seat's report. `worktree` and `own_branch` are the seat's worktree and its own branch, and a stage
whose seat has none leaves them out. `decisions_file` is the file where the seat writes its decisions,
one per line. `launch-settings.md` lists every key that reaches the adapter and how
the engine builds them.

### `done` conditions

A condition is a mapping with exactly one key:

- `file: <path>`: the file exists.
- `exit0: <command>`: the command exits 0. A non-zero exit means "not yet" and does not park.
- `pr: {argv, test}`: the `argv` command prints a PR URL. `test` is `url`, the only test in version 1.

### `outcomes`

A stage can send its unit back to an earlier stage, or on to a later one, up to a cap. This is how a
reviewer's block becomes a fix and a new review. The loop is a construct of the workflow file. The
engine knows no review.

`outcomes` is a list of one or more entries. Each entry has these fields, all required:

| Field | Meaning |
|---|---|
| `when` | A list of one or more conditions, in the form of `done` conditions (`file`, `exit0`, `pr`). All of them must hold. |
| `to` | A stage of the same track. It may be the stage itself, an earlier stage or a later one. |
| `max` | A whole number of 1 or more. The entry sends the unit at most this many times. |

The engine tries the entries when every `done` condition of the stage holds and the stage's session is
over (its turn ended, or it ended). It tries them in order, and the first entry whose `when`
conditions all hold sends the unit to `to`:

- The unit's stages done becomes the place of `to`, so the unit runs `to` next, through the normal
  launch path. The stage that sent the unit ends `done`.
- The engine keeps one **visit counter** per entry with the unit (`unit-record.md`). It adds 1 each
  time the entry sends the unit, and the ledger line reads
  `stage <stage> done (<signal>); outcome <n> sends the unit to <to> (visit <v> of <max>)`.
- When the counter would go above `max`, the entry does not send the unit. The unit parks with the
  reason `<stage>: <to> cap <max> reached`, and the park opens a request of kind `park`
  (`requests.md`). The stage ends `done` and the unit's place does not change.
- When no entry holds, the stage is done as it was before: the unit goes to the next stage, or to its
  merge. A `when` condition that is a command is `exit0` like a `done` condition: a non-zero exit means
  "this entry does not hold" and does not park.

Every `clear` resets all of the unit's visit counters to 0. A clear is the operator's grant of a new
budget. An `answer` that resumes the unit does not reset them. A restart of the stage by a stall does
not reset them either.

`entered_by: outcome` marks a stage that only an outcome enters. When the unit reaches it in order, the
engine passes it over: the stage never runs, and the merge rule counts it as done. It runs only when
an outcome sends the unit there. Such a stage must hold at least one `outcomes` entry, or the file is
refused.

Load-time checks, each refused with the position of the entry: `to` names a stage of the same track
(an unknown stage, or a stage of another track, is refused); `max` is present and at least 1;
`when` is present and not empty; `entered_by` has the value `outcome` and the stage holds
`outcomes`. A file with no `outcomes` and no `entered_by` loads and runs as before: version 1 does not
change for it.

Each time an entry sends a unit it launches its `to` stage once. `arm` counts that, so its most
launches adds every entry's `max` (`cli.md`).

### `park_on`

Each entry is `{file, reason}`. When the file exists, the unit parks with the reason. The stop file
parks with its own text and wins over every other condition.

The park opens a request (`requests.md`). Its kind comes from the file's name: `gaps.json` opens a
`gaps` request, `correction.txt` a `red_gate` request, and any other name a `park` request. The
stop file opens a `stop` request. A turn that ends with no `done`
condition, no `park_on` file and no stop file parks with "seat ended with no done signal".

### Answer hooks

A `park_on` entry and the stage's `stop_file` take an optional `on_answer`: a list of commands, in the
form of "Commands". `stop_file` is then a mapping `{file, on_answer}`; a plain path declares no hook.
`on_answer` is refused anywhere else.

```yaml
stop_file:
  file: "{run.dir}/out/{unit.id}/stop"
  on_answer:
    - [sh, "{run.package}/scripts/rule.sh", "{run.dir}/out/{unit.id}/rulings.md", ...]
```

- The hook runs when an `answer` resumes the unit, before the unit becomes `running`. The hook is
  the one of the file that opened the request: the stop file's for a `stop` request; for any other
  kind, the first `park_on` entry whose file name gives that kind (for the kind `park` the entry's
  reason must also be the request's reason). A `clear` runs no hook.
- The commands run in order and the first failure stops the hook. Each has the usual bounds: 60
  seconds by default and at most 600, no retry. The answer placeholders hold controller-made values
  only, and each reaches the argv as one word. Nothing renders a value twice.
- A command may run a file of the package as `{run.package}/<path>`. The file must be one the run
  pins: the package pins every plain file of its `scripts/` directory, with the workflow file, the
  briefs and the rule files. A package whose hook names another file is refused at load.
- The engine is asked first whether it would admit the answer. A refusal (a stale revision, a
  fenced claim, an empty text, a changed pin) runs no hook.
- Rc 0 of every command resumes the unit. Any failure (a non-zero exit, a timeout, a command that
  cannot start or render) leaves the request answered and the unit `parked`, with the reason
  `answer hook failed: <cause>` and a new `park` request. Both happen in one commit.
- Each run writes a ledger line, `answer hook <R-n>: rc <n>`, with `rc none` for a hook that ended
  with no exit code, and the cause after it for a failure.
- Hooks run in the controller, as the user who runs it. The workflow file is pinned, so a hook cannot
  change after `arm`.

## Commands

A command is data, never a shell string. Write it in one of two forms:

```yaml
- [mkdir, -p, "{run.dir}/out/{unit.id}"]
- argv: [make, check]
  timeout_seconds: 300
```

Rules:

- The first item is the program. It must not come from a unit record.
- Each placeholder renders into exactly one argv item. A value with a space or a `;` cannot split an
  item or add one.
- A rendered value from a unit record that starts with `-` parks the unit. It cannot act as an option.
- The default timeout is 60 seconds. `timeout_seconds` may raise it to at most 600.
- Stdout and stderr are each kept to 64 KiB. The runner drains the rest.
- A non-zero exit, a timeout or a failure to start parks the unit. Nothing retries a command.
- A timed-out process is killed.
- `plan_check` has no unit. Its failure refuses `arm`.
- `after_merge` runs on a unit already `done`. Its failure calls `Engine::halt_for` with the reason.

## Placeholders

A placeholder is `{name}`. The names are a closed set:

| Placeholder | Value | Where it is allowed |
|---|---|---|
| `{unit.id}` | The unit's ID. | Where a unit exists. |
| `{unit.vars.<k>}` | A variable of the unit's record. | Where a unit exists, and only when a source or seed of the unit's track declares `<k>`. |
| `{run.id}` | The run's ID. | Where a run exists. |
| `{run.dir}` | The run's directory. | Where a run exists. |
| `{run.package}` | The run's own copy of the workflow package, `<run dir>/package`. | Where a run exists. |
| `{param.<k>}` | A parameter. | Everywhere, and only when `<k>` is in `params`. |
| `{answer.file}` | A file the controller writes (mode 0600) that holds the answer text. | An `on_answer` command only. |
| `{answer.request}` | The request ID, `R-<n>`. | An `on_answer` command only. |
| `{answer.revision}` | The revision the answer is bound to. | An `on_answer` command only. |
| `{answer.generation}` | The claim generation that answered. | An `on_answer` command only. |
| `{request.body_file}` | A file the controller writes (mode 0600) that holds the request body as JSON. | An `on_answer` command only. |

A placeholder with an undeclared name is an error at parse time. A record that lacks a declared
variable parks the unit at run time.

**Literal braces.** A `{` starts a placeholder only when the text up to the next `}` holds
only `A-Z a-z 0-9 _ . -` and is not empty. Any other brace text is literal. So a JSON object or a
`jq` program such as `{"a": 1}` or `select(.x == {})` needs no escape. A name-like word in braces,
such as `{unit.idd}`, is read as a placeholder and refused as unknown.

## Files that the workflow names

- `roles.<role>.rules` and each stage `brief` are paths relative to the workflow file.
- The package pins the workflow file, its briefs and its rule files into the run's manifest (ADR-0007).
- A workflow package holds no host path. Every path is a placeholder that `arm` fills.

## Size limits

- A queue record is at most 64 KiB (`unit-record.md`).
- A command's output is at most 64 KiB per stream.
- A command runs at most 600 seconds.

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
`repeat`.

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

### `tracks`

A track is `{stages, merge}`. `stages` is a list of one or more stages and runs in the order given.
`merge` is optional. A unit on a track with no `merge` never waits for a merge.

`merge` takes these fields:

| Field | Required | Meaning |
|---|---|---|
| `rule` | yes | `green_checks_in_declared_order` is the only rule in version 1. |
| `repo` | yes | The repository, a template. |
| `base` | yes | The branch the unit merges into, a template. |
| `pr` | yes | A command that prints the unit's PR URL. |
| `head_check` | no | `true` or `false`; default `false`. When `true`, the merge step compares the PR head with the last recorded push. |
| `after_merge` | no | A list of commands that run after the merge, on a unit already `done`. |

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
| `stop_file` | yes | The file whose text, when present, parks the unit. |
| `stall_minutes` | yes | Minutes without output before the seat counts as stalled. 1 or more. |
| `first_turn_seconds` | yes | Seconds the seat has to finish a first turn. 1 or more. |
| `skills` | no | The skill names the seat may use. |
| `settings` | no | A mapping with `env.<NAME>`, `push_dir` and `push_branch` keys. |
| `done` | yes | One or more conditions. All of them must hold. |
| `park_on` | no | A list of `{file, reason}`. |

`before` holds the checks that cap launches. `after_first_turn` holds the count of dispatches. The
count runs after a first turn, not after a launch, so a launch that sits at a trust prompt does not
count.

`settings` keys other than `env.<NAME>` (`NAME` is `A-Z`, `0-9` and `_`), `push_dir` and
`push_branch` are refused here. `launch-settings.md` lists every key that reaches the adapter and how
the engine builds them.

### `done` conditions

A condition is a mapping with exactly one key:

- `file: <path>`: the file exists.
- `exit0: <command>`: the command exits 0. A non-zero exit means "not yet" and does not park.
- `pr: {argv, test}`: the `argv` command prints a PR URL. `test` is `url`, the only test in version 1.

### `park_on`

Each entry is `{file, reason}`. When the file exists, the unit parks with the reason. The stop file
parks with its own text and wins over every other condition. A turn that ends with no `done`
condition, no `park_on` file and no stop file parks with "seat ended with no done signal".

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
| `{param.<k>}` | A parameter. | Everywhere, and only when `<k>` is in `params`. |

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

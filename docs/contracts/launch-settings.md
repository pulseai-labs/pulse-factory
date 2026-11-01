# Launch settings contract

A **launch plan** (`pf_engine::LaunchPlan`) is what the engine hands to an agent adapter to start one
stage of one unit. Besides the run, unit, stage, role, model and attempt, it carries `runtime` and a
map of **settings**. Adapters (`r0.s1.w3`) read the settings. This file lists the keys and says how
the engine builds the values.

## `runtime`

`LaunchPlan.runtime` is the role's `runtime` from the workflow file, for example `omp`. It is a free
name. The engine does not know any runtime. The adapter that serves the name launches the seat. A
`StageDef` made without a runtime gets the name `default`.

## Keys

| Key | Value | Notes |
|---|---|---|
| `cwd` | The seat's working directory. | See "Directory checks". |
| `message` | The first message to the seat. | |
| `env.<NAME>` | One environment variable for the seat's process. | `NAME` uses `A-Z`, `0-9` and `_`. |
| `brief` | A comma-joined list of brief paths. | The paths are relative to the workflow package. |
| `plugins` | A comma-joined list of pinned plugin directories. | The adapter or controller adds it from the run's pins. It is not written in the file. |
| `skills` | A comma-joined list of skill names. | |
| `first_turn_seconds` | A whole number, as text. | The time the seat has for a first turn. |
| `stall_minutes` | A whole number, as text. | The time without output before the seat counts as stalled. |
| `push_dir` | The directory where a push is measured. | For a push whose target the rules cannot read. See "Directory checks". |
| `push_branch` | The branch where a push is measured. | |
| `report_dir` | The seat's report directory. | Stage setting. The rules let a role with `write_own_report_and_run_files` write here. |
| `worktree` | The seat's worktree. | Stage setting. Empty for a seat with none, such as a lane. |
| `own_branch` | The branch of the seat's own unit. | Stage setting. May be empty. |
| `decisions_file` | The file where the seat writes its decisions, one per line. | Stage setting. The controller reads it and records each line (see "The controller adds"). |
| `repo` | The forge repository, `owner/name` or the bare repo's path. | The controller adds it from the run's `forge_repo`. |
| `remote` | The git remote of pushes, `origin`. | The controller adds it. |
| `default_branch` | The default branch of the forge repository. | The controller adds it from the forge, at `arm`. |
| `forge_api_hosts` | A comma-joined list of the forge's API hosts. | The controller adds it from the forge: `api.github.com`, or `forge.invalid` for the bare forge. Never empty. |
| `rules_file` | The role's rule file in the run's own copy of the package. | The controller adds it. |
| `scratch_dir` | The seat's scratch directory, `<run dir>/out/<unit>/scratch`. | The controller adds it and makes it. |
| `run_dir` | The run's directory. | The controller adds it. |
| `target_dir` | The build target of the repository. | The controller adds it from `cargo metadata` at `arm`. Empty when that failed. |
| `own_tag` | The tag of the seat's own unit. | Empty in this spine. |
| `env.PF_HOOK_CMD` | The shell command that judges a tool call: the binary's path, quoted, then ` hook`. | The controller adds it. |
| `env.PF_SESSION_KEY` | A fresh key for this launch. | The controller adds it. It names the seat to the hook. It never reaches a log, a reason or `status`. |
| `env.PF_STATE_DIR` | The controller's state directory. | The controller adds it, so the hook finds the socket. |
| `env.PI_EDIT_VARIANT` | `replace`. | The OMP adapter adds it, so OMP's `edit` call carries a path. |

A stage's `settings` map in the workflow file may hold `env.<NAME>`, `push_dir`, `push_branch`,
`report_dir`, `worktree`, `own_branch` and `decisions_file`.
The other keys come from the stage's own fields (`cwd`, `message`, `brief`, `skills`,
`first_turn_seconds`, `stall_minutes`). The engine receives all of them as one map of key to template
text in `StageDef.settings`.

## Rendering

`start_stage` renders every value as a template. The placeholders are `{unit.id}`,
`{unit.vars.<k>}`, `{run.id}`, `{run.dir}` and `{param.<k>}`. The run's parameters come from
`EngineConfig.params` and the run directory from `EngineConfig.run_dir`. The rules are in
`workflow-file.md`, section "Placeholders".

A value that cannot render parks the unit before the adapter is called. The reasons are:

- the unit's record lacks a variable that the template names;
- the run lacks a parameter that the template names.

The park reason starts with `launch refused:` and names the setting. No launch reaches the adapter.

## Directory checks

A value for `cwd` or `push_dir` that **comes from a unit record** (it holds `{unit.id}` or
`{unit.vars.<k>}`) must pass all of these checks:

1. It is an absolute path.
2. It exists.
3. It is a directory.
4. After the engine makes the path canonical (it follows symlinks), it lies under one of the
   workflow's `roots`. Each root is rendered with the run and its parameters, never with a unit, and
   is made canonical too.

A workflow with no `roots` refuses every such value. A failed check parks the unit with a reason that
names the setting, and no launch reaches the adapter. This holds for a relative path, a missing path,
a file, a path outside the roots and a symlink that leads outside the roots.

A directory that comes only from the workflow file and the run's parameters is not checked. The
operator wrote it.

## The controller adds

`pulse-factory`'s `SeatPort` wraps the run's adapters. Its `launch` and `hand_off` add `plugins`,
`repo`, `remote`, `default_branch`, `forge_api_hosts`, `rules_file`, `scratch_dir`, `run_dir`,
`target_dir`, `env.PF_HOOK_CMD`, `env.PF_SESSION_KEY` and `env.PF_STATE_DIR` to the plan. It refuses
the launch (`AgentError::Refused`, no process) when `repo`, `remote`, `default_branch`,
`forge_api_hosts`, `rules_file`, `scratch_dir`, `run_dir` or `report_dir` is empty, and when a role
that may push its own branch has no `push_dir` or `push_branch`. Only `worktree`, `target_dir`,
`own_branch` and `own_tag` may be empty, because a lane has none. An empty path matches nothing.

The controller reads the stage's `decisions_file` at each tick, as whole lines up to the last
newline, and records each non-blank line in the ledger with the ID `decision:<unit>:<line number>`.
A seat that edits an earlier line gets a note in `status`, and the ledger keeps the first text.

## Who writes what

- The workflow file declares the templates. `pf-workflow` converts the file to a `Workflow`.
- The engine renders and checks them in `start_stage`.
- The controller (`SeatPort`) adds the run's facts, a fresh session key and the hook command.
- The adapter reads the finished map and `runtime`. It never sees a template.
- Seat output is untrusted (ADR-0005). The settings hold only text from the workflow file, the run's
  parameters and the unit's record. A record value never runs as a command.

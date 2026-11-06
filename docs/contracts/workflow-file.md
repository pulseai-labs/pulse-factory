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
| `base_push` | no | `{dir, branch}`, both templates: a branch the controller puts on `origin` before the stage launches, when `origin` lacks it (see "`base_push`"). |

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

### `base_push`

A stage can name a branch that must be on `origin` before the stage launches. The seat of the stage
may not own that branch, so its rules deny it the push. The controller does the push, in its launch
step (ADR-0001). The key is a construct of the workflow file. The engine and the controller know no
method (ADR-0002).

`base_push` is a mapping with two fields, both required and both templates in the stage's scope (the
same placeholders as `cwd`):

| Field | Meaning |
|---|---|
| `dir` | The local repository that holds the branch. Its `origin` receives the push. |
| `branch` | The branch name, without `refs/heads/`. |

Any other field in the mapping is refused with its position, and so is an empty value or a
placeholder the file does not declare. The file version stays 1 (ADR-0004): a file without the key
loads and runs as before, and unknown keys are still refused. A run armed before a package gained
the key keeps its pinned package copy (ADR-0007), so it never gains a base push.

**What the controller trusts.** `dir` is a seat's repository, and its `.git/config` is shared by
every worktree of that repository, so any seat of the run can write it. A local `core.hooksPath`,
a `pre-push` hook, `remote.origin.receivepack`, `core.sshCommand`, `credential.helper` or
`url.<x>.insteadOf` would otherwise run a command or change a URL under the controller. So the
controller reads only names and objects from `dir`. It checks, asks and pushes from a bare
repository of its own. The global and the system git config are the operator's, and the
controller's repository reads them: the operator's credentials live there.

**Two kinds of git call.** Both run an argv with no shell, with `GIT_TERMINAL_PROMPT=0` and
`LC_ALL=C`, with the 60 s command timeout, and with `-c core.hooksPath=/dev/null -c
core.fsmonitor=false` before the subcommand.

| Call | Runs as | Used for |
|---|---|---|
| `seat_read` | `git -C <dir> …` | Calls that read names and run no transport. |
| `own` | `git --git-dir=<the controller's repository> …` | Every call that reaches the forge. |

No transport call (`ls-remote`, `push`) runs in `dir`.

**The controller's repository.** For each launch the controller makes
`<run dir>/controller/base-push/<unit>.git`. It removes an old one first, runs
`git init --bare --quiet --template=`, and writes `<common dir>/objects` as the one line of
`objects/info/alternates`. `<common dir>` is the common git directory of `dir`. The repository
holds no objects and no hooks: git reads the seat's objects through the alternate and copies none.
It takes a few KiB. The controller removes it again when the check returns, on every path. A
failure to remove it is not a park reason.

**The order of the steps.** For a stage with `base_push`, the launch step does these after the
pin check and before the stage's `before` commands:

1. It renders `dir` and `branch` for the unit.
2. It refuses the branch when it is empty, starts with `-`, fails `git check-ref-format --branch`,
   or is the run's default branch. It refuses an empty `dir`.
3. It reads the common git directory with `seat_read`:
   `rev-parse --path-format=absolute --git-common-dir`. It refuses when the read fails or the
   directory has no `objects` directory. A `dir` that is not a repository stops here.
4. It makes the controller's repository (above).
5. It names each seat-written setting it ignores. `seat_read` runs
   `config -z --show-scope --get-regexp <keys>`. The keys are `core.hookspath`, `core.fsmonitor`,
   `core.sshcommand`, `core.gitproxy`, `core.askpass`, `core.alternaterefscommand`, `credential.*`, `url.*.insteadof`,
   `url.*.pushinsteadof`, `remote.origin.receivepack`, `remote.origin.uploadpack`,
   `remote.origin.vcs`, `remote.origin.proxy`, `http.*`, `protocol.*`, `include.path` and
   `includeif.*`. The controller keeps the entries of the `local` and `worktree` scopes, once per
   key. Exit 1 (no key) is none. Any other failure is a refusal. Each key gets one `note:` line
   (below). The controller ignores a setting because no transport runs in `dir`, not because the
   list names it: the list only chooses what the ledger names.
6. It refuses the `dir` when its `origin` is not the run's forge. It reads every URL of
   `git -C <dir> remote get-url --all origin` (the fetch URL) and of
   `git -C <dir> remote get-url --all --push origin` (the push URLs, after
   `remote.origin.pushurl` and `pushInsteadOf`). On GitHub the `owner/name` of each must be the
   run's forge repository (the case is not compared). In the paper env the canonical path of each
   must be the canonical path of `forge.git`. When either read fails (for example, `dir` has no
   remote named `origin`, which git would read as a path), it refuses and names the cause. It
   never skips the check.
7. It sets the first push URL as the remote `forge` of the controller's repository, and reads
   `remote get-url --all forge` and `--all --push forge` there. These reads apply only the global
   and system config. Each URL must again be the run's forge. So the URL the controller checked
   is the URL git uses. No call reads `origin` of `dir` after this, so a seat that changes the
   config of `dir` during the check changes nothing the push uses.
   The controller reads these URLs a second time right before the push. Git reads the global and
   system config at each call, and a seat's shell can write the operator's global config, so the
   second read narrows the time in which a `pushurl` or `pushInsteadOf` written there could move
   the push. It does not close that time: a shell write is the limit of ADR-0005.
8. It runs `ls-remote forge refs/heads/<branch>` with `own`, without `--exit-code`.
   `ls-remote` matches a pattern by the tail of a ref, so it also prints
   `refs/tags/refs/heads/<branch>`. The controller counts only a line whose second field is
   exactly `refs/heads/<branch>`.
9. **Exit 0 with that line:** `origin` holds the branch, at any SHA. Nothing is pushed, there is
   no push line, and the launch goes on.
10. **Exit 0 without that line:** `origin` lacks the branch. The controller reads the local SHA
    with `seat_read`: `rev-parse --verify refs/heads/<branch>`. It then pushes that SHA to
    `refs/heads/<branch>` and to nothing else, with no force flag and no `+`. It records the
    result with the unit's push record, and a successful push goes on to `before` and the launch
    in the same tick.
11. **Any other exit code, a timeout or a signal:** the controller pushes nothing.

**How the push names its ref.** The push is `<sha>:refs/heads/<branch>`. git reads a destination
as an abbreviation, so on a forge that holds only `refs/tags/refs/heads/<branch>` the plain form
aims at that tag, and git reports the tag as up to date or as already existing. The controller
therefore makes `refs/heads/<branch>` at `<sha>` in its own repository with `update-ref`. That
repository is made fresh for the call and holds no other branch. It then lists the refs of that
repository and pushes nothing when one besides `refs/heads/<branch>` is there. Then it pushes the
glob `refs/heads/<branch>*:refs/heads/<branch>*`. The remote ref of a glob is matched by its full
name. The glob sends each ref of the repository whose name starts with `refs/heads/<branch>`, so
the check of the refs is what makes the push send the one. A seat reaches that repository only by
a shell write, within the time of one call: the limit of ADR-0005.

**The ledger lines.** Every push is an admitted `Push` command, so it writes
`push: repo=<forge> branch=<branch> sha=<sha> outcome=pushed` or `outcome=failed: <cause>`. The
command ID is `push:base-push:<unit>:<park generation>:<attempt>`. The attempt number counts the
base pushes the unit already has in that park generation, so each push attempt has its own ID and
its own line, also a second push in one generation. A repeat of one ID is a replay and writes no
second line. A `clear` gives the unit a new park generation, so the attempt count starts again.
A refusal and a failed read write no push line, because nothing was tried. A missing local branch
writes `outcome=failed: no local branch <branch>` with an empty SHA, and tries no push.

Every ignored setting is an admitted `Note` command (step 5). It writes
`note: unit=<unit> base push ignored git config <key> (<scope>) of <dir>`. The line names the
key and never the value, because a value can hold a command or a URL with a secret. The event ID
is `base-push-config:<unit>:<park generation>:<key>` and the command ID is `note:<event ID>`. A
repeat of one ID with the same text is a replay, so a second launch in the same park generation
writes no second line, and a new park generation names the settings again. A note that fails to
write is not a park reason: the push goes on, and the tick notes the failure.

**The park reasons.** Each parks the unit and opens a `park` request (`requests.md`). A reason that
comes from git ends with the first line of git's stderr. The controller redacts the whole line,
cuts it to 160 characters and cleans it to one line, so an authentication failure names itself
and a token that crosses the cut is never shown in part.

| Case | Reason |
|---|---|
| A template does not render | `cannot render the <field> of base_push of stage <stage> for unit <unit>: <why>` |
| The branch is refused | `base push of branch <branch> refused: <rule>` |
| The `dir` is empty | `base push of branch <branch> refused: the dir is empty` |
| The common directory cannot be read | `base push of branch <branch> refused: cannot read the common git directory of <dir>: <cause>` |
| The controller's repository cannot be made | `base push of branch <branch> refused: cannot make the controller's repository for <dir>: <cause>` |
| The git config cannot be read | `base push of branch <branch> refused: cannot read the git config of <dir>: <cause>` |
| `origin` cannot be read | `base push of branch <branch> refused: cannot read the <fetch or push> URL of origin of <dir>: <cause>` |
| The `origin` is another forge | `base push of branch <branch> refused: origin of <dir> is <url>, which is not the run's forge <forge>`, or `origin of <dir> pushes to <url>, which …` for a push URL |
| The controller's remote cannot be set or read | `base push of branch <branch> refused: cannot set the controller's remote for <dir>: <cause>`, or `cannot read the <fetch or push> URL of the controller's remote for <dir>: <cause>` |
| The controller's remote is another forge | `base push of branch <branch> refused: the controller's remote for <dir> is <url>, which is not the run's forge <forge>` |
| `ls-remote` fails | `cannot read <branch> on origin of <dir> (ls-remote rc <n>)`, or `(ls-remote timed out)` |
| The ledger cannot be read | `base push of <branch> to origin not tried: cannot read the ledger: <why>` |
| The push fails | `base push of <branch> to origin failed: <cause>` |
| The push was done, but its line was not written | `base push of <branch> to origin was done, but its ledger line was not written: <why>` |

**The git calls and their limit.** Every call has the 60 s command timeout. A tick that runs the
whole check makes up to 13 calls, in turn:

- 11 local calls, which read or write local files: `check-ref-format`, the common-directory read,
  `init`, the config read, the two `get-url` reads in `dir`, the `config` that sets the remote,
  the two `get-url` reads in the controller's repository, `rev-parse --verify` and `update-ref`.
- 2 calls that reach the network: `ls-remote` and `push`. They are the only two.

The worst case is 13 calls of 60 s each, 780 s, and 120 s of that is the network. A timeout of
`push` is recorded as a failed push (see the known limit below).

**What it never does.**

- A halted run pushes nothing: the launch step returns before the check.
- It never overwrites. A branch that `origin` holds at any SHA is exit 0, and nothing is pushed.
- It never pushes the default branch, and never to another forge than the run's.
- It never runs a hook, a command or a URL rewrite that a seat wrote into the config of `dir`.
- It does not change `head_check`. The base push is recorded under the unit before the stage's seat
  launches, so the seat's own push stays the unit's newest push, and `head_check` reads that one. If
  `origin` loses the branch after the seat pushed, the next launch of the stage pushes the base
  again, and `head_check` then refuses the merge and parks the unit: it fails closed.
- It changes no seat's rule set or own branch.

**Credentials.** The controller pushes with the git credentials of the user unit, the same ones the
seats it launches push with (`docs/install.md`). They come from the global and system git config,
which the operator owns. A `credential.helper` in the config of `dir` is not used. No new secret
is read, and none reaches a ledger line or a reason.

**Known limit (the local ref).** The controller pushes the SHA that the seat's local branch
holds. A seat that repoints the branch (`git update-ref`, `git branch -f`) before the push makes
the controller push that tip. The item PR's review and the spine PR's ack still read the result
(ADR-0005).

**Known limit (the first push URL).** When `origin` has several push URLs, each must be the run's
forge, and the controller pushes to the first.

**Known limit (timeout of `push`).** A push the 60 s limit kills after the remote took the ref is
recorded as `outcome=failed` and parks the unit. After a `clear`, `ls-remote` returns exit 0, so
the launch goes on and the ledger keeps the `failed` line over a pushed branch.

**Known limit (ADR-0007).** The push has no record before it runs. If the controller dies between
the push and its record, the push has no ledger line, and after a restart `ls-remote` returns
exit 0, so nothing repairs the line.

`arm` names each base push before the run starts (`cli.md`).

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

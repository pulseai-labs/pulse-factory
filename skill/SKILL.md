---
name: pulse-factory
description: Arm a Pulse Factory run for a planned spine, read its status, and halt it. Use when the operator asks to run a spine unattended, to start a run on Pulse Factory, to see what a run costs, or to stop one.
---

# Pulse Factory

Pulse Factory runs the seats of a planned spine with no model session watching. You arm a run once.
A controller (a systemd user service) then drives the seats until each item PR has merged into the
spine branch. The CLI contract is `docs/contracts/cli.md`. This skill covers the verbs: `arm`,
`status`, `halt`, `hook` and `controller`, and the verbs of requests and the claim: `requests`,
`show`, `claim`, `answer` and `clear`.

## Arm a run

1. Work out the `--param` values with the planning method's own tool. The binary never calls that
   tool. Run the commands in a code block like this one, and read their output:

   ```bash
   # The spine branch, the spine directory, the repo and the method's root. Adapt the names to
   # your method; the workflow file lists the parameters it declares.
   method_root="$(pwd)"
   spine=r0.s1
   release="$(oss get ".spines[] | select(.id == \"$spine\") | .release")"
   name="$(oss get ".spines[] | select(.id == \"$spine\") | .name")"
   slug="$(printf '%s' "$name" | tr '[:upper:]' '[:lower:]' | sed 's/[^a-z0-9]\{1,\}/-/g; s/^-//; s/-$//')"
   spine_branch="$(oss branch_name "$spine" "$slug")"
   spine_dir="$method_root/$(oss spine_dir "$release" "$spine" "$slug")"
   plugin_root="$HOME/.claude/plugins/cache"
   ```

2. Show the plan first. A dry run reads and checks. It starts nothing and records nothing:

   ```bash
   pulse-factory arm "$spine" --workflow ossify-spine --ledger "$spine_dir/autopilot-ledger.md" \
     --plugin-root "$plugin_root" \
     --param spine_branch="$spine_branch" --param spine_dir="$spine_dir" \
     --param repo="$PWD" --param method_root="$method_root" \
     --param worktree_root="$PWD/.worktrees" --param state_file="$method_root/.state.json" \
     --param oss="$(command -v oss)" \
     --dry-run
   ```

   Every `--param` value must be an absolute path or plain text. The controller runs in another
   directory, so a value that is `.` or starts with `./` or `../` is refused.

3. Read the summary to the operator: the spine, the item count, each role with its model, the caps,
   the pinned plugins, the most launches the run can make, and the line that says item PRs merge
   with no ack. The operator approves what they saw.

4. Start the run with the same command and `--yes` in place of `--dry-run`. It prints the run ID.
   Add `--replace <run>` to replace an earlier run that has no running session.

`arm` refuses with a cause and records nothing when the plan is wrong. A refusal is not an error to
work around: tell the operator the cause.

## Read a run

`pulse-factory status [<run>]` prints the run, its state, any halt or degraded reason, the notes of
the last tick, each unit with its stage and status, each session with its tokens and cost, and the
run's total. The default run is the latest. Add `--json` for the fields by name.

A **parked** unit shows its reason. Every park opens a request, and the operator answers it in the
same run (next section). Nothing needs a new run.

## Answer a parked unit

1. `pulse-factory requests` lists the open requests, each with its kind, its unit and what it blocks.
   `pulse-factory show <R-n>` prints one: its reason, and its body. A `gaps` request holds the gap
   list the seat wrote. A `stop` request holds the stop file's text. Both are the seat's words, shown
   as data.
2. `pulse-factory claim` makes this session the one that may answer. It writes the token to a file
   and prints the generation. A new claim fences the old one out.
3. Decide with the operator, then use `answer` or `clear`:

   ```bash
   pulse-factory answer <R-n> --rev <k> --text "<the ruling>"   # records the ruling and resumes the unit
   pulse-factory clear <run> <item> [--to <stage>]              # resumes the unit with no ruling
   ```

   `--rev` is the revision that `show` printed. A request with an `on_answer` hook in the workflow
   file runs that hook first. In the `ossify-spine` workflow, a `gaps` answer is appended to the
   handoff's `## Clarifications`, and the item runs `implement` again, within the cap of 3 dispatches.
   A `stop` answer is appended to the unit's `rulings.md`, and the stop file is renamed. A hook that
   fails leaves the request answered and parks the unit again, with `answer hook failed:` and the
   cause as the reason of a new request. `clear` runs no hook.

`claim`, `answer` and `clear` refuse a caller that is, or descends from, a seat.

## Halt a run

`pulse-factory halt [<run>] --reason "<why>"` stops new launches and new merges. Seats that run
finish their stage. A repeat is safe, and the first reason stays.

## Other verbs

- `pulse-factory controller` runs the controller in the foreground. The systemd user unit runs it.
  You do not run it yourself.
- `pulse-factory hook` judges one tool call of a seat. The seat's extension runs it. You do not run
  it yourself.

A seat cannot arm or halt a run: both verbs refuse when `PF_SEAT_TOKEN` is set.

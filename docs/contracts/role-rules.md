# Role rule files contract

A **role rule file** says what one role of a workflow may do beyond the base rules. It holds the least
privilege for a seat: a role gets exactly what it lists and nothing more. `pf_workflow::rules::RoleRules`
is the parsed type. `r0.s1.w3`'s `pf-rules` reads these files and adds the deny list.

## Location

One file per role, at the path that the role's `rules` field names in the workflow file. The package
puts them in `rules/<role>.yaml`.

## Format

```yaml
version: 1
role: close
allow:
  - delete_own_scratch
  - push_own_branch
  - write_own_report_and_run_files
```

| Field | Required | Meaning |
|---|---|---|
| `version` | yes | The integer `1`. |
| `role` | yes | The role's name. It must equal the file name without its extension. |
| `allow` | yes | A list of allow values. It may be empty. A value may appear once. |

## Allow values

The set is closed. There are five values:

| Value | Meaning |
|---|---|
| `delete_own_scratch` | The seat may delete files in its own scratch directory. |
| `delete_own_build_target` | The seat may delete its own build target directory. |
| `push_own_branch` | The seat may push the branch of its own unit. |
| `push_own_tag` | The seat may push a tag of its own unit. |
| `write_own_report_and_run_files` | The seat may write its report and its files under the run directory. |

A role that does not list a value does not get it. A role that lists no push value may not push.

## Strict reading

The parser is as strict as the workflow file's. It refuses:

- an unknown field;
- a `version` other than `1`. The error names the supported version. The version is read first;
- an unknown allow value. The error lists the known values;
- a repeated allow value;
- a `role` that does not match the file name, so `implementer.yaml` must say `role: implementer`;
- a duplicate key and a missing required field.

Each error names the line and the column of the text it blames.

## Link to the stage

`StageDef.rules` carries the role's name with `prompts_off: true`. The engine passes it to the agent
port (`apply_rules`). The adapter loads the file and builds the real rules.

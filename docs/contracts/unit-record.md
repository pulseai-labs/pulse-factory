# Unit record contract

A **unit record** is one versioned JSON document that says: here is one unit of work. A seat (or
any producer) writes one record per unit into a run's queue. The engine reads records through its
unit source port (`crates/pf-engine/src/port/unit_source.rs`). This format is the port's contract
(ADR-0002), so a producer for any planning method can emit it.

## Fields

| Field | Type | Meaning |
|---|---|---|
| `version` | integer | The record format version. The engine supports version `1`. |
| `id` | string | The unit's ID. It is unique within a run. Use only `A-Z a-z 0-9 . _ -`. |
| `group` | string | The group the unit belongs to. Units in one group sit together in the declared order. |
| `order` | integer (0 or more) | The unit's place within its group. |
| `vars` | object of string to string | The variables that launch the unit. Keys are sorted. |

All five fields are required. The engine refuses a record with any other field.

## Rules

- **Version.** The engine refuses a record with a `version` it does not support. The error names the
  supported version. The check runs before any other field is read, so a record of a future
  version gets the version error.
- **Admission.** The engine admits each unit once by its `id`. A second record with the same `id`
  and the same content changes nothing. A second record with the same `id` and other content is a
  conflict.
- **Canonical form.** Keys of `vars` are sorted when the engine writes a record, so the JSON text
  of equal records is equal.
- **No plan copy.** A record holds only what launches a unit. It does not copy the planner's state.

## Engine-owned unit detail

The engine keeps more about a unit than its record: the track, the stages done, the PR and the park
generation. This is the unit's detail in the store, not a field of the record, and a producer never
writes it. `park_generation` counts the times a claim resumed the unit (from 0; a detail written
before `r0.s2` reads as 0). `visits` counts, for each `outcomes` entry of the workflow file, the times
the entry sent the unit (key `<stage>#<entry number from 0>`; absent when empty, so a detail written
before stage outcomes reads the same). A `clear` resets it (`workflow-file.md`, "`outcomes`"). Command IDs that are unique per unit end with it (`park:<unit>:<gen>`,
`merge:<unit>:<gen>`, `merge-result:<unit>:<gen>`; see `requests.md`).

## Example

```json
{"version":1,"id":"unit-001","group":"group-a","order":1,"vars":{"branch":"work/unit-001","brief":"implement"}}
```

A record with an unsupported version, such as `"version":2`, is refused with the message
`unsupported unit record version 2; the supported version is 1`.

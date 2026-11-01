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

## Example

```json
{"version":1,"id":"unit-001","group":"group-a","order":1,"vars":{"branch":"work/unit-001","brief":"implement"}}
```

A record with an unsupported version, such as `"version":2`, is refused with the message
`unsupported unit record version 2; the supported version is 1`.

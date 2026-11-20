//! `run-state-admission`'s property test for host positions, on the real store (ADR-0003's
//! amendment of 2026-11-16; `r2.s2.w4`).
//!
//! Generated sequences of `Host` (a launch record and a host row for a unit), `Save` (a
//! `HostPositionWrite` that names any `seq`, so most are refused) and `SaveNext` (a write that reads
//! the store and names the next `seq`, so that most of them apply), each under `@controller`, over
//! three units and a pool of five command IDs. A save comes in three variants of its position, so one
//! command ID can come back with other content. After every step:
//!
//! - a command ID that was applied answers `Replayed` with the first admission when the content is
//!   the same, and `Conflict` with the first admission when it is not, and writes nothing;
//! - a save for a unit with no host row, or with a `seq` that is not the stored one plus 1, is
//!   refused, and a refused command ID is not remembered;
//! - the stored `seq` of a unit rises by exactly 1 for each accepted save;
//! - the read is always the last accepted position, with the command ID that wrote it.
//!
//! Failure persistence is off, as in the other property tests, so a run from another directory
//! leaves no `*.proptest-regressions` file.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::{
    host_commit, new_host, new_record, open, position_commit, record_body, record_commit,
};
use pf_engine::port::store::{Committed, Store};
use pf_store_sqlite::SqliteStore;
use proptest::prelude::*;

const UNITS: [&str; 3] = ["u-1", "u-2", "u-3"];

#[derive(Clone, Debug)]
enum Op {
    Host {
        unit: u8,
    },
    Save {
        cmd: u8,
        unit: u8,
        seq: u8,
        variant: u8,
    },
    SaveNext {
        cmd: u8,
        unit: u8,
        variant: u8,
    },
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        2 => (0u8..3).prop_map(|unit| Op::Host { unit }),
        3 => (0u8..5, 0u8..3, 0u8..5, 0u8..3)
            .prop_map(|(cmd, unit, seq, variant)| Op::Save { cmd, unit, seq, variant }),
        6 => (0u8..5, 0u8..3, 0u8..3)
            .prop_map(|(cmd, unit, variant)| Op::SaveNext { cmd, unit, variant }),
    ]
}

fn position(variant: u8) -> String {
    format!(
        "{{\"offset\":{},\"variant\":{variant}}}",
        100 + u32::from(variant)
    )
}

#[derive(Default)]
struct Model {
    /// The units that have a host row.
    hosts: BTreeSet<String>,
    /// Each applied command ID and the content it was applied with.
    applied: BTreeMap<String, String>,
    /// The accepted saves of each unit: the `seq`, the command ID and the position of the last one.
    saved: BTreeMap<String, (u64, String, String)>,
}

fn step(store: &SqliteStore, m: &mut Model, op: &Op) {
    let (unit, seq, command, position) = match op {
        Op::Host { unit } => {
            let unit = UNITS[usize::from(*unit)];
            if m.hosts.insert(unit.to_string()) {
                let session = format!("S-{unit}");
                let record = new_record(
                    "seat",
                    &session,
                    1,
                    1,
                    &record_body(&common::hashed_secret()),
                );
                let hash = record.record_sha256.clone();
                let record_cmd = format!("lr-{unit}");
                store.commit(record_commit(&record_cmd, record)).unwrap();
                let host = new_host(unit, "seat", &session, 1, 1, &hash);
                store
                    .commit(host_commit(&format!("hs-{unit}"), host))
                    .unwrap();
            }
            return;
        }
        Op::Save {
            cmd,
            unit,
            seq,
            variant,
        } => (
            UNITS[usize::from(*unit)],
            u64::from(*seq),
            format!("c{cmd}"),
            position(*variant),
        ),
        Op::SaveNext { cmd, unit, variant } => {
            let unit = UNITS[usize::from(*unit)];
            let next = store.host_position(unit).unwrap().map_or(1, |r| r.seq + 1);
            (unit, next, format!("c{cmd}"), position(*variant))
        }
    };
    let commit = position_commit(&command, unit, seq, &position);
    let content = commit.content.clone();
    let result = store.commit(commit).unwrap();
    match (m.applied.get(&command).cloned(), result) {
        (Some(first), Committed::Replayed(row)) => {
            assert_eq!(first, content, "{command} replays only the same content");
            assert_eq!(row.command_id, command);
        }
        (Some(first), Committed::Conflict(row)) => {
            assert_ne!(
                first, content,
                "{command} conflicts only with other content"
            );
            assert_eq!(
                row.content, first,
                "the conflict answers the first admission"
            );
        }
        (Some(first), other) => panic!("{command} was applied with {first}, and now: {other:?}"),
        (None, Committed::Applied { admission, ledger }) => {
            assert_eq!(admission.command_id, command);
            assert!(ledger.is_none(), "a save writes no ledger line");
            assert!(
                m.hosts.contains(unit),
                "{unit} has no host row, and a save applied"
            );
            let before = m.saved.get(unit).map_or(0, |s| s.0);
            assert_eq!(seq, before + 1, "{unit}: seq rises by exactly 1");
            m.applied.insert(command.clone(), content);
            m.saved
                .insert(unit.to_string(), (seq, command.clone(), position));
        }
        (None, Committed::Rejected(why)) => {
            assert!(!why.is_empty());
            let before = m.saved.get(unit).map_or(0, |s| s.0);
            assert!(
                !m.hosts.contains(unit) || seq != before + 1,
                "{unit}: seq {seq} after {before} was refused with a host row: {why}"
            );
        }
        (None, other) => panic!("{command} was never applied, and now: {other:?}"),
    }
}

fn check_after(store: &SqliteStore, m: &Model) {
    for unit in UNITS {
        let row = store.host_position(unit).unwrap();
        match (row, m.saved.get(unit)) {
            (Some(row), Some((seq, command, position))) => {
                assert_eq!(row.seq, *seq, "{unit}: the stored seq");
                assert_eq!(
                    &row.command_id, command,
                    "{unit}: the command that wrote it"
                );
                assert_eq!(
                    &row.position, position,
                    "{unit}: the last accepted position"
                );
            }
            (None, None) => {}
            (row, saved) => panic!("{unit}: row {row:?}, model {saved:?}"),
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn positions_admit_once_replay_by_id_and_rise_by_exactly_one(
        ops in prop::collection::vec(op(), 1..40)
    ) {
        let (_d, store) = open();
        let mut m = Model::default();
        for op in &ops {
            step(&store, &mut m, op);
            check_after(&store, &m);
        }
    }
}

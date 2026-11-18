//! `run-state-admission`'s property test for launch records and the host registry, on the real
//! store (ADR-0003's amendment of 2026-11-16; `r2.s2.w2`).
//!
//! Generated sequences of `Record` (a `LaunchRecordWrite`), `Start` (a `HostStarted`) and `End` (a
//! `HostEnded`), each under `@controller` with a command ID from a pool of five, over two sessions,
//! two incarnations, three revisions and two units. A record body comes in two variants, so one
//! command ID can come back with other content. `Record` and `Start` name any key, so most are
//! refused. `Next` and `StartLatest` read the store and name the next revision, the next
//! incarnation, or the newest record, so that most of them apply, and a host really starts and
//! ends. After every step:
//!
//! - a command ID that was applied answers `Replayed` with the first admission when the content is
//!   the same, and `Conflict` with the first admission when it is not, and writes nothing;
//! - a command ID that was refused is not remembered: a later commit may use it;
//! - no session, incarnation and revision has a second row, whatever the IDs: its row carries the
//!   first command ID that was applied for it, and the table holds one row per applied key;
//! - the same holds for a host unit (`HostStarted`), and a host ends once (`HostEnded`).
//!
//! Failure persistence is off, as in the other property tests, so a run from another directory
//! leaves no `*.proptest-regressions` file.

mod common;

use std::collections::BTreeMap;

use common::{
    host_commit, host_end_commit, new_host, new_record, open, record_body, record_commit,
};
use pf_engine::port::store::{Commit, Committed, Store};
use pf_store_sqlite::SqliteStore;
use proptest::prelude::*;

const SESSIONS: [&str; 2] = ["S-1", "S-2"];
const UNITS: [&str; 2] = ["u-1", "u-2"];

#[derive(Clone, Debug)]
enum Op {
    Record {
        cmd: u8,
        session: u8,
        incarnation: u8,
        revision: u8,
        variant: u8,
    },
    Start {
        cmd: u8,
        unit: u8,
        session: u8,
        incarnation: u8,
        revision: u8,
    },
    /// The next revision of a session's newest incarnation, or the next incarnation.
    Next {
        cmd: u8,
        session: u8,
        new_incarnation: bool,
        variant: u8,
    },
    /// A host for the newest incarnation of a session, from its first or its newest revision.
    StartLatest {
        cmd: u8,
        unit: u8,
        session: u8,
        first_revision: bool,
    },
    End {
        cmd: u8,
        unit: u8,
        code: u8,
    },
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        2 => (0u8..5, 0u8..2, 1u8..3, 1u8..4, 0u8..2).prop_map(
            |(cmd, session, incarnation, revision, variant)| Op::Record {
                cmd,
                session,
                incarnation,
                revision,
                variant
            }
        ),
        1 => (0u8..5, 0u8..2, 0u8..2, 1u8..3, 1u8..4).prop_map(
            |(cmd, unit, session, incarnation, revision)| Op::Start {
                cmd,
                unit,
                session,
                incarnation,
                revision
            }
        ),
        5 => (0u8..5, 0u8..2, any::<bool>(), 0u8..2).prop_map(
            |(cmd, session, new_incarnation, variant)| Op::Next {
                cmd,
                session,
                new_incarnation,
                variant
            }
        ),
        4 => (0u8..5, 0u8..2, 0u8..2, any::<bool>()).prop_map(
            |(cmd, unit, session, first_revision)| Op::StartLatest {
                cmd,
                unit,
                session,
                first_revision
            }
        ),
        4 => (0u8..5, 0u8..2, 0u8..2).prop_map(|(cmd, unit, code)| Op::End { cmd, unit, code }),
    ]
}

fn body(variant: u8) -> String {
    record_body(&common::hashed_secret()).replace("/work/alpha", &format!("/work/v{variant}"))
}

#[derive(Default)]
struct Model {
    /// Each applied command ID and the content it was applied with.
    applied: BTreeMap<String, String>,
    /// The command ID that was applied first for each launch record key.
    records: BTreeMap<(String, u32, u32), String>,
    /// The command ID that started each unit, and the one that ended it.
    started: BTreeMap<String, String>,
    ended: BTreeMap<String, String>,
}

/// The newest incarnation of a session and its newest revision, read from the store.
fn newest(store: &SqliteStore, session: &str) -> Option<(u32, u32)> {
    let mut found = None;
    let mut incarnation = 1;
    while let Some(row) = store
        .newest_launch_record("chat", session, incarnation)
        .unwrap()
    {
        found = Some((incarnation, row.revision));
        incarnation += 1;
    }
    found
}

fn step(store: &SqliteStore, m: &mut Model, op: &Op) {
    let (commit, key): (Commit, Key) = match op {
        Op::Next {
            cmd,
            session,
            new_incarnation,
            variant,
        } => {
            let session = SESSIONS[usize::from(*session)];
            let (incarnation, revision) = match (newest(store, session), new_incarnation) {
                (None, _) => (1, 1),
                (Some((i, _)), true) => (i + 1, 1),
                (Some((i, r)), false) => (i, r + 1),
            };
            let record = new_record("chat", session, incarnation, revision, &body(*variant));
            (
                record_commit(&format!("c{cmd}"), record),
                Key::Record(session.to_string(), incarnation, revision),
            )
        }
        Op::StartLatest {
            cmd,
            unit,
            session,
            first_revision,
        } => {
            let session = SESSIONS[usize::from(*session)];
            let (incarnation, newest_revision) = newest(store, session).unwrap_or((1, 1));
            let revision = if *first_revision { 1 } else { newest_revision };
            let hash = store
                .launch_record("chat", session, incarnation, revision)
                .unwrap()
                .map_or_else(|| "0".repeat(64), |r| r.record_sha256);
            let unit = UNITS[usize::from(*unit)];
            (
                host_commit(
                    &format!("c{cmd}"),
                    new_host(unit, "chat", session, incarnation, revision, &hash),
                ),
                Key::Start(unit.to_string()),
            )
        }
        Op::Record {
            cmd,
            session,
            incarnation,
            revision,
            variant,
        } => {
            let session = SESSIONS[usize::from(*session)];
            let record = new_record(
                "chat",
                session,
                u32::from(*incarnation),
                u32::from(*revision),
                &body(*variant),
            );
            (
                record_commit(&format!("c{cmd}"), record),
                Key::Record(
                    session.to_string(),
                    u32::from(*incarnation),
                    u32::from(*revision),
                ),
            )
        }
        Op::Start {
            cmd,
            unit,
            session,
            incarnation,
            revision,
        } => {
            let session = SESSIONS[usize::from(*session)];
            let (inc, rev) = (u32::from(*incarnation), u32::from(*revision));
            // The hash of the stored revision when there is one, so a start can succeed.
            let hash = store
                .launch_record("chat", session, inc, rev)
                .unwrap()
                .map_or_else(|| "0".repeat(64), |r| r.record_sha256);
            let unit = UNITS[usize::from(*unit)];
            (
                host_commit(
                    &format!("c{cmd}"),
                    new_host(unit, "chat", session, inc, rev, &hash),
                ),
                Key::Start(unit.to_string()),
            )
        }
        Op::End { cmd, unit, code } => {
            let unit = UNITS[usize::from(*unit)];
            (
                host_end_commit(&format!("c{cmd}"), unit, &format!("{{\"code\":{code}}}")),
                Key::End(unit.to_string()),
            )
        }
    };
    let command = commit.command_id.clone();
    let content = commit.content.clone();
    let result = store.commit(commit).unwrap();
    match (m.applied.get(&command).cloned(), result) {
        (Some(first), Committed::Replayed(row)) => {
            assert_eq!(first, content, "{command} replays only the same content");
            assert_eq!(
                (row.command_id.as_str(), row.content.as_str()),
                (command.as_str(), first.as_str())
            );
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
        (None, Committed::Applied { admission, .. }) => {
            assert_eq!(admission.command_id, command);
            m.applied.insert(command.clone(), content);
            match key {
                Key::Record(s, i, r) => {
                    let first = m
                        .records
                        .entry((s, i, r))
                        .or_insert_with(|| command.clone());
                    assert_eq!(
                        *first, command,
                        "a second row for one session, incarnation and revision"
                    );
                }
                Key::Start(u) => {
                    let first = m.started.entry(u).or_insert_with(|| command.clone());
                    assert_eq!(*first, command, "a second host row for one unit");
                }
                Key::End(u) => {
                    let first = m.ended.entry(u).or_insert_with(|| command.clone());
                    assert_eq!(*first, command, "a second end for one unit");
                }
            }
        }
        (None, Committed::Rejected(why)) => assert!(!why.is_empty()),
        (None, other) => panic!("{command} was never applied, and now: {other:?}"),
    }
}

enum Key {
    Record(String, u32, u32),
    Start(String),
    End(String),
}

fn check_after(store: &SqliteStore, m: &Model) {
    // One row per applied key, and it carries the first command ID. The model's keys are every row
    // there should be: the store holds the newest revision of each, and no revision or incarnation
    // beyond them.
    for session in SESSIONS {
        let model_keys: Vec<(u32, u32)> = m
            .records
            .keys()
            .filter(|(s, _, _)| s == session)
            .map(|(_, i, r)| (*i, *r))
            .collect();
        let max_incarnation = model_keys.iter().map(|(i, _)| *i).max().unwrap_or(0);
        for incarnation in 1..=max_incarnation + 1 {
            let max_revision = model_keys
                .iter()
                .filter(|(i, _)| *i == incarnation)
                .map(|(_, r)| *r)
                .max()
                .unwrap_or(0);
            let newest_row = store
                .newest_launch_record("chat", session, incarnation)
                .unwrap();
            assert_eq!(newest_row.map(|r| r.revision).unwrap_or(0), max_revision);
            for revision in 1..=max_revision {
                let row = store
                    .launch_record("chat", session, incarnation, revision)
                    .unwrap()
                    .unwrap_or_else(|| panic!("no row for {session} {incarnation} {revision}"));
                let key = (session.to_string(), incarnation, revision);
                assert_eq!(Some(&row.command_id), m.records.get(&key));
            }
        }
    }
    for unit in UNITS {
        let host = store.host(unit).unwrap();
        match (host, m.started.get(unit)) {
            (Some(host), Some(first)) => {
                assert_eq!(&host.command_id, first);
                assert_eq!(host.ended_at.is_some(), m.ended.contains_key(unit));
                assert_eq!(host.exit.is_some(), m.ended.contains_key(unit));
            }
            (None, None) => assert!(!m.ended.contains_key(unit), "{unit} ended without a row"),
            (host, first) => panic!("{unit}: row {host:?}, first command {first:?}"),
        }
    }
    let live = store.live_hosts().unwrap().len();
    let ended = m.ended.len();
    assert_eq!(live, m.started.len() - ended, "the live hosts have no end");
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn records_and_hosts_admit_once_replay_by_id_and_leave_one_row_per_key(
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

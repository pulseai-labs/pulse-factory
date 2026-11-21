//! `run-state-admission`'s property test for notices, on the real store (ADR-0003's amendment of
//! 2026-11-21; `r2.s11.w3`, AC-2).
//!
//! Generated sequences of `Add` (a notice of any request, revision and event), `Supersede` (a
//! delivery of any position and channel), `Send` and `SendNext` (a send that names any count, and one
//! that reads the store and names the next), `Ack` and `AckNext` (an acknowledgement of any position,
//! and one that names the next position that can rise), each under `@controller`, over three
//! requests, three revisions, three events, three targets and a pool of six command IDs. After every
//! step:
//!
//! - a command ID that was applied answers `Replayed` with the first admission when the content is
//!   the same, and `Conflict` with the first admission when it is not, and writes nothing;
//! - a notice of an event that has one is refused, and a refused command ID is not remembered;
//! - a delivery that waits is superseded once, and a send names the stored count plus 1;
//! - an acknowledgement applies only above the stored position and at or below the newest notice;
//! - a position never falls, the positions of the notices are 1, 2, 3 with no gap, and no
//!   `(request, revision, event)` holds two notices.
//!
//! Failure persistence is off, as in the other property tests, so a run from another directory
//! leaves no `*.proptest-regressions` file.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::{ack_commit, notice_commit, open, send_commit, supersede_commit, waiting};
use pf_engine::port::store::{Commit, Committed, DeliveryState, Store};
use pf_store_sqlite::SqliteStore;
use proptest::prelude::*;

const REQUESTS: [&str; 3] = ["R-1", "R-2", "R-3"];
const EVENTS: [&str; 3] = ["opened", "revised", "closed"];
const TARGETS: [(&str, &str); 3] = [
    ("device:D-1", "app"),
    ("device:D-2", "app"),
    ("agent:P-1:orchestrator", "orchestrator"),
];
const CHANNELS: [&str; 2] = ["app", "orchestrator"];

#[derive(Clone, Debug)]
enum Op {
    Add {
        cmd: u8,
        request: u8,
        revision: u8,
        event: u8,
    },
    Supersede {
        cmd: u8,
        position: u8,
        channel: u8,
    },
    Send {
        cmd: u8,
        position: u8,
        target: u8,
        sends: u8,
    },
    SendNext {
        cmd: u8,
        position: u8,
        target: u8,
    },
    Ack {
        cmd: u8,
        target: u8,
        position: u8,
    },
    AckNext {
        cmd: u8,
        target: u8,
    },
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        5 => (0u8..6, 0u8..3, 1u8..4, 0u8..3)
            .prop_map(|(cmd, request, revision, event)| Op::Add { cmd, request, revision, event }),
        3 => (0u8..6, 0u8..6, 0u8..2)
            .prop_map(|(cmd, position, channel)| Op::Supersede { cmd, position, channel }),
        2 => (0u8..6, 0u8..6, 0u8..3, 0u8..5)
            .prop_map(|(cmd, position, target, sends)| Op::Send { cmd, position, target, sends }),
        4 => (0u8..6, 0u8..6, 0u8..3)
            .prop_map(|(cmd, position, target)| Op::SendNext { cmd, position, target }),
        3 => (0u8..6, 0u8..3, 0u8..6)
            .prop_map(|(cmd, target, position)| Op::Ack { cmd, target, position }),
        4 => (0u8..6, 0u8..3).prop_map(|(cmd, target)| Op::AckNext { cmd, target }),
    ]
}

#[derive(Default)]
struct Model {
    /// Each applied command ID and the content it was applied with.
    applied: BTreeMap<String, String>,
    /// The notices, by position (index + 1): request, revision, event.
    notices: Vec<(String, u32, String)>,
    /// Whether each delivery still waits, by (position, channel).
    waiting: BTreeMap<(u64, String), bool>,
    /// The send count of each (position, target).
    sends: BTreeMap<(u64, String), u64>,
    /// The channel and the position of each target.
    targets: BTreeMap<String, (String, u64)>,
}

impl Model {
    fn newest(&self) -> u64 {
        self.notices.len() as u64
    }
}

fn target(index: u8) -> (&'static str, &'static str) {
    TARGETS[usize::from(index)]
}

/// The outcome the model expects for a fresh command, as `Ok(())` for applied or the reason word.
fn expect(m: &Model, op: &Op) -> Result<(), &'static str> {
    match op {
        Op::Add {
            request,
            revision,
            event,
            ..
        } => {
            let key = (
                REQUESTS[usize::from(*request)].to_string(),
                u32::from(*revision),
                EVENTS[usize::from(*event)].to_string(),
            );
            if m.notices.contains(&key) {
                Err("the event has a notice already")
            } else {
                Ok(())
            }
        }
        Op::Supersede {
            position, channel, ..
        } => match m.waiting.get(&(
            u64::from(*position),
            CHANNELS[usize::from(*channel)].to_string(),
        )) {
            Some(true) => Ok(()),
            _ => Err("no waiting delivery"),
        },
        Op::Send {
            position,
            target: t,
            sends,
            ..
        } => {
            let position = u64::from(*position);
            if position == 0 || position > m.newest() {
                return Err("no notice");
            }
            let stored = m
                .sends
                .get(&(position, target(*t).0.to_string()))
                .copied()
                .unwrap_or(0);
            if u64::from(*sends) == stored + 1 {
                Ok(())
            } else {
                Err("not the next count")
            }
        }
        Op::SendNext { position, .. } => {
            let position = u64::from(*position);
            if position == 0 || position > m.newest() {
                Err("no notice")
            } else {
                Ok(())
            }
        }
        Op::Ack {
            target: t,
            position,
            ..
        } => ack_expect(m, *t, u64::from(*position)),
        Op::AckNext { target: t, .. } => {
            let (name, _) = target(*t);
            let stored = m.targets.get(name).map_or(0, |(_, acked)| *acked);
            ack_expect(m, *t, stored + 1)
        }
    }
}

fn ack_expect(m: &Model, t: u8, position: u64) -> Result<(), &'static str> {
    let (name, _) = target(t);
    if position > m.newest() {
        return Err("above the newest");
    }
    match m.targets.get(name) {
        Some((_, acked)) if position <= *acked => Err("does not rise"),
        _ => Ok(()),
    }
}

fn build(m: &Model, store: &SqliteStore, op: &Op) -> Commit {
    match op {
        Op::Add {
            cmd,
            request,
            revision,
            event,
        } => notice_commit(
            &format!("c{cmd}"),
            REQUESTS[usize::from(*request)],
            u32::from(*revision),
            EVENTS[usize::from(*event)],
            vec![waiting("app", "App."), waiting("orchestrator", "Chat.")],
        ),
        Op::Supersede {
            cmd,
            position,
            channel,
        } => supersede_commit(
            &format!("c{cmd}"),
            u64::from(*position),
            CHANNELS[usize::from(*channel)],
            "request_closed",
        ),
        Op::Send {
            cmd,
            position,
            target: t,
            sends,
        } => {
            let mut c = send_commit(u64::from(*position), target(*t).0, u64::from(*sends));
            c.command_id = format!("c{cmd}");
            c
        }
        Op::SendNext {
            cmd,
            position,
            target: t,
        } => {
            let position = u64::from(*position);
            let stored = store
                .notice_sends(position)
                .unwrap()
                .into_iter()
                .find(|s| s.target == target(*t).0)
                .map_or(0, |s| s.sends);
            let mut c = send_commit(position, target(*t).0, stored + 1);
            c.command_id = format!("c{cmd}");
            c
        }
        Op::Ack {
            cmd,
            target: t,
            position,
        } => {
            let (name, channel) = target(*t);
            ack_commit(
                &format!("c{cmd}"),
                name,
                channel,
                u64::from(*position),
                "shown",
            )
        }
        Op::AckNext { cmd, target: t } => {
            let (name, channel) = target(*t);
            let stored = m.targets.get(name).map_or(0, |(_, acked)| *acked);
            ack_commit(&format!("c{cmd}"), name, channel, stored + 1, "shown")
        }
    }
}

fn step(store: &SqliteStore, m: &mut Model, op: &Op) {
    let commit = build(m, store, op);
    let command = commit.command_id.clone();
    let content = commit.content.clone();
    let expected = expect(m, op);
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
        (None, Committed::Applied { admission, .. }) => {
            assert_eq!(admission.command_id, command);
            assert_eq!(expected, Ok(()), "{op:?} applied, and the model refuses it");
            m.applied.insert(command, content);
            apply_to_model(m, op, admission.unit_id.as_deref());
        }
        (None, Committed::Rejected(why)) => {
            assert!(!why.is_empty());
            assert!(
                expected.is_err(),
                "{op:?} was refused ({why}), and the model admits it"
            );
        }
        (None, other) => panic!("{command} was never applied, and now: {other:?}"),
    }
}

fn apply_to_model(m: &mut Model, op: &Op, unit_id: Option<&str>) {
    match op {
        Op::Add {
            request,
            revision,
            event,
            ..
        } => {
            m.notices.push((
                REQUESTS[usize::from(*request)].to_string(),
                u32::from(*revision),
                EVENTS[usize::from(*event)].to_string(),
            ));
            let position = m.newest();
            assert_eq!(
                unit_id,
                Some(position.to_string().as_str()),
                "the position is the next of one sequence"
            );
            for channel in CHANNELS {
                m.waiting.insert((position, channel.to_string()), true);
            }
        }
        Op::Supersede {
            position, channel, ..
        } => {
            m.waiting.insert(
                (
                    u64::from(*position),
                    CHANNELS[usize::from(*channel)].to_string(),
                ),
                false,
            );
        }
        Op::Send {
            position,
            target: t,
            ..
        }
        | Op::SendNext {
            position,
            target: t,
            ..
        } => {
            *m.sends
                .entry((u64::from(*position), target(*t).0.to_string()))
                .or_insert(0) += 1;
        }
        Op::Ack {
            target: t,
            position,
            ..
        } => {
            let (name, channel) = target(*t);
            m.targets.insert(
                name.to_string(),
                (channel.to_string(), u64::from(*position)),
            );
        }
        Op::AckNext { target: t, .. } => {
            let (name, channel) = target(*t);
            let next = m.targets.get(name).map_or(0, |(_, a)| *a) + 1;
            m.targets
                .insert(name.to_string(), (channel.to_string(), next));
        }
    }
}

fn check_after(store: &SqliteStore, m: &Model, before: &BTreeMap<String, u64>) {
    // The notices: positions 1, 2, 3 with no gap, and no event twice.
    let rows = store.notices_after(0, 1000).unwrap();
    assert_eq!(rows.len(), m.notices.len());
    let mut seen = BTreeSet::new();
    for (i, row) in rows.iter().enumerate() {
        assert_eq!(row.position, i as u64 + 1, "the positions have no gap");
        assert_eq!(
            (&row.request_id, row.revision, &row.event),
            (&m.notices[i].0, m.notices[i].1, &m.notices[i].2)
        );
        assert!(
            seen.insert((row.request_id.clone(), row.revision, row.event.clone())),
            "{:?} holds two notices",
            (&row.request_id, row.revision, &row.event)
        );
    }
    assert_eq!(store.newest_notice().unwrap(), m.newest());
    // The deliveries.
    for ((position, channel), is_waiting) in &m.waiting {
        let row = store
            .notice_deliveries(*position)
            .unwrap()
            .into_iter()
            .find(|d| &d.channel == channel)
            .expect("a delivery");
        let expected = if *is_waiting {
            DeliveryState::Waiting
        } else {
            DeliveryState::Superseded
        };
        assert_eq!(row.state, expected, "delivery {position}/{channel}");
    }
    // The sends.
    for ((position, name), n) in &m.sends {
        let row = store
            .notice_sends(*position)
            .unwrap()
            .into_iter()
            .find(|s| &s.target == name)
            .expect("a send row");
        assert_eq!(row.sends, *n, "sends {position}/{name}");
    }
    // The targets: the stored position is the model's, and never below what it was.
    for row in store.notice_targets().unwrap() {
        let (channel, acked) = &m.targets[&row.target];
        assert_eq!(
            (&row.channel, row.acked),
            (channel, *acked),
            "{}",
            row.target
        );
        assert!(
            row.acked >= before.get(&row.target).copied().unwrap_or(0),
            "{}: the position fell",
            row.target
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn notices_admit_once_replay_by_id_and_only_rise(
        ops in prop::collection::vec(op(), 1..40)
    ) {
        let (_d, store) = open();
        let mut m = Model::default();
        for op in &ops {
            let before: BTreeMap<String, u64> = m
                .targets
                .iter()
                .map(|(t, (_, a))| (t.clone(), *a))
                .collect();
            step(&store, &mut m, op);
            check_after(&store, &m, &before);
        }
    }
}

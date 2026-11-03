//! AC-4 (`run-state-admission`, property test): over random sequences of park, clear, answer,
//! re-park, halt and replay on one unit, the unit always has exactly one status, at most one open
//! park request, a park generation equal to the number of resumes (clears and answers), and an
//! admissions table in which every ID is unique.
//!
//! The shared cargo config turns proptest's failure persistence off. The config below does too, so
//! a run from another directory leaves no `*.proptest-regressions` file.

mod common;

use std::collections::HashSet;

use common::*;
use pf_engine::admission::body_of_content;
use pf_engine::claim::sha256_hex;
use pf_engine::port::store::{RequestKind, RequestStatus};
use pf_engine::{Command, EndStatus, EngineError, Status};
use proptest::prelude::*;

#[derive(Clone, Debug)]
enum Op {
    /// Launch the unit's next stage.
    Launch,
    /// Park the unit, with the request kind at this place of the list.
    Park(usize),
    /// End the unit's running session, as the driver does after a park.
    EndSession,
    /// Clear the unit's open request to its own stage, or to `implement`.
    Clear(bool),
    /// Answer the unit's open request with a ruling.
    Answer,
    Halt,
    /// Take the claim again. The old one is fenced.
    Claim,
    /// Clear with the claim before the current one.
    StaleClear,
    /// Submit a stored command again.
    Replay(usize),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => Just(Op::Launch),
        4 => (0usize..7).prop_map(Op::Park),
        3 => Just(Op::EndSession),
        3 => any::<bool>().prop_map(Op::Clear),
        2 => Just(Op::Answer),
        1 => Just(Op::Halt),
        1 => Just(Op::Claim),
        1 => Just(Op::StaleClear),
        3 => (0usize..1000).prop_map(Op::Replay),
    ]
}

const KINDS: [RequestKind; 7] = [
    RequestKind::Park,
    RequestKind::Gaps,
    RequestKind::Stop,
    RequestKind::RedGate,
    RequestKind::Stall,
    RequestKind::MergeFailed,
    RequestKind::SeatLost,
];

fn check_invariants(env: &Env, resumes: u32) -> Result<(), TestCaseError> {
    let view = env.engine.view().unwrap();
    // Exactly one node, so exactly one status.
    let nodes: Vec<_> = view.nodes.iter().filter(|n| n.id == "u1").collect();
    prop_assert_eq!(nodes.len(), 1, "one node for the unit");
    let status = nodes[0].status;

    // At most one open request for the unit, and it is there exactly when the unit is parked.
    let open: Vec<_> = env
        .store
        .all_requests()
        .into_iter()
        .filter(|r| r.unit_id.as_deref() == Some("u1") && r.status == RequestStatus::Open)
        .collect();
    prop_assert!(open.len() <= 1, "open requests: {:?}", open);
    prop_assert_eq!(
        open.len() == 1,
        status == Status::Parked,
        "an open request exactly when parked: {:?} {:?}",
        status,
        open
    );

    // The park generation counts the resumes.
    let detail = view.unit_detail(nodes[0]).unwrap();
    prop_assert_eq!(detail.park_generation, resumes);
    // Each resume answered one request; each request that is answered was resumed.
    let answered = env
        .store
        .all_requests()
        .iter()
        .filter(|r| r.status == RequestStatus::Answered)
        .count() as u32;
    prop_assert_eq!(answered, resumes);

    // Every command ID is unique in the admissions table.
    let ids: HashSet<&str> = view
        .admissions
        .iter()
        .map(|a| a.command_id.as_str())
        .collect();
    prop_assert_eq!(ids.len(), view.admissions.len());
    // Request IDs are unique too, and rise from R-1 with no gap.
    let requests = env.store.all_requests();
    for (i, r) in requests.iter().enumerate() {
        prop_assert_eq!(&r.id, &format!("R-{}", i + 1));
    }
    Ok(())
}

fn run_ops(ops: &[Op]) -> Result<(), TestCaseError> {
    let env = env(item_workflow(&["implement", "verify"], true));
    env.engine
        .ingest(&mut FakeUnitSource {
            records: vec![unit("u1", "g1", 1)],
        })
        .unwrap();
    let mut resumes = 0u32;
    let mut claims = 0u64;
    let mut cycle = 0u64;
    check_invariants(&env, resumes)?;

    for op in ops {
        let before = env.engine.view().unwrap();
        let result: Result<pf_engine::Admission, EngineError> = match op {
            Op::Launch => env.engine.start_stage("u1").map(|_| {
                // A launch has its own receipt; report the last admission.
                let view = env.engine.view().unwrap();
                pf_engine::Admission {
                    row: view.admissions.last().unwrap().clone(),
                    replayed: true,
                }
            }),
            Op::Park(i) => env.engine.park_as("u1", KINDS[*i], "random park"),
            Op::EndSession => {
                let view = env.engine.view().unwrap();
                match view.running_stage_of("u1") {
                    Some(stage) => env.engine.end(&stage.id, EndStatus::Failed, "ended"),
                    None => continue,
                }
            }
            Op::Clear(explicit) => {
                let view = env.engine.view().unwrap();
                let generation = view.current_claim().map_or(1, |c| c.generation);
                let request = view
                    .open_request_of("u1")
                    .map_or("R-1".to_string(), |r| r.id.clone());
                let to = explicit.then_some("implement");
                env.engine.clear_unit("u1", to, generation, &request)
            }
            Op::Answer => {
                let view = env.engine.view().unwrap();
                let generation = view.current_claim().map_or(1, |c| c.generation);
                match view.open_request_of("u1") {
                    Some(r) => env
                        .engine
                        .answer_request(&r.id, r.revision, generation, "a ruling"),
                    None => continue,
                }
            }
            Op::Halt => env.engine.halt("random halt"),
            Op::Claim => {
                claims += 1;
                cycle += 1;
                env.engine
                    .claim("op@host", &sha256_hex(format!("token {cycle}").as_bytes()))
                    .map(|g| g.admission)
            }
            Op::StaleClear => {
                let view = env.engine.view().unwrap();
                let generation = view
                    .current_claim()
                    .map_or(0, |c| c.generation.saturating_sub(1));
                let request = view
                    .open_request_of("u1")
                    .map_or("R-1".to_string(), |r| r.id.clone());
                env.engine.clear_unit("u1", None, generation, &request)
            }
            Op::Replay(i) => {
                let rows = &before.admissions;
                let row = &rows[i % rows.len()];
                let body = body_of_content(&row.content).expect("a stored command reads back");
                let replayed = env
                    .engine
                    .submit(Command {
                        id: row.command_id.clone(),
                        body,
                    })
                    .expect("a replay of a stored command is not an error");
                prop_assert!(replayed.replayed, "{} must replay", row.command_id);
                prop_assert_eq!(&replayed.row, row);
                let after = env.engine.view().unwrap();
                prop_assert_eq!(after.admissions.len(), before.admissions.len());
                prop_assert_eq!(after.nodes.len(), before.nodes.len());
                check_invariants(&env, resumes)?;
                continue;
            }
        };

        match (&result, op) {
            (Ok(adm), Op::Clear(_) | Op::Answer) if !adm.replayed => resumes += 1,
            (Ok(adm), Op::StaleClear) if !adm.replayed && claims >= 2 => {
                prop_assert!(false, "a fenced claim cleared a unit");
            }
            _ => {}
        }
        if let Err(EngineError::Rejected(_) | EngineError::Conflict { .. }) = &result {
            // A refused command records nothing.
            let after = env.engine.view().unwrap();
            prop_assert_eq!(after.admissions.len(), before.admissions.len());
        }
        check_invariants(&env, resumes)?;
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 96,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn a_unit_keeps_one_status_one_open_request_and_a_generation_that_counts_its_resumes(
        ops in proptest::collection::vec(op(), 1..40)
    ) {
        run_ops(&ops)?;
    }
}

/// The sequence that the property is built around, fixed: park, clear, park again, answer.
#[test]
fn a_fixed_sequence_of_park_clear_repark_answer_and_halt() {
    let ops = [
        Op::Claim,
        Op::Launch,
        Op::Park(0),
        Op::EndSession,
        Op::Clear(false),
        Op::Launch,
        Op::Park(4),
        Op::EndSession,
        Op::Answer,
        Op::Halt,
        Op::Park(0),
        Op::Replay(2),
        Op::Replay(0),
    ];
    run_ops(&ops).unwrap();
}

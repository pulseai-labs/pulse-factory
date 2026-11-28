//! AC-2 of `r2.s3.w2`: the property test of the settle decision (`run-state-admission`, property
//! test; audit C6). For any order of settles, retries with other reasons, stale revisions, claim
//! clears, launches, parks, halts, grants and revokes over two units of one run:
//!
//! - the same command ID and content returns the same record;
//! - a settle never launches a unit twice: each admitted settle is followed by at most one admitted
//!   launch of its unit before the unit parks again;
//! - a unit's `park_generation` only rises;
//! - a commit that carries `RequireLiveGrant` is applied only while its generation is live, and
//!   never after that generation's revoke;
//! - no grant act passes the run's stored launch count.

mod common;

use std::collections::BTreeMap;

use common::*;
use pf_engine::EndStatus;
use pf_engine::claim::{Act, SettleAsk, SettleFacts, sha256_hex};
use pf_engine::launch::launches_so_far;
use pf_engine::port::store::{
    AdmissionOutcome, AdmissionRow, CONTROLLER_SCOPE, Cause, Commit, Committed, Effect,
    NewAdmission, NewClearGrant, ORCHESTRATOR_AGENT, RequestKind, RequestStatus, Store,
};
use pf_engine::port::unit_source::UnitRecord;
use proptest::prelude::*;

const FORGE: (&str, &str) = ("synthetic/repo", "main");
const UNITS: [&str; 2] = ["u0", "u1"];
const BOUND: u64 = 5;

#[derive(Clone, Debug)]
enum Op {
    /// Launch the unit's next stage, as a tick does.
    Launch(usize),
    /// Park the unit with a stall and end its seat's stage.
    Park(usize),
    /// A settle of the unit's open request: `clear` or `redispatch --to build`. `stale` names a
    /// revision before the open one; `old_grant` names a generation that is not the live one.
    Settle {
        unit: usize,
        redispatch: bool,
        reason: u8,
        stale: bool,
        old_grant: bool,
    },
    /// A retry of the n-th admitted settle, with another reason.
    Retry(usize),
    /// The claim's clear of the unit's open request.
    ClaimClear(usize),
    /// The operator's halt.
    Halt,
    /// The operator ends the live grant.
    Revoke,
    /// The operator gives the grant again, at the next generation.
    Regrant,
    /// The request's facts change: a new revision.
    Revise(usize),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (0..2usize).prop_map(Op::Launch),
        3 => (0..2usize).prop_map(Op::Park),
        6 => (0..2usize, any::<bool>(), 0..4u8, any::<bool>(), any::<bool>()).prop_map(
            |(unit, redispatch, reason, stale, old_grant)| Op::Settle {
                unit,
                redispatch,
                reason,
                stale: stale && reason == 0,
                old_grant: old_grant && reason == 1,
            }
        ),
        2 => (0..8usize).prop_map(Op::Retry),
        2 => (0..2usize).prop_map(Op::ClaimClear),
        1 => Just(Op::Halt),
        1 => Just(Op::Revoke),
        2 => Just(Op::Regrant),
        1 => (0..2usize).prop_map(Op::Revise),
    ]
}

fn commit_of(id: &str, effects: Vec<Effect>) -> Commit {
    Commit {
        run_id: CONTROLLER_SCOPE.to_string(),
        command_id: id.to_string(),
        content: id.to_string(),
        admission: NewAdmission {
            kind: "probe".to_string(),
            outcome: AdmissionOutcome::Admitted,
            reason: None,
            unit_id: None,
            role: None,
            model: None,
            attempt: None,
            admitted_at: "t".to_string(),
        },
        effects,
        ledger: None,
    }
}

fn apply(env: &Env, id: &str, effects: Vec<Effect>) {
    let applied = env.store.commit(commit_of(id, effects)).unwrap();
    assert!(matches!(applied, Committed::Applied { .. }), "{applied:?}");
}

fn rig() -> Env {
    let mut wf = item_workflow(&["build", "review"], false);
    wf.tracks[0].name = "item".to_string();
    let env = env_with_params(wf, BTreeMap::new());
    env.engine
        .ingest(&mut TrackedSource {
            track: "item".to_string(),
            records: UNITS
                .iter()
                .enumerate()
                .map(|(i, id)| {
                    UnitRecord::new(*id, "g1", u32::try_from(i).unwrap(), BTreeMap::new())
                })
                .collect(),
        })
        .unwrap();
    env.store.add_project("P-1");
    apply(
        &env,
        "bound",
        vec![Effect::SetRunLaunchBound {
            run_id: "run-1".to_string(),
            bound: BOUND,
        }],
    );
    env
}

fn grant_effect(generation: u64) -> Effect {
    Effect::GrantClear(NewClearGrant {
        project_id: "P-1".to_string(),
        generation,
        agent_id: ORCHESTRATOR_AGENT.to_string(),
        granted_by: "cli".to_string(),
        granted_at: "t".to_string(),
    })
}

/// The model of the grant: the live generation, if any, and the highest generation given.
struct Model {
    live: Option<u64>,
    last: u64,
    /// The asks that were admitted (not replayed), with their records.
    settled: Vec<(SettleAsk, AdmissionRow)>,
    park_generation: [u32; 2],
    halted: bool,
}

fn open_request(env: &Env, unit: &str) -> Option<pf_engine::RequestRow> {
    env.store
        .all_requests()
        .into_iter()
        .find(|r| r.unit_id.as_deref() == Some(unit) && r.status == RequestStatus::Open)
}

fn park_generation(env: &Env, unit: &str) -> u32 {
    let view = env.engine.view().unwrap();
    view.unit_detail(view.node(unit).unwrap())
        .unwrap()
        .park_generation
}

fn ask(
    request: &pf_engine::RequestRow,
    redispatch: bool,
    reason: u8,
    generation: u64,
) -> SettleAsk {
    SettleAsk {
        request_id: request.id.clone(),
        revision: request.revision,
        act: if redispatch {
            Act::Redispatch
        } else {
            Act::Clear
        },
        to_stage: redispatch.then(|| "build".to_string()),
        facts: SettleFacts {
            reason: format!("reason {reason}"),
            session: "S-1".to_string(),
            project: "P-1".to_string(),
            generation,
            agent: ORCHESTRATOR_AGENT.to_string(),
            ..SettleFacts::default()
        },
    }
}

/// Run the ops and check the properties. It returns how many settles were admitted.
fn run_ops(ops: &[Op]) -> Result<usize, TestCaseError> {
    let env = rig();
    apply(&env, "grant-1", vec![grant_effect(1)]);
    let claim = env
        .engine
        .claim("op@host", &sha256_hex(b"a token"))
        .unwrap()
        .generation;
    let mut model = Model {
        live: Some(1),
        last: 1,
        settled: Vec::new(),
        park_generation: [0; 2],
        halted: false,
    };
    for (step, op) in ops.iter().enumerate() {
        match op {
            Op::Launch(u) => {
                let _ = env.engine.start_stage(UNITS[*u]);
            }
            Op::Park(u) => {
                let unit = UNITS[*u];
                let _ = env.engine.start_stage(unit);
                let parked = env.engine.park_with(
                    unit,
                    RequestKind::Stall,
                    Cause::Stalled,
                    "stalled: no output",
                    None,
                );
                if parked.is_ok() {
                    let view = env.engine.view().unwrap();
                    if let Some(stage) = view.running_stage_of(unit) {
                        let id = stage.id.clone();
                        let _ = env.engine.end(&id, EndStatus::Failed, "stalled: no output");
                    }
                }
            }
            Op::Settle {
                unit,
                redispatch,
                reason,
                stale,
                old_grant,
            } => {
                let Some(request) = open_request(&env, UNITS[*unit]) else {
                    continue;
                };
                let generation = match (model.live, old_grant) {
                    (Some(g), false) => g,
                    (Some(g), true) => g + 1,
                    (None, _) => model.last,
                };
                let mut a = ask(&request, *redispatch, *reason, generation);
                if *stale {
                    a.revision = a.revision.saturating_sub(1);
                }
                let before = launches_so_far(&env.engine.view().unwrap()).so_far();
                let outcome = env.engine.settle(a.clone(), Some(FORGE));
                if let Ok(admission) = outcome
                    && !admission.replayed
                {
                    // A commit that carries the precondition applies only while its generation is
                    // live.
                    prop_assert_eq!(
                        model.live,
                        Some(generation),
                        "step {}: an admitted settle needs the live generation",
                        step
                    );
                    // The grant never passes the stored count.
                    prop_assert!(
                        u64::try_from(before).unwrap() < BOUND,
                        "step {}: a settle was admitted at {} launches so far",
                        step,
                        before
                    );
                    prop_assert!(
                        !model.halted,
                        "step {}: a halted run admitted a settle",
                        step
                    );
                    model.settled.push((a, admission.row));
                }
            }
            Op::Retry(n) => {
                if model.settled.is_empty() {
                    continue;
                }
                let (first, row) = &model.settled[*n % model.settled.len()];
                let mut retry = first.clone();
                retry.facts.reason = "another reason".to_string();
                retry.facts.session = "S-9".to_string();
                let again = env.engine.settle(retry, Some(FORGE));
                match again {
                    Ok(admission) => {
                        prop_assert!(admission.replayed, "step {}: a retry is a replay", step);
                        prop_assert_eq!(&admission.row, row, "step {}: the same record", step);
                    }
                    Err(e) => prop_assert!(false, "step {}: a retry was refused: {:?}", step, e),
                }
            }
            Op::ClaimClear(u) => {
                if let Some(request) = open_request(&env, UNITS[*u]) {
                    let _ = env.engine.clear_unit(UNITS[*u], None, claim, &request.id);
                }
            }
            Op::Halt => {
                if env.engine.halt("stop").is_ok() {
                    model.halted = true;
                }
            }
            Op::Revoke => {
                if let Some(g) = model.live.take() {
                    apply(
                        &env,
                        &format!("revoke-{g}"),
                        vec![Effect::RevokeClearGrant {
                            project_id: "P-1".to_string(),
                            generation: g,
                            revoked_by: "cli".to_string(),
                            revoked_at: "t".to_string(),
                        }],
                    );
                }
            }
            Op::Regrant => {
                if model.live.is_none() {
                    model.last += 1;
                    apply(
                        &env,
                        &format!("grant-{}", model.last),
                        vec![grant_effect(model.last)],
                    );
                    model.live = Some(model.last);
                }
            }
            Op::Revise(u) => {
                if let Some(request) = open_request(&env, UNITS[*u]) {
                    let body: serde_json::Value = serde_json::from_str(&request.body).unwrap();
                    let _ = env
                        .engine
                        .revise_request(&request.id, request.revision, &body);
                }
            }
        }
        // A unit's park generation only rises.
        for (i, unit) in UNITS.iter().enumerate() {
            let now = park_generation(&env, unit);
            prop_assert!(
                now >= model.park_generation[i],
                "step {}: the park generation of {} fell from {} to {}",
                step,
                unit,
                model.park_generation[i],
                now
            );
            model.park_generation[i] = now;
        }
    }

    // A settle never launches a unit twice: after an admitted settle, at most one admitted launch of
    // its unit comes before the unit parks again.
    let admissions = env.store.admissions("run-1").unwrap();
    for unit in UNITS {
        let mut armed = false;
        let mut launched = 0;
        for a in admissions
            .iter()
            .filter(|a| a.unit_id.as_deref() == Some(unit))
        {
            if a.outcome != AdmissionOutcome::Admitted {
                continue;
            }
            match a.kind.as_str() {
                "settle" => {
                    armed = true;
                    launched = 0;
                }
                "launch" if armed => {
                    launched += 1;
                    prop_assert!(launched <= 1, "{unit} launched twice after one settle");
                }
                "park" => armed = false,
                _ => {}
            }
        }
    }
    // The same command ID with the same content returns the same record, for every command.
    for a in &admissions {
        if let Some(stored) = env.store.admission("run-1", &a.command_id).unwrap() {
            prop_assert_eq!(&stored, a);
        }
    }
    // The stored answer of every admitted settle names the generation that was live when it was
    // admitted, and the record is the one the settle returned.
    for (a, row) in &model.settled {
        let answer = env
            .store
            .all_requests()
            .into_iter()
            .find(|r| r.id == a.request_id)
            .and_then(|r| r.answer)
            .expect("a settled request has an answer");
        let answer: serde_json::Value = serde_json::from_str(&answer).unwrap();
        prop_assert_eq!(answer["verb"].as_str(), Some("settle"));
        prop_assert_eq!(
            answer["grant"]["generation"].as_u64(),
            Some(a.facts.generation)
        );
        prop_assert_eq!(row.kind.as_str(), "settle");
    }
    Ok(model.settled.len())
}

/// The property test is not vacuous: a short sequence admits a settle, and the run's count bounds a
/// longer one.
#[test]
fn the_model_admits_settles_and_the_bound_stops_them() {
    let settle = |unit, redispatch| Op::Settle {
        unit,
        redispatch,
        reason: 2,
        stale: false,
        old_grant: false,
    };
    let admitted = run_ops(&[Op::Park(0), settle(0, false)]).unwrap();
    assert_eq!(admitted, 1);
    // Two units, each launched once (two launches so far), a bound of five: a unit that is settled
    // and launches again takes the third and the fourth, and the fifth is refused.
    let admitted = run_ops(&[
        Op::Park(0),
        Op::Park(1),
        settle(0, false),
        Op::Launch(0),
        Op::Park(0),
        settle(0, true),
        Op::Launch(0),
        Op::Park(0),
        settle(0, false),
        Op::Park(1),
        settle(1, false),
    ])
    .unwrap();
    assert!((3..=4).contains(&admitted), "{admitted}");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    #[test]
    fn any_order_of_settles_claims_launches_halts_grants_and_revokes_keeps_the_properties(
        ops in prop::collection::vec(op(), 1..28)
    ) {
        run_ops(&ops)?;
    }
}

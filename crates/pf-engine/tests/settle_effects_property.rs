//! AC-3 of `r2.s3.w3`: the property test of the acts with an effect (`run-state-admission`, property
//! test). For any order of `rerun_check`, re-attach and `resolve_conflict` settles, their retries,
//! results, restarts (the reconcile), halts, revokes, launches and parks over two units of one run:
//!
//! - the same command ID and content returns the same record;
//! - each settle has at most one `settle-result`, and a re-run or a rebind is called at most once per
//!   settle;
//! - the admitted `rerun_check` settles of one head never pass the limit;
//! - no unit with a pending settle launches or merges;
//! - the run's launches pass the armed count by no grant act.

mod common;

use std::collections::{BTreeMap, HashMap};

use common::*;
use pf_engine::admission::body_of_content;
use pf_engine::claim::{Act, SettleAsk, SettleFacts, SettleOutcome, pending_settle};
use pf_engine::launch::launches_so_far;
use pf_engine::ledger::FsLedger;
use pf_engine::port::agent::RebindOutcome;
use pf_engine::port::forge::{Checks, Mergeable, PrRef};
use pf_engine::port::store::{
    AdmissionOutcome, CONTROLLER_SCOPE, Commit, Committed, Effect, NewAdmission, NewClearGrant,
    ORCHESTRATOR_AGENT, RequestStatus, Store,
};
use pf_engine::port::unit_source::UnitRecord;
use pf_engine::unit::Signal;
use pf_engine::workflow::OnConflict;
use pf_engine::{CommandBody, Engine, MergeRule, Status};
use proptest::prelude::*;

const FORGE: (&str, &str) = ("synthetic/repo", "main");
const UNITS: [&str; 2] = ["u0", "u1"];
const BOUND: u64 = 7;
const LIMIT: u32 = 2;
const HEADS: [&str; 2] = [
    "1111111111111111111111111111111111111111",
    "2222222222222222222222222222222222222222",
];

#[derive(Clone, Debug)]
enum Op {
    /// A settle of the unit's open request.
    Settle {
        unit: usize,
        reason: u8,
        stale: bool,
    },
    /// A retry of the n-th admitted settle, with another reason.
    Retry(usize),
    /// The effect of the unit's pending settle.
    Effect(usize),
    /// A tick: launch the unit's next stage, or merge the next unit.
    Tick(usize),
    /// The controller stops and opens again: the reconcile.
    Restart,
    /// What the next rebind of the re-attached unit's host answers.
    Rebind(u8),
    /// The unit's PR goes red or conflicts again, on a head.
    Redden(usize),
    /// One whole turn on `u0`: settle its open request, run the effect, and let the merge step
    /// park the unit again. Three turns on one head pass the limit.
    Cycle,
    Halt,
    Revoke,
    Regrant,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => (0..2usize, 0..3u8, any::<bool>())
            .prop_map(|(unit, reason, stale)| Op::Settle { unit, reason, stale }),
        2 => (0..8usize).prop_map(Op::Retry),
        6 => (0..2usize).prop_map(Op::Effect),
        4 => (0..2usize).prop_map(Op::Tick),
        2 => Just(Op::Restart),
        2 => (0..3u8).prop_map(Op::Rebind),
        3 => (0..2usize).prop_map(Op::Redden),
        4 => Just(Op::Cycle),
        1 => Just(Op::Halt),
        1 => Just(Op::Revoke),
        2 => Just(Op::Regrant),
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

fn workflow() -> pf_engine::Workflow {
    let mut wf = item_workflow(&["build", "review"], true);
    let mut resolve = pf_engine::StageDef::new("resolve", "close-role", "model-a");
    resolve.entered_by_conflict = true;
    wf.tracks[0].stages.push(resolve);
    let def = wf.tracks[0].merge.as_mut().unwrap();
    def.rule = MergeRule::GreenChecksInDeclaredOrder;
    def.repo = "synthetic/repo".to_string();
    def.base = "spine".to_string();
    def.on_conflict = Some(OnConflict {
        stage: "resolve".to_string(),
        then: "review".to_string(),
    });
    def.guarded = vec!["docs/**".to_string()];
    wf
}

fn reopen(env: &Env) -> Engine {
    let engine = Engine::new(
        config(env.dir.path(), workflow()),
        env.store.clone(),
        env.agent.clone(),
        env.forge.clone(),
        counting_clock(),
        Box::new(FsLedger::new(env.dir.path().join("ledger.md"))),
    )
    .unwrap();
    engine.start_up().unwrap();
    engine
}

fn grant(generation: u64) -> Effect {
    Effect::GrantClear(NewClearGrant {
        project_id: "P-1".to_string(),
        generation,
        agent_id: ORCHESTRATOR_AGENT.to_string(),
        granted_by: "cli".to_string(),
        granted_at: "t".to_string(),
    })
}

fn pr_of(number: u64) -> PrRef {
    PrRef {
        repo: "synthetic/repo".to_string(),
        number,
    }
}

/// `u0` is parked at its merge on PR 7 (a red check, or a conflict when `conflict`), and `u1` is
/// parked by tier 1 as `host_unreachable`.
fn rig(conflict: bool) -> (Env, Engine) {
    let env = env_with_params(workflow(), BTreeMap::new());
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
    apply(&env, "grant-1", vec![grant(1)]);
    apply(
        &env,
        "bound",
        vec![Effect::SetRunLaunchBound {
            run_id: "run-1".to_string(),
            bound: BOUND,
        }],
    );
    // u0 to its merge.
    env.engine.start_stage("u0").unwrap();
    env.engine
        .signal(
            "u0",
            "build",
            Signal::DoneFile {
                path: "done".to_string(),
            },
            None,
        )
        .unwrap();
    env.engine.start_stage("u0").unwrap();
    env.engine
        .signal(
            "u0",
            "review",
            Signal::PrState {
                state: "open".to_string(),
            },
            Some(pr_of(7)),
        )
        .unwrap();
    let mut script = PrScript::green("work/u0", HEADS[0]);
    if conflict {
        script.mergeable = Mergeable::Conflict;
    } else {
        script.checks = Checks::Red;
    }
    env.forge.script(7, script);
    env.forge.script_failed(7, HEADS[0], &[("ci", None)]);
    env.engine.merge_next().unwrap();
    // u1: a launched seat whose host does not answer.
    env.engine.start_stage("u1").unwrap();
    env.agent.script_rebind(
        "proc-3",
        RebindOutcome::Unreachable {
            why: "no answer".to_string(),
        },
    );
    let engine = reopen(&env);
    (env, engine)
}

fn open_request(engine: &Engine, unit: &str) -> Option<pf_engine::RequestRow> {
    engine
        .view()
        .unwrap()
        .requests
        .into_iter()
        .find(|r| r.unit_id.as_deref() == Some(unit) && r.status == RequestStatus::Open)
}

fn act_for(unit: usize, conflict: bool) -> Act {
    match (unit, conflict) {
        (0, true) => Act::ResolveConflict,
        (0, false) => Act::RerunCheck,
        _ => Act::Resume,
    }
}

fn facts(reason: u8, generation: u64) -> SettleFacts {
    SettleFacts {
        reason: format!("reason {reason}"),
        session: "S-1".to_string(),
        project: "P-1".to_string(),
        generation,
        agent: ORCHESTRATOR_AGENT.to_string(),
        rerun_limit: Some(LIMIT),
        ..SettleFacts::default()
    }
}

fn rebinds(env: &Env) -> usize {
    env.agent
        .call_log()
        .iter()
        .filter(|c| c.starts_with("rebind:"))
        .count()
}

/// The admitted settles of the run, with their act and the request they name.
fn settles(engine: &Engine) -> Vec<(String, u32, Act)> {
    engine
        .view()
        .unwrap()
        .admissions
        .iter()
        .filter(|a| a.kind == "settle" && a.outcome == AdmissionOutcome::Admitted)
        .filter_map(|a| match body_of_content(&a.content)? {
            CommandBody::Settle {
                request_id,
                revision,
                act,
                ..
            } => Some((request_id, revision, act)),
            _ => None,
        })
        .collect()
}

fn check_invariants(
    env: &Env,
    engine: &Engine,
    seen_pending: &mut HashMap<String, (usize, usize, (String, usize))>,
) {
    let view = engine.view().unwrap();
    let all = settles(engine);
    // Each settle has at most one result, and only a settle with an effect has one.
    let results: Vec<&str> = view
        .admissions
        .iter()
        .filter(|a| a.kind == "settle_result")
        .map(|a| a.command_id.as_str())
        .collect();
    let mut sorted = results.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), results.len(), "a settle has two results");
    // A re-run is called at most once per rerun settle.
    let reruns = all.iter().filter(|(_, _, a)| *a == Act::RerunCheck).count();
    assert!(env.forge.reruns().len() <= reruns, "a re-run ran twice");
    // The admitted rerun settles of one head never pass the limit.
    for head in HEADS {
        let used = view
            .requests
            .iter()
            .filter(|r| r.status == RequestStatus::Answered)
            .filter_map(|r| serde_json::from_str::<serde_json::Value>(r.answer.as_deref()?).ok())
            .filter(|a| a["verb"] == "settle" && a["act"] == "rerun_check" && a["head_sha"] == head)
            .count();
        assert!(used <= LIMIT as usize, "{used} re-runs for {head}");
    }
    // A unit with a pending settle neither launches nor merges.
    let launches_of = |unit: &str| {
        view.admissions
            .iter()
            .filter(|a| a.kind == "launch" && a.unit_id.as_deref() == Some(unit))
            .count()
    };
    let merges = env.forge.merged().len();
    // What a merge step or a launch would change without a landing or a launch: the unit's status
    // and its requests (a merge step that finds the checks red parks the unit again).
    let state_of = |unit: &str| {
        let status = view
            .node(unit)
            .map_or("none", |n| n.status.as_str())
            .to_string();
        let requests = view
            .requests
            .iter()
            .filter(|r| r.unit_id.as_deref() == Some(unit))
            .count();
        (status, requests)
    };
    let mut still = Vec::new();
    for unit in UNITS {
        if let Some(p) = pending_settle(&view, unit) {
            let key = format!("{unit}:{}", p.name());
            let now = (
                launches_of(unit),
                if unit == "u0" { merges } else { 0 },
                state_of(unit),
            );
            let first = seen_pending
                .entry(key.clone())
                .or_insert_with(|| now.clone())
                .clone();
            assert_eq!(first, now, "{unit} moved while {} was pending", p.name());
            still.push(key);
        }
    }
    seen_pending.retain(|k, _| still.contains(k));
}

/// One settle of the unit's open request. It returns the ask when the settle was admitted new.
fn settle_op(
    engine: &Engine,
    unit: usize,
    reason: u8,
    stale: bool,
    conflict: bool,
    generation: u64,
) -> Option<SettleAsk> {
    let request = open_request(engine, UNITS[unit])?;
    let act = act_for(unit, conflict);
    let revision = if stale && request.revision > 1 {
        request.revision - 1
    } else {
        request.revision
    };
    let ask = SettleAsk {
        request_id: request.id.clone(),
        revision,
        act,
        to_stage: None,
        facts: facts(reason, generation),
    };
    let launches_before = launches_so_far(&engine.view().unwrap()).so_far();
    match engine.settle(ask.clone(), Some(FORGE)) {
        Ok(admission) if !admission.replayed => {
            if act.launches() {
                let after = launches_so_far(&engine.view().unwrap()).so_far();
                assert!(after <= BOUND as usize, "{after} launches pass {BOUND}");
                assert!(launches_before < BOUND as usize);
            }
            Some(ask)
        }
        Ok(_) | Err(_) => None,
    }
}

/// The effect of the unit's pending settle, run twice: the second run is the replay.
fn effect_op(env: &Env, engine: &Engine, unit: usize, before_rebinds: usize) {
    if let Some(p) = engine.pending_settle_of(UNITS[unit]).unwrap() {
        let first = engine.settle_effect(&p.request_id, p.revision).unwrap();
        assert!(!first.replayed);
        let calls = (env.forge.reruns().len(), rebinds(env));
        let second = engine.settle_effect(&p.request_id, p.revision).unwrap();
        assert!(second.replayed);
        assert_eq!(first.row.seq, second.row.seq);
        assert_eq!(calls, (env.forge.reruns().len(), rebinds(env)));
        assert!(rebinds(env) - before_rebinds <= 1);
    }
}

fn run_case(conflict: bool, ops: &[Op]) {
    let (env, mut engine) = rig(conflict);
    let mut generation = 1u64;
    let mut live = true;
    let mut seen_pending: HashMap<String, (usize, usize, (String, usize))> = HashMap::new();
    let mut sent: Vec<SettleAsk> = Vec::new();
    for op in ops {
        let before_rebinds = rebinds(&env);
        match op {
            Op::Settle {
                unit,
                reason,
                stale,
            } => {
                if let Some(ask) = settle_op(&engine, *unit, *reason, *stale, conflict, generation)
                {
                    sent.push(ask);
                }
            }
            Op::Retry(n) => {
                if sent.is_empty() {
                    continue;
                }
                let mut ask = sent[n % sent.len()].clone();
                ask.facts.reason = "another reason".to_string();
                let first = engine
                    .view()
                    .unwrap()
                    .admissions
                    .into_iter()
                    .find(|a| {
                        a.command_id
                            == pf_engine::claim::settle_command_id(&ask.request_id, ask.revision)
                    })
                    .unwrap();
                let count = engine.view().unwrap().admissions.len();
                let again = engine.settle(ask, Some(FORGE));
                match again {
                    Ok(a) => {
                        assert!(a.replayed, "a retry is a replay");
                        assert_eq!(a.row.seq, first.seq);
                        assert_eq!(a.row.content, first.content);
                    }
                    Err(e) => panic!("a retry of an admitted settle is refused: {e}"),
                }
                assert_eq!(engine.view().unwrap().admissions.len(), count);
            }
            Op::Effect(unit) => effect_op(&env, &engine, *unit, before_rebinds),
            Op::Tick(unit) => {
                let name = UNITS[*unit];
                let _ = engine.start_stage(name);
                let _ = engine.merge_next();
            }
            Op::Cycle => {
                if let Some(ask) = settle_op(&engine, 0, 0, false, conflict, generation) {
                    sent.push(ask);
                }
                effect_op(&env, &engine, 0, before_rebinds);
                let _ = engine.merge_next();
            }
            Op::Restart => {
                engine = reopen(&env);
            }
            Op::Rebind(answer) => {
                let outcome = match answer {
                    0 => RebindOutcome::Survived,
                    1 => RebindOutcome::Lost {
                        cause: "gone".to_string(),
                    },
                    _ => RebindOutcome::Unreachable {
                        why: "no answer".to_string(),
                    },
                };
                env.agent.script_rebind("proc-3", outcome);
            }
            Op::Redden(unit) => {
                if *unit == 0 {
                    let mut scripts = env.forge.scripts.lock().unwrap();
                    if let Some(script) = scripts.get_mut(&7) {
                        script.sha = HEADS[(generation as usize) % 2].to_string();
                        if conflict {
                            script.mergeable = Mergeable::Conflict;
                        } else {
                            script.checks = Checks::Red;
                        }
                        let head = script.sha.clone();
                        drop(scripts);
                        env.forge.script_failed(7, &head, &[("ci", None)]);
                    }
                }
            }
            Op::Halt => {
                engine.halt("operator stop").unwrap();
            }
            Op::Revoke => {
                if live {
                    apply(
                        &env,
                        &format!("revoke-{generation}"),
                        vec![Effect::RevokeClearGrant {
                            project_id: "P-1".to_string(),
                            generation,
                            revoked_by: "cli".to_string(),
                            revoked_at: "t".to_string(),
                        }],
                    );
                    live = false;
                }
            }
            Op::Regrant => {
                if !live {
                    generation += 1;
                    apply(
                        &env,
                        &format!("grant-{generation}"),
                        vec![grant(generation)],
                    );
                    live = true;
                }
            }
        }
        if !matches!(op, Op::Effect(_) | Op::Restart | Op::Cycle) {
            assert_eq!(
                rebinds(&env),
                before_rebinds,
                "{op:?} called a rebind outside an effect or a restart"
            );
        }
        check_invariants(&env, &engine, &mut seen_pending);
    }
    // After the last op, every pending settle closes with one result.
    drop(engine);
    let engine = reopen(&env);
    let view = engine.view().unwrap();
    for unit in UNITS {
        assert!(
            pending_settle(&view, unit).is_none(),
            "{unit} is pending after a restart"
        );
    }
    for unit in UNITS {
        let node = env.store.node("run-1", unit).unwrap();
        assert!(
            matches!(
                node.status,
                Status::Running | Status::Parked | Status::Done | Status::Failed | Status::Stopped
            ),
            "{unit} is {:?}",
            node.status
        );
    }
    let _ = SettleOutcome::Ran;
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 40, ..ProptestConfig::default() })]

    #[test]
    fn the_acts_with_an_effect_keep_the_run_state_rules(
        conflict in any::<bool>(),
        ops in proptest::collection::vec(op(), 1..28),
    ) {
        run_case(conflict, &ops);
    }
}

/// Three turns on one head pass the limit: the third settle is refused, and the count stays at the
/// limit. The property reaches this by chance only, so this test walks it.
#[test]
fn a_head_that_goes_red_again_and_again_is_re_run_the_limit_times_and_no_more() {
    let (env, engine) = rig(false);
    let mut seen_pending = HashMap::new();
    let mut admitted = 0usize;
    for turn in 1..=LIMIT + 2 {
        let request = open_request(&engine, "u0").expect("the unit is parked again each turn");
        let ask = SettleAsk {
            request_id: request.id.clone(),
            revision: request.revision,
            act: Act::RerunCheck,
            to_stage: None,
            facts: facts(0, 1),
        };
        match engine.settle(ask, Some(FORGE)) {
            Ok(admission) => {
                assert!(
                    turn <= LIMIT,
                    "turn {turn} was admitted, past the limit {LIMIT}"
                );
                assert!(!admission.replayed);
                admitted += 1;
                engine.settle_effect(&request.id, request.revision).unwrap();
                // The checks are red again on the same head.
                env.forge
                    .scripts
                    .lock()
                    .unwrap()
                    .get_mut(&7)
                    .unwrap()
                    .checks = Checks::Red;
                assert!(matches!(
                    engine.merge_next().unwrap(),
                    pf_engine::merge::MergeStep::Parked { .. }
                ));
            }
            Err(e) => {
                assert!(turn > LIMIT, "turn {turn} was refused: {e}");
                assert!(
                    e.to_string().contains("authority.rerun_checks_per_head"),
                    "{e}"
                );
            }
        }
        check_invariants(&env, &engine, &mut seen_pending);
    }
    assert_eq!(admitted, LIMIT as usize);
    assert_eq!(env.forge.reruns().len(), LIMIT as usize);
}

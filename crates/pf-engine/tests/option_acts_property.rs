//! AC-2 of `r2.s11.w2` (`run-state-admission`, property test): for any request with options and any
//! sequence of acts, a replay of a command ID with the same content returns the first record and
//! writes nothing; the same ID with another option is refused; and a unit resumes at most once for
//! each park generation, so an option act never resumes a unit twice.
//!
//! The shared cargo config turns proptest's failure persistence off. The config below does too, so
//! a run from another directory leaves no `*.proptest-regressions` file.

mod common;

use common::*;
use pf_engine::claim::{AnswerAct, sha256_hex};
use pf_engine::port::store::{Cause, RequestKind, RequestRow};
use pf_engine::{EndStatus, EngineError};
use proptest::prelude::*;
use serde_json::{Value, json};

/// The causes a unit park can name: the merge ack opens through its own command.
fn parkable() -> Vec<Cause> {
    Cause::ALL
        .into_iter()
        .filter(|c| *c != Cause::MergeWaitsAck)
        .collect()
}

fn kind_of(cause: Cause) -> RequestKind {
    match cause {
        Cause::StopFile => RequestKind::Stop,
        Cause::Stalled => RequestKind::Stall,
        Cause::SeatLost | Cause::HostUnreachable => RequestKind::SeatLost,
        Cause::MergeConflict | Cause::MergeChecksRed | Cause::MergeRefused | Cause::MergeError => {
            RequestKind::MergeFailed
        }
        _ => RequestKind::Park,
    }
}

fn facts_of(cause: Cause, options: &[(String, bool)]) -> Option<Value> {
    match cause {
        Cause::StopFile => Some(json!({
            "stop_text": "which way?",
            "stop_options": options
                .iter()
                .map(|(text, mark)| json!({ "text": text, "recommended": mark }))
                .collect::<Vec<_>>(),
        })),
        Cause::OutcomeCap => Some(json!({ "cap_to": "verify", "cap_max": 2 })),
        _ => None,
    }
}

/// One act of a generated sequence. `pick` chooses among the options the block holds; one past the
/// last is an ID the block does not hold.
#[derive(Clone, Debug)]
enum Act {
    AnswerOption { pick: usize },
    AnswerText { text: String, stage: bool },
    ClearOption { pick: usize },
    ClearTyped { stage: bool },
}

fn act() -> impl Strategy<Value = Act> {
    prop_oneof![
        (0usize..5).prop_map(|pick| Act::AnswerOption { pick }),
        ("[a-z ]{1,16}", any::<bool>()).prop_map(|(text, stage)| Act::AnswerText { text, stage }),
        (0usize..5).prop_map(|pick| Act::ClearOption { pick }),
        any::<bool>().prop_map(|stage| Act::ClearTyped { stage }),
    ]
}

fn item_env() -> Env {
    let env = env(item_workflow(&["implement", "verify"], true));
    env.engine
        .ingest(&mut FakeUnitSource {
            records: vec![unit("u1", "g1", 0)],
        })
        .unwrap();
    env.engine.start_stage("u1").unwrap();
    env
}

/// What the store holds: a replay or a refusal must leave all of it as it was.
fn state_of(env: &Env) -> (Vec<RequestRow>, usize, String, u32) {
    let view = env.engine.view().unwrap();
    let park_generation = view
        .unit_detail(view.node("u1").unwrap())
        .unwrap()
        .park_generation;
    (
        env.engine.requests().unwrap(),
        view.admissions.len(),
        env.ledger_text(),
        park_generation,
    )
}

fn config() -> ProptestConfig {
    ProptestConfig {
        cases: 48,
        failure_persistence: None,
        ..ProptestConfig::default()
    }
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn a_unit_resumes_at_most_once_and_a_replay_writes_nothing(
        which in 0usize..21,
        options in proptest::collection::vec(("[a-z ]{1,12}", any::<bool>()), 0..5),
        acts in proptest::collection::vec(act(), 1..7),
    ) {
        let causes = parkable();
        let cause = causes[which % causes.len()];
        let env = item_env();
        env.engine
            .park_with("u1", kind_of(cause), cause, "parked", facts_of(cause, &options).as_ref())
            .unwrap();
        env.engine.end("u1/implement#1", EndStatus::Failed, "ended").unwrap();
        let request = env.engine.requests().unwrap().remove(0);
        let block: Value = serde_json::from_str(&request.body).unwrap();
        let ids: Vec<String> = block["decision"]["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["id"].as_str().unwrap().to_string())
            .collect();
        let id_of = |pick: usize| ids.get(pick).cloned().unwrap_or_else(|| "zz".to_string());
        let holder = env.engine.claim("op", &sha256_hex(b"op")).unwrap().generation;

        let mut first_answer: Option<AnswerKept> = None;
        let mut successes = 0u32;
        for step in &acts {
            let before = state_of(&env);
            let result = match step {
                Act::AnswerOption { pick } => {
                    let id = id_of(*pick);
                    let r = env.engine.answer_act(
                        &request.id, request.revision, holder, &AnswerAct::Option(&id));
                    if let Ok(a) = &r {
                        if !a.replayed {
                            first_answer = Some(AnswerKept::Option(id));
                        } else {
                            // A replay of the same option writes nothing; the stored record is
                            // the first one.
                            prop_assert_eq!(state_of(&env), before.clone());
                        }
                    }
                    r.map(|a| !a.replayed)
                }
                Act::AnswerText { text, stage } => {
                    let to_stage = stage.then_some("verify");
                    let r = env.engine.answer_act(
                        &request.id, request.revision, holder,
                        &AnswerAct::Text { text, to_stage });
                    if let Ok(a) = &r {
                        if !a.replayed {
                            first_answer = Some(AnswerKept::Text(text.clone(), to_stage.is_some()));
                        } else {
                            prop_assert_eq!(state_of(&env), before.clone());
                        }
                    }
                    r.map(|a| !a.replayed)
                }
                Act::ClearOption { pick } => {
                    let id = id_of(*pick);
                    env.engine
                        .clear_unit_option("u1", &id, request.revision, holder, &request.id)
                        .map(|a| !a.replayed)
                }
                Act::ClearTyped { stage } => env
                    .engine
                    .clear_unit("u1", stage.then_some("verify"), holder, &request.id)
                    .map(|a| !a.replayed),
            };
            match result {
                Ok(true) => successes += 1,
                Ok(false) => {}
                // A refusal writes nothing.
                Err(EngineError::Rejected(_) | EngineError::Conflict { .. }) => {
                    prop_assert_eq!(state_of(&env), before);
                }
                Err(other) => prop_assert!(false, "unexpected error {other:?}"),
            }
        }
        // The unit resumed at most once for the one park generation it had.
        prop_assert!(successes <= 1, "{successes} acts resumed the unit");
        let after = state_of(&env);
        prop_assert_eq!(after.3, successes);

        // The same ID with another option or another text is refused, and nothing is written.
        if let Some(kept) = first_answer {
            let before = state_of(&env);
            let again = match &kept {
                AnswerKept::Option(id) => ids
                    .iter()
                    .find(|other| *other != id)
                    .map(|other| env.engine.answer_act(
                        &request.id, request.revision, holder, &AnswerAct::Option(other))),
                AnswerKept::Text(..) => ids.first().map(|other| env.engine.answer_act(
                    &request.id, request.revision, holder, &AnswerAct::Option(other))),
            };
            if let Some(again) = again {
                prop_assert!(
                    matches!(again, Err(EngineError::Conflict { .. })),
                    "{:?}", again
                );
            }
            prop_assert_eq!(state_of(&env), before);
        }
    }
}

/// What the first admitted answer was, to name another under its command ID.
#[derive(Clone, Debug)]
enum AnswerKept {
    Option(String),
    #[allow(dead_code)]
    Text(String, bool),
}

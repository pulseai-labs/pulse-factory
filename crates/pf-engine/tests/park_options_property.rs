//! AC-3 of `r2.s11.w4` (`claim-authority` and `git-effects`, property test): the fact `park_options`
//! is data that a park command carries. For any JSON value under it, and either hook fact, the block
//! holds at most four options, each effect is `answer` or `resume` and never `ack`, the IDs are
//! distinct, a block that holds a `w` option holds only options `by: "workflow"`, and a
//! recommendation names an option of the block. A replay of the park command with the same content
//! returns the first request, and the same command ID with other facts is refused.
//!
//! The shared cargo config turns proptest's failure persistence off. The config below does too.

mod common;

use common::*;
use pf_engine::claim::KEY_PARK_OPTIONS;
use pf_engine::port::store::{Cause, RequestKind};
use pf_engine::{Command, CommandBody, EngineError};
use proptest::prelude::*;
use serde_json::{Map, Value, json};

fn leaf() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|n| json!(n)),
        "[a-z ]{0,12}".prop_map(Value::String),
        Just(json!("ack")),
        Just(json!("verify")),
        Just(json!("approve")),
    ]
}

/// Any JSON value, a few levels deep.
fn any_json() -> impl Strategy<Value = Value> {
    leaf().prop_recursive(3, 24, 4, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            proptest::collection::vec(("[a-z]{1,6}", inner), 0..4)
                .prop_map(|pairs| Value::Object(pairs.into_iter().collect::<Map<_, _>>())),
        ]
    })
}

/// An object with the keys the builder reads and the keys an author might try, each with any leaf.
fn entry() -> impl Strategy<Value = Value> {
    proptest::collection::vec(
        (
            prop_oneof![
                Just("label"),
                Just("answer"),
                Just("to"),
                Just("recommended"),
                Just("verb"),
                Just("effect"),
                Just("id"),
                Just("by"),
                Just("text"),
            ],
            prop_oneof![leaf(), any_json()],
        ),
        0..6,
    )
    .prop_map(|pairs| {
        Value::Object(
            pairs
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect::<Map<_, _>>(),
        )
    })
}

/// The value under `park_options`: a list of entries, a list of anything, or anything.
fn options_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        proptest::collection::vec(entry(), 0..8).prop_map(Value::Array),
        proptest::collection::vec(any_json(), 0..6).prop_map(Value::Array),
        any_json(),
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

fn park_command(cause: Cause, kind: RequestKind, facts: &Value) -> Command {
    Command {
        id: "park:u1:0".to_string(),
        body: CommandBody::Park {
            unit_id: "u1".to_string(),
            reason: "parked".to_string(),
            request_kind: kind,
            cause: Some(cause),
            detail: Some(facts.clone()),
        },
    }
}

fn facts_of(options: Value, takes_text: bool) -> Value {
    json!({ "takes_text": takes_text, KEY_PARK_OPTIONS: options })
}

fn config() -> ProptestConfig {
    ProptestConfig {
        cases: 96,
        failure_persistence: None,
        ..ProptestConfig::default()
    }
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn any_value_under_park_options_gives_a_block_a_workflow_cannot_widen(
        value in options_value(),
        hook in any::<bool>(),
    ) {
        let env = item_env();
        let facts = facts_of(value, hook);
        env.engine
            .submit(park_command(Cause::ParkFile, RequestKind::RedGate, &facts))
            .unwrap();
        let row = env.engine.requests().unwrap().remove(0);
        let body: Value = serde_json::from_str(&row.body).unwrap();
        let block = &body["decision"];
        let options = block["options"].as_array().unwrap();

        prop_assert!(options.len() <= 4, "{}", block);
        let mut seen = std::collections::BTreeSet::new();
        for o in options {
            let verb = o["effect"]["verb"].as_str().unwrap_or("");
            prop_assert!(verb == "answer" || verb == "resume", "{}", o);
            let id = o["id"].as_str().unwrap().to_string();
            prop_assert!(seen.insert(id), "an ID is used twice: {}", block);
        }
        // Either every option is the workflow's, or none is (the controller's one `resume`).
        let workflow_ids = options
            .iter()
            .filter(|o| o["id"].as_str().is_some_and(|i| i.starts_with('w')))
            .count();
        if workflow_ids > 0 {
            for o in options {
                prop_assert_eq!(&o["by"], &json!("workflow"), "{}", block);
                prop_assert!(o["id"].as_str().unwrap().starts_with('w'), "{}", block);
            }
        } else {
            for o in options {
                prop_assert_eq!(&o["by"], &json!("controller"), "{}", block);
            }
        }
        if let Some(rec) = block.get("recommended") {
            let named = rec["option"].as_str().unwrap();
            prop_assert!(
                options.iter().any(|o| o["id"] == named),
                "the recommendation names no option: {}",
                block
            );
            prop_assert_eq!(&rec["by"], &json!("workflow"));
            prop_assert_eq!(&rec["why"], &json!("The workflow marked this option."));
        }
        // The decision key is the engine's: the fact never moves it.
        prop_assert_eq!(&body["decision"]["cause"], &json!("park_file"));
        prop_assert_eq!(&body["decision"]["asked_by"]["kind"], &json!("seat"));
    }

    #[test]
    fn a_replay_returns_the_first_request_and_other_facts_under_the_id_are_refused(
        value in options_value(),
        hook in any::<bool>(),
    ) {
        let env = item_env();
        let facts = facts_of(value, hook);
        let first = env
            .engine
            .submit(park_command(Cause::ParkFile, RequestKind::RedGate, &facts))
            .unwrap();
        prop_assert!(!first.replayed);
        let before = (env.engine.requests().unwrap(), env.ledger_text());

        let again = env
            .engine
            .submit(park_command(Cause::ParkFile, RequestKind::RedGate, &facts))
            .unwrap();
        prop_assert!(again.replayed);
        prop_assert_eq!(&again.row, &first.row);
        prop_assert_eq!(&(env.engine.requests().unwrap(), env.ledger_text()), &before);

        let mut other = facts.clone();
        other["x_other"] = json!("another fact");
        let conflict = env
            .engine
            .submit(park_command(Cause::ParkFile, RequestKind::RedGate, &other));
        prop_assert!(
            matches!(conflict, Err(EngineError::Conflict { .. })),
            "{:?}",
            conflict
        );
        prop_assert_eq!(&(env.engine.requests().unwrap(), env.ledger_text()), &before);
    }
}

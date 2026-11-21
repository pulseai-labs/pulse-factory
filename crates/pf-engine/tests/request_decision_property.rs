//! AC-2 of `r2.s11.w1` (`run-state-admission`, property test): the decision block rides in the
//! park command's inputs and is derived when the engine decides the command. So for any cause and
//! any facts, the same command ID and content returns the same stored request and writes nothing;
//! the same ID with another cause is refused; and a replay after the unit's PR or attempt changed
//! returns the first block unchanged.
//!
//! The shared cargo config turns proptest's failure persistence off. The config below does too, so
//! a run from another directory leaves no `*.proptest-regressions` file.

mod common;

use common::*;
use pf_engine::claim::sha256_hex;
use pf_engine::port::forge::PrRef;
use pf_engine::port::store::{Cause, RequestKind, RequestRow};
use pf_engine::unit::Signal;
use pf_engine::{Command, CommandBody, EndStatus, EngineError};
use proptest::prelude::*;
use serde_json::{Map, Value, json};

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

/// The facts of one generated park: a bounded mix of the keys the builder reads and keys it does
/// not. None of them is a key the engine owns.
#[derive(Clone, Debug)]
struct Facts {
    takes_text: Option<bool>,
    options: Vec<(String, bool)>,
    cap: Option<(String, u64)>,
    extra: Vec<(String, String)>,
}

impl Facts {
    fn to_json(&self) -> Value {
        let mut map = Map::new();
        if let Some(t) = self.takes_text {
            map.insert("takes_text".to_string(), json!(t));
        }
        if !self.options.is_empty() {
            let options: Vec<Value> = self
                .options
                .iter()
                .map(|(text, mark)| json!({ "text": text, "recommended": mark }))
                .collect();
            map.insert("stop_options".to_string(), Value::Array(options));
        }
        if let Some((to, max)) = &self.cap {
            map.insert("cap_to".to_string(), json!(to));
            map.insert("cap_max".to_string(), json!(max));
        }
        for (k, v) in &self.extra {
            map.insert(format!("x_{k}"), json!(v));
        }
        Value::Object(map)
    }
}

fn facts() -> impl Strategy<Value = Facts> {
    (
        proptest::option::of(any::<bool>()),
        proptest::collection::vec(("[a-z ]{1,20}", any::<bool>()), 0..6),
        proptest::option::of(("[a-z]{1,8}", 1u64..5)),
        proptest::collection::vec(("[a-z]{1,6}", "[a-z ]{0,12}"), 0..3),
    )
        .prop_map(|(takes_text, options, cap, extra)| Facts {
            takes_text,
            options,
            cap,
            extra,
        })
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

fn park_command(cause: Cause, facts: &Value) -> Command {
    Command {
        id: "park:u1:0".to_string(),
        body: CommandBody::Park {
            unit_id: "u1".to_string(),
            reason: "parked".to_string(),
            request_kind: kind_of(cause),
            cause: Some(cause),
            detail: Some(facts.clone()),
        },
    }
}

/// What the store holds, as numbers and text: a replay or a refusal must leave all of it as it was.
fn state_of(env: &Env) -> (Vec<RequestRow>, usize, String) {
    let view = env.engine.view().unwrap();
    (
        env.engine.requests().unwrap(),
        view.admissions.len(),
        env.ledger_text(),
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
    fn the_same_command_replays_and_another_cause_under_its_id_is_refused(
        which in 0usize..21,
        facts in facts(),
        other in 1usize..21,
    ) {
        let causes = parkable();
        let cause = causes[which % causes.len()];
        let another = causes[(which + other) % causes.len()];
        prop_assert_ne!(cause, another);
        let facts = facts.to_json();

        let env = item_env();
        let first = env.engine.submit(park_command(cause, &facts)).unwrap();
        prop_assert!(!first.replayed);
        let after_first = state_of(&env);
        prop_assert_eq!(after_first.0.len(), 1);
        let block = after_first.0[0].body.clone();

        // The same ID and content: the stored record, and nothing written.
        let again = env.engine.submit(park_command(cause, &facts)).unwrap();
        prop_assert!(again.replayed);
        prop_assert_eq!(&again.row, &first.row);
        prop_assert_eq!(state_of(&env), after_first.clone());

        // The same ID with another cause: a conflict, and nothing written.
        let conflict = env.engine.submit(park_command(another, &facts));
        prop_assert!(
            matches!(conflict, Err(EngineError::Conflict { .. })),
            "{:?}",
            conflict
        );
        prop_assert_eq!(state_of(&env), after_first);
        prop_assert_eq!(&env.engine.requests().unwrap()[0].body, &block);
    }

    #[test]
    fn a_replay_after_the_pr_or_the_attempt_changed_returns_the_first_block_unchanged(
        which in 0usize..21,
        facts in facts(),
    ) {
        let causes = parkable();
        let cause = causes[which % causes.len()];
        let facts = facts.to_json();

        let env = item_env();
        let first = env.engine.submit(park_command(cause, &facts)).unwrap();
        let stored = env.engine.requests().unwrap()[0].clone();
        let block = serde_json::from_str::<Value>(&stored.body).unwrap()["decision"].clone();
        prop_assert_eq!(block["attempt"].as_u64(), Some(1));
        prop_assert!(block["refs"].get("pr").is_none());

        // The unit resumes, launches the stage again (attempt 2) and records a PR.
        env.engine
            .end("u1/implement#1", EndStatus::Failed, "stalled")
            .unwrap();
        let holder = env
            .engine
            .claim("op", &sha256_hex(b"op"))
            .unwrap()
            .generation;
        env.engine
            .clear_unit("u1", None, holder, &stored.id)
            .unwrap();
        env.engine.start_stage("u1").unwrap();
        env.engine
            .signal(
                "u1",
                "implement",
                Signal::PrState { state: "open".to_string() },
                Some(PrRef { repo: "synthetic/repo".to_string(), number: 7 }),
            )
            .unwrap();
        let before = state_of(&env);

        let again = env.engine.submit(park_command(cause, &facts)).unwrap();
        prop_assert!(again.replayed);
        prop_assert_eq!(&again.row, &first.row);
        prop_assert_eq!(state_of(&env), before.clone());
        // The first request keeps the block it was opened with.
        let kept = before.0.iter().find(|r| r.id == stored.id).unwrap();
        prop_assert_eq!(&kept.body, &stored.body);
    }
}

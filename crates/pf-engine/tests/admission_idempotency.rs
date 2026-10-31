//! AC-3: with a fake store, for generated commands, the same ID and content returns the same
//! record; changed content under a used ID is a conflict; a repeated launch command admits no
//! second launch.

mod common;

use std::collections::HashMap;

use common::*;
use pf_engine::port::store::{AdmissionOutcome, Store};
use pf_engine::{Command, CommandBody, EngineError};
use proptest::prelude::*;

fn park(id: &str, unit: &str, reason: &str) -> Command {
    Command {
        id: id.to_string(),
        body: CommandBody::Park {
            unit_id: unit.to_string(),
            reason: reason.to_string(),
        },
    }
}

fn launch(id: &str, unit: &str) -> Command {
    Command {
        id: id.to_string(),
        body: CommandBody::Launch {
            unit_id: unit.to_string(),
            stage: "build".to_string(),
            role: "build-role".to_string(),
            model: "model-a".to_string(),
            attempt: 1,
        },
    }
}

fn env_with_units(n: usize) -> Env {
    let env = env(workflow(&["build"], false));
    let records = (1..=n)
        .map(|i| unit(&format!("u{i}"), "g1", u32::try_from(i).unwrap()))
        .collect();
    env.engine.ingest(&mut FakeUnitSource { records }).unwrap();
    env
}

#[test]
fn the_same_id_with_the_same_content_returns_the_stored_record() {
    let env = env_with_units(1);
    let first = env.engine.submit(park("p1", "u1", "stalled")).unwrap();
    assert!(!first.replayed);
    let again = env.engine.submit(park("p1", "u1", "stalled")).unwrap();
    assert!(again.replayed);
    assert_eq!(first.row, again.row);
    let parks = env
        .store
        .admissions("run-1")
        .unwrap()
        .into_iter()
        .filter(|a| a.kind == "park")
        .count();
    assert_eq!(parks, 1);
}

#[test]
fn changed_content_under_a_used_id_is_a_conflict() {
    let env = env_with_units(2);
    env.engine.submit(park("p1", "u1", "stalled")).unwrap();
    let err = env
        .engine
        .submit(park("p1", "u1", "a different reason"))
        .unwrap_err();
    assert_eq!(
        err,
        EngineError::Conflict {
            command_id: "p1".to_string()
        }
    );
    // The stored reason did not change.
    assert_eq!(env.status_of("u1").1.as_deref(), Some("stalled"));
    // Another unit under the used ID is a conflict too.
    let err = env.engine.submit(park("p1", "u2", "stalled")).unwrap_err();
    assert!(matches!(err, EngineError::Conflict { .. }));
}

#[test]
fn a_repeated_launch_command_admits_no_second_launch() {
    let env = env_with_units(1);
    let first = env.engine.submit(launch("l1", "u1")).unwrap();
    let again = env.engine.submit(launch("l1", "u1")).unwrap();
    assert!(again.replayed);
    assert_eq!(first.row, again.row);
    let admitted_launches = env
        .store
        .admissions("run-1")
        .unwrap()
        .into_iter()
        .filter(|a| a.kind == "launch" && a.outcome == AdmissionOutcome::Admitted)
        .count();
    assert_eq!(admitted_launches, 1);
    assert_eq!(env.store.sessions("run-1").unwrap().len(), 1);
    // A launch under a new ID, while the stage runs, is not a second launch either. This holds
    // for the same attempt and for the next one.
    for (id, attempt) in [("l2", 1), ("l3", 2)] {
        let mut command = launch(id, "u1");
        if let CommandBody::Launch { attempt: a, .. } = &mut command.body {
            *a = attempt;
        }
        let err = env.engine.submit(command).unwrap_err();
        assert!(matches!(err, EngineError::Rejected(_)), "{err:?}");
    }
    assert_eq!(env.store.sessions("run-1").unwrap().len(), 1);
}

#[derive(Clone, Debug)]
enum Gen {
    Park(usize, usize),
    Halt(usize),
    Launch(usize),
}

fn arb_op() -> impl Strategy<Value = (usize, Gen)> {
    (
        0..5usize,
        prop_oneof![
            (0..2usize, 0..3usize).prop_map(|(u, r)| Gen::Park(u, r)),
            (0..3usize).prop_map(Gen::Halt),
            (0..2usize).prop_map(Gen::Launch),
        ],
    )
}

fn build(id: usize, op: &Gen) -> Command {
    let id = format!("c{id}");
    match op {
        Gen::Park(u, r) => park(&id, &format!("u{}", u + 1), &format!("reason {r}")),
        Gen::Halt(r) => Command {
            id,
            body: CommandBody::Halt {
                reason: format!("halt {r}"),
            },
        },
        Gen::Launch(u) => launch(&id, &format!("u{}", u + 1)),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn generated_commands_keep_the_admission_rules(ops in proptest::collection::vec(arb_op(), 1..14)) {
        let env = env_with_units(2);
        // command ID -> (canonical body text, stored receipt)
        let mut seen: HashMap<String, (String, pf_engine::port::store::AdmissionRow)> = HashMap::new();
        for (id, op) in &ops {
            let command = build(*id, op);
            let body = serde_json::to_string(&command.body).unwrap();
            let result = env.engine.submit(command.clone());
            match seen.get(&command.id) {
                Some((first_body, first_row)) if *first_body == body => {
                    let again = result.expect("a repeat of an admitted command succeeds");
                    prop_assert!(again.replayed);
                    prop_assert_eq!(&again.row, first_row);
                }
                Some(_) => {
                    prop_assert_eq!(
                        result.unwrap_err(),
                        EngineError::Conflict { command_id: command.id.clone() }
                    );
                }
                None => match result {
                    Ok(adm) => {
                        prop_assert!(!adm.replayed);
                        seen.insert(command.id.clone(), (body, adm.row));
                    }
                    Err(EngineError::Rejected(_)) => {}
                    Err(other) => prop_assert!(false, "unexpected error {:?}", other),
                },
            }
        }
        // One receipt per accepted command ID, and no duplicate launch.
        let admissions = env.store.admissions("run-1").unwrap();
        let mut ids: Vec<&str> = admissions.iter().map(|a| a.command_id.as_str()).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        prop_assert_eq!(before, ids.len());
        for u in ["u1", "u2"] {
            let launched = admissions
                .iter()
                .filter(|a| a.kind == "launch"
                    && a.outcome == AdmissionOutcome::Admitted
                    && a.unit_id.as_deref() == Some(u))
                .count();
            prop_assert!(launched <= 1, "unit {u} launched {launched} times");
        }
    }
}

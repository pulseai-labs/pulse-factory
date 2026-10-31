//! AC-9: a `running` unit whose session is gone is `parked` with the reason "seat lost across a
//! controller restart", and no launch is admitted. A `running` unit whose session is alive stays
//! `running`. Units already `done`, `failed`, `stopped` and `parked` keep their status and reason.

mod common;

use std::sync::Arc;

use common::*;
use pf_engine::ledger::FsLedger;
use pf_engine::port::store::{AdmissionOutcome, Store};
use pf_engine::unit::Signal;
use pf_engine::{Command, CommandBody, EndStatus, Engine, Status};

const LOST: &str = "seat lost across a controller restart";

/// The first controller leaves eight units in different states. Then it stops.
fn first_controller() -> Env {
    let env = env(workflow(&["build"], false));
    let ids = [
        "u-live",
        "u-gone",
        "u-done",
        "u-failed",
        "u-stopped",
        "u-parked",
        "u-idle",
        "u-unattached",
    ];
    let records = ids
        .iter()
        .enumerate()
        .map(|(i, id)| unit(id, "g1", u32::try_from(i).unwrap()))
        .collect();
    env.engine.ingest(&mut FakeUnitSource { records }).unwrap();

    for id in [
        "u-live",
        "u-gone",
        "u-done",
        "u-failed",
        "u-stopped",
        "u-parked",
    ] {
        env.engine.start_stage(id).unwrap();
    }
    env.engine
        .signal("u-done", "build", Signal::TurnEnd, None)
        .unwrap();
    env.engine
        .signal("u-failed", "build", Signal::ExitCode { code: 2 }, None)
        .unwrap();
    env.engine
        .end("u-stopped", EndStatus::Stopped, "operator stopped it")
        .unwrap();
    env.engine.park("u-parked", "stalled").unwrap();

    // u-unattached: the launch was admitted, and the controller stopped before the adapter
    // handed back a session handle. u-idle never launched.
    env.engine
        .submit(Command {
            id: "launch-unattached".to_string(),
            body: CommandBody::Launch {
                unit_id: "u-unattached".to_string(),
                stage: "build".to_string(),
                role: "build-role".to_string(),
                model: "model-a".to_string(),
                attempt: 1,
            },
        })
        .unwrap();
    env
}

fn second_controller(old: &Env) -> Engine {
    Engine::new(
        config(old.dir.path(), workflow(&["build"], false)),
        old.store.clone(),
        old.agent.clone(),
        old.forge.clone(),
        counting_clock(),
        Box::new(FsLedger::new(old.ledger_path())),
    )
    .unwrap()
}

fn node(env: &Env, id: &str) -> (Status, Option<String>) {
    env.status_of(id)
}

#[test]
fn at_start_up_a_running_unit_with_a_gone_session_is_parked_and_nothing_is_launched() {
    let old = first_controller();
    // The seat of u-gone died while the controller was down. The seat of u-live is still there.
    // Session handles are numbered in launch order: u-live is proc-1, u-gone is proc-2.
    old.agent.kill("proc-2");
    let launches_before = old.agent.launch_count();
    let launch_records = |env: &Env| {
        env.store
            .admissions("run-1")
            .unwrap()
            .into_iter()
            .filter(|a| a.kind == "launch" && a.outcome == AdmissionOutcome::Admitted)
            .count()
    };
    let launch_records_before = launch_records(&old);

    let second = second_controller(&old);
    second.start_up().unwrap();

    assert_eq!(
        node(&old, "u-gone"),
        (Status::Parked, Some(LOST.to_string()))
    );
    assert_eq!(
        node(&old, "u-unattached"),
        (Status::Parked, Some(LOST.to_string()))
    );
    // The lost stage ended `failed`, so no stage node claims to run.
    assert_eq!(node(&old, "u-gone/build#1").0, Status::Failed);

    // A live session stays running. A unit with no session is left alone.
    assert_eq!(node(&old, "u-live"), (Status::Running, None));
    assert_eq!(node(&old, "u-idle"), (Status::Running, None));
    assert_eq!(node(&old, "u-live/build#1"), (Status::Running, None));

    // Units that already ended keep their status and their reason.
    assert_eq!(
        node(&old, "u-done"),
        (Status::Done, Some("all stages done".to_string()))
    );
    assert_eq!(
        node(&old, "u-failed"),
        (
            Status::Failed,
            Some("stage build failed: exit code 2".to_string())
        )
    );
    assert_eq!(
        node(&old, "u-stopped"),
        (Status::Stopped, Some("operator stopped it".to_string()))
    );
    assert_eq!(
        node(&old, "u-parked"),
        (Status::Parked, Some("stalled".to_string()))
    );

    // Nothing relaunched, and no launch was admitted.
    assert_eq!(old.agent.launch_count(), launches_before);
    assert_eq!(launch_records(&old), launch_records_before);
}

#[test]
fn reconcile_reports_what_it_did_and_a_second_pass_changes_nothing() {
    let old = first_controller();
    old.agent.kill("proc-2");
    let second = second_controller(&old);
    second.open_run().unwrap();

    let report = second.reconcile().unwrap();
    assert_eq!(report.parked, vec!["u-gone", "u-unattached"]);
    assert_eq!(report.alive, vec!["u-live"]);

    let admissions_after_first = old.store.admissions("run-1").unwrap().len();
    let again = second.reconcile().unwrap();
    assert!(again.parked.is_empty());
    assert_eq!(again.alive, vec!["u-live"]);
    assert_eq!(
        old.store.admissions("run-1").unwrap().len(),
        admissions_after_first
    );
}

#[test]
fn a_failed_liveness_check_stops_the_start_up_and_parks_nothing() {
    struct BrokenAgent;
    impl pf_engine::port::agent::AgentPort for BrokenAgent {
        fn launch(
            &self,
            _: &pf_engine::port::agent::LaunchPlan,
        ) -> Result<pf_engine::port::agent::SessionRef, pf_engine::port::agent::AgentError>
        {
            unreachable!()
        }
        fn status(
            &self,
            _: &pf_engine::port::agent::SessionRef,
        ) -> Result<pf_engine::port::agent::SessionStatus, pf_engine::port::agent::AgentError>
        {
            unreachable!()
        }
        fn deliver(
            &self,
            _: &pf_engine::port::agent::SessionRef,
            _: &str,
        ) -> Result<(), pf_engine::port::agent::AgentError> {
            unreachable!()
        }
        fn interrupt(
            &self,
            _: &pf_engine::port::agent::SessionRef,
        ) -> Result<(), pf_engine::port::agent::AgentError> {
            unreachable!()
        }
        fn usage(
            &self,
            _: &pf_engine::port::agent::SessionRef,
        ) -> Result<pf_engine::port::agent::Usage, pf_engine::port::agent::AgentError> {
            unreachable!()
        }
        fn answer_dialog(
            &self,
            _: &pf_engine::port::agent::SessionRef,
            _: &pf_engine::port::agent::Dialog,
        ) -> Result<pf_engine::port::agent::DialogAnswer, pf_engine::port::agent::AgentError>
        {
            unreachable!()
        }
        fn apply_rules(
            &self,
            _: &mut pf_engine::port::agent::LaunchPlan,
            _: &pf_engine::port::agent::RuleSet,
        ) -> Result<(), pf_engine::port::agent::AgentError> {
            unreachable!()
        }
        fn remove_ask_tool(
            &self,
            _: &mut pf_engine::port::agent::LaunchPlan,
        ) -> Result<(), pf_engine::port::agent::AgentError> {
            unreachable!()
        }
        fn hand_off(
            &self,
            _: &pf_engine::port::agent::SessionRef,
            _: &pf_engine::port::agent::LaunchPlan,
        ) -> Result<pf_engine::port::agent::SessionRef, pf_engine::port::agent::AgentError>
        {
            unreachable!()
        }
        fn is_alive(
            &self,
            _: &pf_engine::port::agent::SessionRef,
        ) -> Result<bool, pf_engine::port::agent::AgentError> {
            Err(pf_engine::port::agent::AgentError::Failed(
                "socket closed".to_string(),
            ))
        }
    }
    let old = first_controller();
    let second = Engine::new(
        config(old.dir.path(), workflow(&["build"], false)),
        old.store.clone(),
        Arc::new(BrokenAgent),
        old.forge.clone(),
        counting_clock(),
        Box::new(FsLedger::new(old.ledger_path())),
    )
    .unwrap();
    let err = second.start_up().unwrap_err();
    assert!(err.to_string().contains("socket closed"), "{err}");
    assert_eq!(node(&old, "u-live"), (Status::Running, None));
}

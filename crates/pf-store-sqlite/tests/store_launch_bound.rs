//! The run's armed launch count on the store port, on the real SQLite store (ADR-0003, amended;
//! `r2.s3.w2`): `Effect::SetRunLaunchBound` under `@controller` sets it once, the same value again
//! admits and changes nothing, another value is `Rejected`, and `PutRun` keeps it.

mod common;

use common::*;
use pf_engine::port::store::{AdmissionOutcome, Commit, Effect, RunRow, RunState, Store};
use pf_store_sqlite::SqliteStore;
use rusqlite::Connection;

fn run_row(id: &str) -> RunRow {
    RunRow {
        id: id.to_string(),
        workflow: "wf".to_string(),
        ledger_file: "/l.md".to_string(),
        state: RunState::Active,
        halted_reason: None,
        project_id: None,
        launch_bound: None,
        created_at: "t1".to_string(),
        updated_at: "t1".to_string(),
    }
}

fn open_run(store: &SqliteStore, id: &str) {
    let mut c = commit(
        "open_run",
        &format!("open-run:{id}"),
        "{}",
        AdmissionOutcome::Admitted,
        None,
        None,
        vec![Effect::PutRun(run_row(id))],
        None,
    );
    c.run_id = id.to_string();
    applied(store.commit(c).unwrap());
}

fn bound_commit(command_id: &str, run: &str, bound: u64, scope: &str) -> Commit {
    let mut c = commit(
        "run_launch_bound",
        command_id,
        "{}",
        AdmissionOutcome::Admitted,
        None,
        None,
        vec![Effect::SetRunLaunchBound {
            run_id: run.to_string(),
            bound,
        }],
        Some(format!("run_launch_bound {run} most_launches={bound}")),
    );
    c.run_id = scope.to_string();
    c
}

fn bound_of(store: &SqliteStore, run: &str) -> Option<u64> {
    store.run(run).unwrap().unwrap().launch_bound
}

#[test]
fn the_effect_under_the_controller_sets_the_count_once() {
    let (_dir, store) = open();
    open_run(&store, "run-1");
    assert_eq!(bound_of(&store, "run-1"), None);
    applied(
        store
            .commit(bound_commit("b1", "run-1", 14, "@controller"))
            .unwrap(),
    );
    assert_eq!(bound_of(&store, "run-1"), Some(14));
}

#[test]
fn the_same_value_again_admits_and_changes_nothing() {
    let (_dir, store) = open();
    open_run(&store, "run-1");
    applied(
        store
            .commit(bound_commit("b1", "run-1", 14, "@controller"))
            .unwrap(),
    );
    applied(
        store
            .commit(bound_commit("b2", "run-1", 14, "@controller"))
            .unwrap(),
    );
    assert_eq!(bound_of(&store, "run-1"), Some(14));
}

#[test]
fn another_value_is_rejected_and_writes_nothing() {
    let (_dir, store) = open();
    open_run(&store, "run-1");
    applied(
        store
            .commit(bound_commit("b1", "run-1", 14, "@controller"))
            .unwrap(),
    );
    let state = admission_state(&store);
    let why = rejected(
        store
            .commit(bound_commit("b2", "run-1", 15, "@controller"))
            .unwrap(),
    );
    assert!(why.contains("run-1") && why.contains("14"), "{why}");
    assert_eq!(bound_of(&store, "run-1"), Some(14));
    assert_eq!(
        admission_state(&store),
        state,
        "no admission, no outbox line"
    );
    assert!(store.admission("@controller", "b2").unwrap().is_none());
}

#[test]
fn an_unknown_run_and_a_run_scope_are_rejected() {
    let (_dir, store) = open();
    open_run(&store, "run-1");
    let why = rejected(
        store
            .commit(bound_commit("b1", "run-9", 3, "@controller"))
            .unwrap(),
    );
    assert!(why.contains("run-9") && why.contains("unknown"), "{why}");
    // The effect in a run's scope is rejected: only the controller sets the count.
    let why = rejected(
        store
            .commit(bound_commit("b2", "run-1", 3, "run-1"))
            .unwrap(),
    );
    assert!(why.contains("@controller"), "{why}");
    assert_eq!(bound_of(&store, "run-1"), None);
}

#[test]
fn an_update_through_a_second_connection_that_changes_a_set_value_fails_in_the_database() {
    let (dir, store) = open();
    open_run(&store, "run-1");
    applied(
        store
            .commit(bound_commit("b1", "run-1", 14, "@controller"))
            .unwrap(),
    );
    let second = Connection::open(dir.path().join("store.sqlite")).unwrap();
    let err = second
        .execute("UPDATE runs SET launch_bound = 99 WHERE id = 'run-1'", [])
        .unwrap_err()
        .to_string();
    assert!(err.contains("a run keeps its launch bound"), "{err}");
    assert_eq!(bound_of(&store, "run-1"), Some(14));
}

#[test]
fn put_run_keeps_the_count() {
    let (_dir, store) = open();
    open_run(&store, "run-1");
    applied(
        store
            .commit(bound_commit("b1", "run-1", 14, "@controller"))
            .unwrap(),
    );
    // The engine's later PutRun (a halt) carries a row that holds no count: the store keeps it.
    let mut halted = run_row("run-1");
    halted.halted_reason = Some("stop".to_string());
    halted.updated_at = "t2".to_string();
    let mut c = commit(
        "halt",
        "halt:run-1",
        "{}",
        AdmissionOutcome::Admitted,
        None,
        None,
        vec![Effect::PutRun(halted)],
        None,
    );
    c.run_id = "run-1".to_string();
    applied(store.commit(c).unwrap());
    let run = store.run("run-1").unwrap().unwrap();
    assert_eq!(run.halted_reason.as_deref(), Some("stop"));
    assert_eq!(run.launch_bound, Some(14));
}

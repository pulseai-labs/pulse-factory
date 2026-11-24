//! The clear grant on the store port, on the real SQLite store (ADR-0003's amendment of
//! 2026-11-16; `r2.s3.w1`): `Effect::GrantClear`, `Effect::RevokeClearGrant`, the read
//! `Store::clear_grants`, and the precondition `Effect::RequireLiveGrant` that a commit in any
//! scope can carry. A commit whose effect breaks a rule is `Rejected` with a one-line reason, and
//! nothing is written: no admission, no outbox line and no other effect of the commit.

mod common;

use common::*;
use pf_engine::port::store::{Committed, ProjectKind, Store};
use pf_store_sqlite::SqliteStore;
use rusqlite::Connection;

/// A store with the live project `P-1`.
fn with_project() -> (tempfile::TempDir, SqliteStore) {
    let (dir, store) = open();
    let id = add_project(
        &store,
        "p1",
        "demo",
        "/work/demo",
        ProjectKind::Folder,
        None,
    );
    assert_eq!(id, "P-1");
    (dir, store)
}

fn second_connection(dir: &tempfile::TempDir) -> Connection {
    Connection::open(dir.path().join("store.sqlite")).unwrap()
}

fn lines(store: &SqliteStore) -> usize {
    store.unwritten_ledger_lines(LEDGER).unwrap().len()
}

fn live_of(store: &SqliteStore, project: &str) -> Vec<u64> {
    store
        .clear_grants()
        .unwrap()
        .into_iter()
        .filter(|g| g.project_id == project && g.revoked_at.is_none())
        .map(|g| g.generation)
        .collect()
}

#[test]
fn a_grant_writes_generation_one_and_its_ledger_line() {
    let (_d, store) = with_project();
    assert!(store.clear_grants().unwrap().is_empty(), "none yet");
    let (admission, ledger) = applied(store.commit(grant_commit("g1", "P-1", 1)).unwrap());
    assert_eq!(admission.kind, "grant_clear");
    assert_eq!(admission.run_id, "@controller");
    assert_eq!(admission.unit_id.as_deref(), Some("P-1:1"));
    assert_eq!(
        ledger.expect("a grant writes its ledger line").body,
        "grant_clear P-1 agent=orchestrator generation=1 by=cli command=g1"
    );
    let rows = store.clear_grants().unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.project_id, "P-1");
    assert_eq!(row.generation, 1);
    assert_eq!(row.agent_id, "orchestrator");
    assert_eq!(row.granted_by, "cli");
    assert_eq!(row.granted_at, "t5");
    assert_eq!(
        (row.revoked_by.clone(), row.revoked_at.clone()),
        (None, None)
    );
    assert_eq!(row.command_id, "g1");
}

#[test]
fn a_second_grant_while_the_first_is_live_is_rejected_and_writes_nothing() {
    let (_d, store) = with_project();
    applied(store.commit(grant_commit("g1", "P-1", 1)).unwrap());
    let before = (store.clear_grants().unwrap(), lines(&store));
    let why = rejected(store.commit(grant_commit("g2", "P-1", 2)).unwrap());
    assert!(!why.contains('\n') && why.contains("P-1"), "{why}");
    assert_eq!(store.clear_grants().unwrap(), before.0, "no row");
    assert_eq!(lines(&store), before.1, "no outbox line");
    assert_eq!(
        store.admission("@controller", "g2").unwrap(),
        None,
        "no admission"
    );
}

#[test]
fn a_revoke_sets_both_revoke_columns_once_and_a_second_revoke_is_rejected() {
    let (_d, store) = with_project();
    applied(store.commit(grant_commit("g1", "P-1", 1)).unwrap());
    let (admission, ledger) = applied(store.commit(revoke_commit("r1", "P-1", 1)).unwrap());
    assert_eq!(admission.kind, "grant_revoke");
    assert_eq!(
        ledger.unwrap().body,
        "grant_revoke P-1 generation=1 by=cli command=r1"
    );
    let row = store.clear_grants().unwrap().remove(0);
    assert_eq!(row.revoked_by.as_deref(), Some("cli"));
    assert_eq!(row.revoked_at.as_deref(), Some("t6"));
    let before = (store.clear_grants().unwrap(), lines(&store));
    let why = rejected(store.commit(revoke_commit("r2", "P-1", 1)).unwrap());
    assert!(!why.contains('\n'), "{why}");
    assert_eq!(
        store.clear_grants().unwrap(),
        before.0,
        "the row is as it was"
    );
    assert_eq!(lines(&store), before.1);
    assert_eq!(store.admission("@controller", "r2").unwrap(), None);
    // A generation that does not exist is rejected too.
    rejected(store.commit(revoke_commit("r3", "P-1", 7)).unwrap());
    rejected(store.commit(revoke_commit("r4", "P-9", 1)).unwrap());
}

#[test]
fn a_grant_after_a_revoke_is_generation_two() {
    let (_d, store) = with_project();
    applied(store.commit(grant_commit("g1", "P-1", 1)).unwrap());
    applied(store.commit(revoke_commit("r1", "P-1", 1)).unwrap());
    applied(store.commit(grant_commit("g2", "P-1", 2)).unwrap());
    assert_eq!(live_of(&store, "P-1"), vec![2]);
    let rows = store.clear_grants().unwrap();
    assert_eq!(rows.len(), 2, "the revoked row stays");
    assert_eq!(rows[0].generation, 1);
    assert_eq!(rows[1].generation, 2);
}

#[test]
fn a_generation_that_is_not_the_highest_plus_one_is_rejected() {
    let (_d, store) = with_project();
    // The first generation is 1, not 0 and not 2.
    for (i, wrong) in [0u64, 2, 5].into_iter().enumerate() {
        rejected(
            store
                .commit(grant_commit(&format!("w{i}"), "P-1", wrong))
                .unwrap(),
        );
    }
    assert!(store.clear_grants().unwrap().is_empty());
    applied(store.commit(grant_commit("g1", "P-1", 1)).unwrap());
    applied(store.commit(revoke_commit("r1", "P-1", 1)).unwrap());
    // After a revoke the next is 2: not 1 again, not 3.
    for (i, wrong) in [1u64, 3, 9].into_iter().enumerate() {
        rejected(
            store
                .commit(grant_commit(&format!("x{i}"), "P-1", wrong))
                .unwrap(),
        );
    }
    assert_eq!(store.clear_grants().unwrap().len(), 1);
}

#[test]
fn a_grant_for_an_unknown_or_a_removed_project_is_rejected() {
    let (_d, store) = with_project();
    let why = rejected(store.commit(grant_commit("g9", "P-9", 1)).unwrap());
    assert!(why.contains("P-9"), "{why}");
    applied(remove_project(&store, "rm", "P-1"));
    let why = rejected(store.commit(grant_commit("g1", "P-1", 1)).unwrap());
    assert!(why.contains("P-1"), "{why}");
    assert!(store.clear_grants().unwrap().is_empty());
}

#[test]
fn the_grant_effects_are_the_controllers_alone() {
    let (_d, store) = with_project();
    let mut grant = grant_commit("g1", "P-1", 1);
    grant.run_id = "run-1".to_string();
    rejected(store.commit(grant).unwrap());
    applied(store.commit(grant_commit("g2", "P-1", 1)).unwrap());
    let mut revoke = revoke_commit("r1", "P-1", 1);
    revoke.run_id = "run-1".to_string();
    rejected(store.commit(revoke).unwrap());
    assert_eq!(live_of(&store, "P-1"), vec![1]);
    assert_eq!(store.admission("run-1", "g1").unwrap(), None);
}

#[test]
fn the_database_refuses_what_the_port_would_not_write() {
    let (dir, store) = with_project();
    applied(store.commit(grant_commit("g1", "P-1", 1)).unwrap());
    let conn = second_connection(&dir);
    let refused = |sql: &str| {
        let err = conn.execute(sql, []).expect_err(sql);
        err.to_string()
    };
    // An update of any other column.
    for sql in [
        "UPDATE clear_grants SET agent_id = 'other'",
        "UPDATE clear_grants SET granted_by = 'chat'",
        "UPDATE clear_grants SET granted_at = 'later'",
        "UPDATE clear_grants SET command_id = 'c9'",
        "UPDATE clear_grants SET generation = 7",
        "UPDATE clear_grants SET project_id = 'P-2'",
    ] {
        refused(sql);
    }
    // A revoke sets both columns together.
    refused("UPDATE clear_grants SET revoked_by = 'cli'");
    refused("UPDATE clear_grants SET revoked_at = 't'");
    // A delete.
    refused("DELETE FROM clear_grants");
    // A second live row of the project, and a generation that is not the highest plus 1.
    refused(
        "INSERT INTO clear_grants (project_id, generation, agent_id, granted_by, granted_at, command_id) \
         VALUES ('P-1', 2, 'orchestrator', 'cli', 't', 'c2')",
    );
    refused(
        "INSERT INTO clear_grants (project_id, generation, agent_id, granted_by, granted_at, command_id) \
         VALUES ('P-1', 5, 'orchestrator', 'cli', 't', 'c3')",
    );
    // A row that is born revoked.
    refused(
        "INSERT INTO clear_grants (project_id, generation, agent_id, granted_by, granted_at, \
         revoked_by, revoked_at, command_id) VALUES ('P-2', 1, 'orchestrator', 'cli', 't', 'cli', 't', 'c4')",
    );
    // The one change: both columns together, once.
    conn.execute(
        "UPDATE clear_grants SET revoked_by = 'cli', revoked_at = 't9'",
        [],
    )
    .unwrap();
    refused("UPDATE clear_grants SET revoked_by = 'cli', revoked_at = 't10'");
    // INSERT OR REPLACE cannot rewrite a row by its key or by its command ID.
    refused(
        "INSERT OR REPLACE INTO clear_grants (project_id, generation, agent_id, granted_by, granted_at, command_id) \
         VALUES ('P-1', 1, 'orchestrator', 'cli', 't', 'c5')",
    );
    refused(
        "INSERT OR REPLACE INTO clear_grants (project_id, generation, agent_id, granted_by, granted_at, command_id) \
         VALUES ('P-1', 2, 'orchestrator', 'cli', 't', 'g1')",
    );
    let rows = store.clear_grants().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].revoked_at.as_deref(), Some("t9"));
    assert_eq!(rows[0].agent_id, "orchestrator");
}

#[test]
fn two_projects_hold_their_own_generations() {
    let (_d, store) = with_project();
    let second = add_project(
        &store,
        "p2",
        "other",
        "/work/other",
        ProjectKind::Folder,
        None,
    );
    assert_eq!(second, "P-2");
    applied(store.commit(grant_commit("g1", "P-1", 1)).unwrap());
    applied(store.commit(grant_commit("g2", "P-2", 1)).unwrap());
    applied(store.commit(revoke_commit("r1", "P-1", 1)).unwrap());
    applied(store.commit(grant_commit("g3", "P-1", 2)).unwrap());
    assert_eq!(live_of(&store, "P-1"), vec![2]);
    assert_eq!(live_of(&store, "P-2"), vec![1]);
    let all: Vec<(String, u64)> = store
        .clear_grants()
        .unwrap()
        .into_iter()
        .map(|g| (g.project_id, g.generation))
        .collect();
    assert_eq!(
        all,
        vec![
            ("P-1".to_string(), 1),
            ("P-1".to_string(), 2),
            ("P-2".to_string(), 1)
        ],
        "every row, by project and generation"
    );
}

// ---------------------------------------------------------------- the precondition (audit C6)

#[test]
fn a_commit_that_requires_the_live_generation_is_applied_with_its_other_effect() {
    let (_d, store) = with_project();
    applied(store.commit(grant_commit("g1", "P-1", 1)).unwrap());
    let (admission, _) = applied(store.commit(guarded_commit("s1", "P-1", 1)).unwrap());
    assert_eq!(admission.run_id, "run-1");
    assert!(
        store.run("run-1").unwrap().is_some(),
        "its other effect is written"
    );
    // The precondition writes nothing of its own: the grant rows are as they were.
    assert_eq!(store.clear_grants().unwrap().len(), 1);
}

/// The commit `s1` is rejected, and nothing of it is written.
fn assert_blocked(store: &SqliteStore, project: &str, generation: u64, why: &str) {
    let lines_before = lines(store);
    let grants_before = store.clear_grants().unwrap();
    let reason = rejected(
        store
            .commit(guarded_commit("s1", project, generation))
            .unwrap(),
    );
    assert!(
        !reason.contains('\n') && !reason.is_empty(),
        "{why}: {reason}"
    );
    assert_eq!(store.run("run-1").unwrap(), None, "{why}: its other effect");
    assert_eq!(
        store.admission("run-1", "s1").unwrap(),
        None,
        "{why}: no admission"
    );
    assert_eq!(lines(store), lines_before, "{why}: no outbox line");
    assert_eq!(store.clear_grants().unwrap(), grants_before, "{why}");
}

#[test]
fn a_commit_that_requires_a_grant_that_is_not_live_is_rejected_and_writes_nothing() {
    // A project with no grant.
    let (_d, store) = with_project();
    assert_blocked(&store, "P-1", 1, "no grant");
    // A generation that is not there yet.
    applied(store.commit(grant_commit("g1", "P-1", 1)).unwrap());
    assert_blocked(&store, "P-1", 2, "a generation that is not there yet");
    // An older generation.
    applied(store.commit(revoke_commit("r1", "P-1", 1)).unwrap());
    applied(store.commit(grant_commit("g2", "P-1", 2)).unwrap());
    assert_blocked(&store, "P-1", 1, "an older generation");
    // After the revoke.
    applied(store.commit(revoke_commit("r2", "P-1", 2)).unwrap());
    assert_blocked(&store, "P-1", 2, "after the revoke");
    // An unknown project.
    assert_blocked(&store, "P-9", 1, "an unknown project");
}

#[test]
fn a_removed_projects_grant_is_live_for_no_commit() {
    let (_d, store) = with_project();
    applied(store.commit(grant_commit("g1", "P-1", 1)).unwrap());
    applied(remove_project(&store, "rm", "P-1"));
    assert_blocked(&store, "P-1", 1, "a removed project");
    // The row stays, still open: the operator can still end it, and a revoke commits.
    assert_eq!(live_of(&store, "P-1"), vec![1]);
    applied(store.commit(revoke_commit("r1", "P-1", 1)).unwrap());
    assert!(live_of(&store, "P-1").is_empty());
}

#[test]
fn a_rejected_commit_can_be_committed_again_after_a_new_grant() {
    let (_d, store) = with_project();
    applied(store.commit(grant_commit("g1", "P-1", 1)).unwrap());
    applied(store.commit(revoke_commit("r1", "P-1", 1)).unwrap());
    // The settle waited for generation 2, which does not exist yet.
    assert_blocked(&store, "P-1", 2, "the grant was revoked");
    applied(store.commit(grant_commit("g2", "P-1", 2)).unwrap());
    // The same command ID and content, now that generation 2 is live.
    match store.commit(guarded_commit("s1", "P-1", 2)).unwrap() {
        Committed::Applied { .. } => {}
        other => panic!("not applied: {other:?}"),
    }
    assert!(store.run("run-1").unwrap().is_some());
    // And a replay of it is a replay, whatever the grant does after.
    applied(store.commit(revoke_commit("r2", "P-1", 2)).unwrap());
    match store.commit(guarded_commit("s1", "P-1", 2)).unwrap() {
        Committed::Replayed(_) => {}
        other => panic!("not replayed: {other:?}"),
    }
}

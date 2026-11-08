//! Projects on the store port, on the real SQLite store (ADR-0003). A project is a controller
//! entity: its commands commit under the scope `@controller`, and the store gives it the next
//! `P-<n>`, never reused.

use pf_engine::port::store::{
    AdmissionOutcome, CONTROLLER_SCOPE, Commit, Committed, Effect, LedgerEntry, NEW_ID,
    NewAdmission, NewProject, ProjectKind, Store,
};
use pf_store_sqlite::SqliteStore;

fn open() -> (tempfile::TempDir, SqliteStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("store.sqlite")).unwrap();
    (dir, store)
}

fn admission(kind: &str, outcome: AdmissionOutcome, reason: Option<&str>) -> NewAdmission {
    NewAdmission {
        kind: kind.to_string(),
        outcome,
        reason: reason.map(String::from),
        unit_id: None,
        role: None,
        model: None,
        attempt: None,
        admitted_at: "t1".to_string(),
    }
}

fn add_commit(command_id: &str, name: &str, root: &str) -> Commit {
    let mut a = admission("project_add", AdmissionOutcome::Admitted, None);
    a.unit_id = Some(NEW_ID.to_string());
    Commit {
        run_id: CONTROLLER_SCOPE.to_string(),
        command_id: command_id.to_string(),
        content: format!("{{\"name\":\"{name}\",\"root\":\"{root}\"}}"),
        admission: a,
        effects: vec![Effect::AddProject(NewProject {
            name: name.to_string(),
            root: root.to_string(),
            created_at: "t1".to_string(),
            kind: ProjectKind::Git,
            remote_key: None,
        })],
        ledger: Some(LedgerEntry {
            ledger_file: "controller.ledger".to_string(),
            body: format!("project_add {NEW_ID} admitted name={name} command={command_id}"),
        }),
    }
}

fn remove_commit(command_id: &str, id: &str) -> Commit {
    let mut a = admission("project_remove", AdmissionOutcome::Admitted, None);
    a.unit_id = Some(id.to_string());
    Commit {
        run_id: CONTROLLER_SCOPE.to_string(),
        command_id: command_id.to_string(),
        content: format!("{{\"id\":\"{id}\"}}"),
        admission: a,
        effects: vec![Effect::RemoveProject {
            id: id.to_string(),
            removed_at: "t2".to_string(),
        }],
        ledger: None,
    }
}

/// The id the store gave: the commit's admission names it in `unit_id`.
fn applied_id(c: Committed) -> String {
    match c {
        Committed::Applied { admission, .. } => admission.unit_id.expect("the assigned id"),
        other => panic!("not applied: {other:?}"),
    }
}

fn rejected(c: Committed) -> String {
    match c {
        Committed::Rejected(why) => why,
        other => panic!("not rejected: {other:?}"),
    }
}

fn admission_count(store: &SqliteStore) -> usize {
    store.admissions(CONTROLLER_SCOPE).unwrap().len()
}

#[test]
fn adds_give_p1_then_p2_in_order() {
    let (_d, store) = open();
    assert_eq!(
        applied_id(store.commit(add_commit("c1", "one", "/r/one")).unwrap()),
        "P-1"
    );
    assert_eq!(
        applied_id(store.commit(add_commit("c2", "two", "/r/two")).unwrap()),
        "P-2"
    );
    let live = store.projects().unwrap();
    let ids: Vec<&str> = live.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, ["P-1", "P-2"]);
    assert_eq!(live[0].name, "one");
    assert_eq!(live[0].root, "/r/one");
    assert_eq!(live[0].created_at, "t1");
    assert_eq!(live[0].removed_at, None);
}

#[test]
fn the_admission_and_the_ledger_line_name_the_assigned_id() {
    let (_d, store) = open();
    let c = store.commit(add_commit("c1", "one", "/r/one")).unwrap();
    let Committed::Applied { admission, ledger } = c else {
        panic!("not applied")
    };
    assert_eq!(admission.unit_id.as_deref(), Some("P-1"));
    assert_eq!(admission.run_id, CONTROLLER_SCOPE);
    let line = ledger.expect("a ledger line");
    assert_eq!(line.body, "project_add P-1 admitted name=one command=c1");
    assert!(!line.body.contains(NEW_ID));
    // The stored row of the admission and the outbox row hold the substituted text too.
    assert_eq!(
        store
            .admission(CONTROLLER_SCOPE, "c1")
            .unwrap()
            .unwrap()
            .unit_id
            .as_deref(),
        Some("P-1")
    );
    let unwritten = store.unwritten_ledger_lines("controller.ledger").unwrap();
    assert_eq!(unwritten.len(), 1);
    assert_eq!(unwritten[0].body, line.body);
    assert_eq!(unwritten[0].run_id, CONTROLLER_SCOPE);
}

#[test]
fn an_id_is_never_reused_after_a_remove() {
    let (_d, store) = open();
    store.commit(add_commit("c1", "one", "/r/one")).unwrap();
    store.commit(add_commit("c2", "two", "/r/two")).unwrap();
    store.commit(remove_commit("c3", "P-2")).unwrap();
    // The last row is removed, and the next add is still P-3.
    assert_eq!(
        applied_id(store.commit(add_commit("c4", "two", "/r/two")).unwrap()),
        "P-3"
    );
}

#[test]
fn projects_lists_live_rows_only_and_project_returns_any_row() {
    let (_d, store) = open();
    store.commit(add_commit("c1", "one", "/r/one")).unwrap();
    store.commit(add_commit("c2", "two", "/r/two")).unwrap();
    store.commit(add_commit("c3", "three", "/r/three")).unwrap();
    store.commit(remove_commit("c4", "P-2")).unwrap();

    let live: Vec<String> = store
        .projects()
        .unwrap()
        .into_iter()
        .map(|p| p.id)
        .collect();
    assert_eq!(live, ["P-1", "P-3"], "live rows, in creation order");

    let removed = store
        .project("P-2")
        .unwrap()
        .expect("the removed row stays");
    assert_eq!(removed.name, "two");
    assert_eq!(removed.removed_at.as_deref(), Some("t2"));
    assert_eq!(store.project("P-1").unwrap().unwrap().removed_at, None);
    assert_eq!(store.project("P-9").unwrap(), None);
}

#[test]
fn a_removed_root_and_name_can_be_added_again() {
    let (_d, store) = open();
    store.commit(add_commit("c1", "one", "/r/one")).unwrap();
    store.commit(remove_commit("c2", "P-1")).unwrap();
    assert_eq!(
        applied_id(store.commit(add_commit("c3", "one", "/r/one")).unwrap()),
        "P-2"
    );
    assert_eq!(store.projects().unwrap().len(), 1);
}

#[test]
fn a_duplicate_live_root_is_a_refusal_with_nothing_written() {
    let (_d, store) = open();
    store.commit(add_commit("c1", "one", "/r/one")).unwrap();
    let before = admission_count(&store);
    let why = rejected(store.commit(add_commit("c2", "other", "/r/one")).unwrap());
    assert!(why.contains("root"), "{why}");
    assert!(why.contains("/r/one"), "{why}");
    assert_eq!(admission_count(&store), before, "no admission was written");
    assert_eq!(store.admission(CONTROLLER_SCOPE, "c2").unwrap(), None);
    assert_eq!(
        store
            .unwritten_ledger_lines("controller.ledger")
            .unwrap()
            .len(),
        1
    );
    assert_eq!(store.projects().unwrap().len(), 1);
    // The refused add took no id: the next add is P-2.
    assert_eq!(
        applied_id(store.commit(add_commit("c3", "other", "/r/other")).unwrap()),
        "P-2"
    );
}

#[test]
fn a_duplicate_live_name_is_a_refusal_with_nothing_written() {
    let (_d, store) = open();
    store.commit(add_commit("c1", "one", "/r/one")).unwrap();
    let before = admission_count(&store);
    let why = rejected(store.commit(add_commit("c2", "one", "/r/two")).unwrap());
    assert!(why.contains("name"), "{why}");
    assert_eq!(admission_count(&store), before);
    assert_eq!(store.projects().unwrap().len(), 1);
}

#[test]
fn a_remove_of_an_unknown_or_removed_id_is_a_refusal() {
    let (_d, store) = open();
    store.commit(add_commit("c1", "one", "/r/one")).unwrap();
    let before = admission_count(&store);

    let unknown = rejected(store.commit(remove_commit("c2", "P-9")).unwrap());
    assert!(unknown.contains("P-9"), "{unknown}");

    store.commit(remove_commit("c3", "P-1")).unwrap();
    let again = rejected(store.commit(remove_commit("c4", "P-1")).unwrap());
    assert!(
        again.contains("P-1") && again.contains("removed"),
        "{again}"
    );

    assert_eq!(
        admission_count(&store),
        before + 1,
        "only the good remove wrote an admission"
    );
    assert_eq!(store.admission(CONTROLLER_SCOPE, "c2").unwrap(), None);
    assert_eq!(store.admission(CONTROLLER_SCOPE, "c4").unwrap(), None);
    // The first remove stands: the row is removed once, at its first time.
    assert_eq!(
        store.project("P-1").unwrap().unwrap().removed_at.as_deref(),
        Some("t2")
    );
}

#[test]
fn a_refused_command_is_an_admission_with_no_effect() {
    let (_d, store) = open();
    let mut a = admission(
        "project_add",
        AdmissionOutcome::Refused,
        Some("root is not a directory: /x"),
    );
    a.unit_id = None;
    let c = Commit {
        run_id: CONTROLLER_SCOPE.to_string(),
        command_id: "c1".to_string(),
        content: "{\"name\":\"x\",\"root\":\"/x\"}".to_string(),
        admission: a,
        effects: vec![],
        ledger: Some(LedgerEntry {
            ledger_file: "controller.ledger".to_string(),
            body: "project_add refused reason=\"root is not a directory: /x\" command=c1"
                .to_string(),
        }),
    };
    let Committed::Applied { admission, ledger } = store.commit(c.clone()).unwrap() else {
        panic!("not applied")
    };
    assert_eq!(admission.outcome, AdmissionOutcome::Refused);
    assert_eq!(
        admission.reason.as_deref(),
        Some("root is not a directory: /x")
    );
    assert!(ledger.is_some());
    assert!(store.projects().unwrap().is_empty());
    // The same command again is a replay, with the first record.
    let Committed::Replayed(row) = store.commit(c).unwrap() else {
        panic!("not a replay")
    };
    assert_eq!(row, admission);
}

#[test]
fn a_replay_returns_the_first_record_and_changed_content_is_a_conflict() {
    let (_d, store) = open();
    let first = match store.commit(add_commit("c1", "one", "/r/one")).unwrap() {
        Committed::Applied { admission, .. } => admission,
        other => panic!("{other:?}"),
    };
    match store.commit(add_commit("c1", "one", "/r/one")).unwrap() {
        Committed::Replayed(row) => assert_eq!(row, first),
        other => panic!("{other:?}"),
    }
    match store.commit(add_commit("c1", "one", "/r/else")).unwrap() {
        Committed::Conflict(row) => assert_eq!(row, first),
        other => panic!("{other:?}"),
    }
    assert_eq!(store.projects().unwrap().len(), 1);
    assert_eq!(
        store
            .unwritten_ledger_lines("controller.ledger")
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn a_commit_that_fails_late_leaves_no_project() {
    let (dir, store) = open();
    // A second connection makes the outbox insert fail: the add must roll back with it.
    let conn = rusqlite::Connection::open(dir.path().join("store.sqlite")).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER no_outbox BEFORE INSERT ON ledger_outbox \
         BEGIN SELECT RAISE(ABORT, 'outbox is closed'); END;",
    )
    .unwrap();
    let err = store.commit(add_commit("c1", "one", "/r/one")).unwrap_err();
    assert!(err.to_string().contains("outbox is closed"), "{err}");
    assert!(store.projects().unwrap().is_empty());
    assert_eq!(store.admission(CONTROLLER_SCOPE, "c1").unwrap(), None);
}

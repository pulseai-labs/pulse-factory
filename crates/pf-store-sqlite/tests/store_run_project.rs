//! A run's project and the start items on the store port, on the real SQLite store (ADR-0003,
//! ADR-0015; `r1.s4.w2`): `Effect::SetRunProject` under `@controller`, the engine's later `PutRun`s
//! keeping the project, and the `start_card` / `start_result` pair in the display log.

mod common;

use common::*;
use pf_engine::port::store::{
    AdmissionOutcome, ChatItemKind, Commit, Committed, Effect, ProjectKind, RunRow, RunState, Store,
};
use pf_store_sqlite::SqliteStore;

fn run_row(id: &str, state: RunState) -> RunRow {
    RunRow {
        id: id.to_string(),
        workflow: "wf".to_string(),
        ledger_file: "/l.md".to_string(),
        state,
        halted_reason: None,
        project_id: None,
        created_at: "t1".to_string(),
        updated_at: "t1".to_string(),
    }
}

/// Open a run in its own scope, as the engine does.
fn open_run(store: &SqliteStore, id: &str) {
    let mut c = commit(
        "open_run",
        &format!("open-run:{id}"),
        "{}",
        AdmissionOutcome::Admitted,
        None,
        None,
        vec![Effect::PutRun(run_row(id, RunState::Active))],
        None,
    );
    c.run_id = id.to_string();
    applied(store.commit(c).unwrap());
}

fn set_project_commit(command_id: &str, run: &str, project: &str, scope: &str) -> Commit {
    let mut c = commit(
        "run_project",
        command_id,
        "{}",
        AdmissionOutcome::Admitted,
        None,
        None,
        vec![Effect::SetRunProject {
            run_id: run.to_string(),
            project_id: project.to_string(),
        }],
        Some(format!("run_project {run} project={project} admitted")),
    );
    c.run_id = scope.to_string();
    c
}

fn two_projects_and_a_run() -> (tempfile::TempDir, SqliteStore) {
    let (d, store) = open();
    add_project(&store, "c1", "one", "/r/one", ProjectKind::Git, None);
    add_project(&store, "c2", "two", "/r/two", ProjectKind::Folder, None);
    open_run(&store, "run-1");
    (d, store)
}

#[test]
fn set_run_project_under_the_controller_sets_the_column_and_run_reads_it_back() {
    let (_d, store) = two_projects_and_a_run();
    assert_eq!(store.run("run-1").unwrap().unwrap().project_id, None);
    applied(
        store
            .commit(set_project_commit("rp-1", "run-1", "P-1", "@controller"))
            .unwrap(),
    );
    assert_eq!(
        store.run("run-1").unwrap().unwrap().project_id,
        Some("P-1".to_string())
    );
    // The same pair again is a no-op that admits, under its own command ID.
    applied(
        store
            .commit(set_project_commit("rp-2", "run-1", "P-1", "@controller"))
            .unwrap(),
    );
}

#[test]
fn a_second_set_naming_another_project_is_a_refused_admission_that_rolls_back_whole() {
    let (_d, store) = two_projects_and_a_run();
    applied(
        store
            .commit(set_project_commit("rp-1", "run-1", "P-1", "@controller"))
            .unwrap(),
    );
    let projects_before = store.projects().unwrap();
    let state = admission_state(&store);
    // The same admission also adds a project, so a partial write would show.
    let mut c = set_project_commit("rp-2", "run-1", "P-2", "@controller");
    c.effects.insert(
        0,
        Effect::AddProject(pf_engine::port::store::NewProject {
            name: "three".to_string(),
            root: "/r/three".to_string(),
            kind: ProjectKind::Folder,
            remote_key: None,
            created_at: "t3".to_string(),
        }),
    );
    let why = rejected(store.commit(c).unwrap());
    assert!(why.contains("keeps its project"), "{why}");
    assert_eq!(
        store.run("run-1").unwrap().unwrap().project_id,
        Some("P-1".to_string())
    );
    assert_eq!(store.projects().unwrap(), projects_before);
    assert_eq!(admission_state(&store), state, "nothing else was written");
}

#[test]
fn set_run_project_names_a_live_project_and_an_existing_run() {
    let (_d, store) = two_projects_and_a_run();
    applied(store.commit(remove_project_commit("rm-2", "P-2")).unwrap());
    let why = rejected(
        store
            .commit(set_project_commit("rp-1", "run-1", "P-2", "@controller"))
            .unwrap(),
    );
    assert!(why.contains("is removed"), "{why}");
    let why = rejected(
        store
            .commit(set_project_commit("rp-2", "run-1", "P-9", "@controller"))
            .unwrap(),
    );
    assert!(why.contains("project"), "{why}");
    let why = rejected(
        store
            .commit(set_project_commit("rp-3", "run-9", "P-1", "@controller"))
            .unwrap(),
    );
    assert!(why.contains("run-9"), "{why}");
    assert_eq!(store.run("run-1").unwrap().unwrap().project_id, None);
}

fn remove_project_commit(command_id: &str, id: &str) -> Commit {
    commit(
        "project_remove",
        command_id,
        "{}",
        AdmissionOutcome::Admitted,
        None,
        None,
        vec![Effect::RemoveProject {
            id: id.to_string(),
            removed_at: "t5".to_string(),
        }],
        None,
    )
}

#[test]
fn set_run_project_in_a_runs_own_scope_is_refused() {
    let (_d, store) = two_projects_and_a_run();
    let why = rejected(
        store
            .commit(set_project_commit("rp-1", "run-1", "P-1", "run-1"))
            .unwrap(),
    );
    assert!(why.contains("@controller"), "{why}");
    assert_eq!(store.run("run-1").unwrap().unwrap().project_id, None);
}

#[test]
fn the_engines_later_put_runs_keep_the_project() {
    let (_d, store) = two_projects_and_a_run();
    applied(
        store
            .commit(set_project_commit("rp-1", "run-1", "P-1", "@controller"))
            .unwrap(),
    );
    // A halt and an end, as the engine writes them: `PutRun` with a row that carries no project.
    for (n, state, reason) in [
        (1, RunState::Active, Some("stop")),
        (2, RunState::Ended, Some("stop")),
    ] {
        let mut row = run_row("run-1", state);
        row.halted_reason = reason.map(String::from);
        row.updated_at = format!("t{}", 10 + n);
        let mut c = commit(
            "halt",
            &format!("halt-{n}"),
            "{}",
            AdmissionOutcome::Admitted,
            None,
            None,
            vec![Effect::PutRun(row)],
            None,
        );
        c.run_id = "run-1".to_string();
        applied(store.commit(c).unwrap());
    }
    let run = store.run("run-1").unwrap().unwrap();
    assert_eq!(run.state, RunState::Ended);
    assert_eq!(run.project_id, Some("P-1".to_string()));
}

fn card_session() -> (tempfile::TempDir, SqliteStore) {
    let (d, store) = open();
    add_project(&store, "c1", "one", "/r/one", ProjectKind::Git, None);
    applied(start(&store, "s1", "P-1", record("/r/one")));
    (d, store)
}

#[test]
fn a_start_result_answers_a_start_card_once() {
    let (_d, store) = card_session();
    let msg = store
        .append_chat_item(item("S-1", ChatItemKind::OperatorMessage))
        .unwrap();
    let card = store
        .append_chat_item(item("S-1", ChatItemKind::StartCard))
        .unwrap();
    assert_eq!(card.kind, "start_card");
    assert_eq!(
        ChatItemKind::parse("start_card"),
        Some(ChatItemKind::StartCard)
    );
    assert_eq!(
        ChatItemKind::StartResult.answers(),
        Some(ChatItemKind::StartCard)
    );
    assert_eq!(
        ChatItemKind::StartCard.answered_by(),
        Some(ChatItemKind::StartResult)
    );
    assert!(!ChatItemKind::StartCard.is_request());
    assert!(!ChatItemKind::StartResult.is_request());

    // A card is not a pending request.
    assert!(store.pending_requests("S-1").unwrap().is_empty());

    let result = store
        .append_chat_item(answer(
            "S-1",
            ChatItemKind::StartResult,
            card.seq,
            "answered",
        ))
        .unwrap();
    assert_eq!(result.answers_seq, Some(card.seq));
    // A second result for the same card is refused.
    let err = store
        .append_chat_item(answer(
            "S-1",
            ChatItemKind::StartResult,
            card.seq,
            "withdrawn",
        ))
        .unwrap_err()
        .to_string();
    assert!(err.contains("answered already"), "{err}");
    // A result naming a non-card item, or no item, is refused.
    for (named, what) in [(msg.seq, "an operator message"), (99, "no item")] {
        let before = store.chat_items_after("S-1", 0, 100).unwrap().len();
        let err = store
            .append_chat_item(answer("S-1", ChatItemKind::StartResult, named, "answered"))
            .expect_err(what);
        assert!(
            err.to_string().contains("is no start_card"),
            "{what}: {err}"
        );
        assert_eq!(store.chat_items_after("S-1", 0, 100).unwrap().len(), before);
    }
    // A result that names no card at all is refused.
    let mut bare = item("S-1", ChatItemKind::StartResult);
    bare.payload = "{\"outcome\":\"answered\"}".to_string();
    assert!(store.append_chat_item(bare).is_err());
}

#[test]
fn a_start_result_outcome_is_answered_lost_or_withdrawn() {
    let (_d, store) = card_session();
    for outcome in ["answered", "lost", "withdrawn"] {
        let card = store
            .append_chat_item(item("S-1", ChatItemKind::StartCard))
            .unwrap();
        store
            .append_chat_item(answer("S-1", ChatItemKind::StartResult, card.seq, outcome))
            .unwrap();
    }
    let card = store
        .append_chat_item(item("S-1", ChatItemKind::StartCard))
        .unwrap();
    for bad in ["armed", "failed", ""] {
        let err = store
            .append_chat_item(answer("S-1", ChatItemKind::StartResult, card.seq, bad))
            .unwrap_err()
            .to_string();
        assert!(err.contains("outcome is one of"), "{bad}: {err}");
    }
    let _ = Committed::Rejected(String::new());
}

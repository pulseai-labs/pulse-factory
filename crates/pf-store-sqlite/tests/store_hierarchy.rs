//! The hierarchy on the store port, on the real SQLite store (ADR-0003, ADR-0015): the kind and
//! remote key of a project, the worktrees, the agents, and the chat sessions with their admission.
//! Each case checks what the commit wrote, and that a rejection writes nothing.

mod common;

use common::*;
use pf_engine::port::store::{
    AdmissionOutcome, AgentRow, CONTROLLER_SCOPE, ChatSessionRow, ChatSessionState, Committed,
    Effect, ProjectKind, ProjectRow, SessionWorktree, Store, WorktreeCreatedBy, WorktreeRow,
};

/// A commit of one effect, with no ledger line.
fn effect_commit(command_id: &str, kind: &str, effect: Effect) -> pf_engine::port::store::Commit {
    commit(
        kind,
        command_id,
        "{}",
        AdmissionOutcome::Admitted,
        None,
        None,
        vec![effect],
        Some(format!("{kind} admitted command={command_id}")),
    )
}

/// Everything that a command of the hierarchy can write: the admissions and the outbox, the agent
/// rows, and every project, worktree and chat session row of `P-1` to `P-12`, whole.
#[derive(Debug, PartialEq, Eq)]
struct Hierarchy {
    counts: (usize, usize),
    agents: Vec<AgentRow>,
    projects: Vec<(String, Option<ProjectRow>)>,
    worktrees: Vec<WorktreeRow>,
    sessions: Vec<ChatSessionRow>,
}

fn hierarchy(store: &pf_store_sqlite::SqliteStore) -> Hierarchy {
    let ids: Vec<String> = (1..=12).map(|n| format!("P-{n}")).collect();
    Hierarchy {
        counts: admission_state(store),
        agents: store.agents().unwrap(),
        projects: ids
            .iter()
            .map(|id| (id.clone(), store.project(id).unwrap()))
            .collect(),
        worktrees: ids
            .iter()
            .flat_map(|id| store.worktrees(id).unwrap())
            .collect(),
        sessions: ids
            .iter()
            .flat_map(|id| store.chat_sessions(id).unwrap())
            .collect(),
    }
}

/// Nothing was written: the same admissions, the same outbox and the same whole rows.
fn assert_unchanged(store: &pf_store_sqlite::SqliteStore, before: &Hierarchy) {
    assert_eq!(&hierarchy(store), before, "a rejection writes nothing");
}

#[test]
fn add_project_stores_the_kind_and_the_key() {
    let (_d, store) = open();
    add_project(
        &store,
        "c1",
        "one",
        "/r/one",
        ProjectKind::Git,
        Some("github.com/o/one"),
    );
    add_project(&store, "c2", "two", "/r/two", ProjectKind::Folder, None);
    let rows = store.projects().unwrap();
    assert_eq!(rows[0].kind, Some(ProjectKind::Git));
    assert_eq!(rows[0].remote_key.as_deref(), Some("github.com/o/one"));
    assert_eq!(rows[1].kind, Some(ProjectKind::Folder));
    assert_eq!(rows[1].remote_key, None);
    assert_eq!(store.project("P-1").unwrap().unwrap(), rows[0]);
}

#[test]
fn set_project_kind_sets_a_null_kind_once() {
    // A project that the migration left without a kind: make one by hand, as version 4 had it.
    let (dir, store) = open();
    let path = dir.path().join("store.sqlite");
    drop(store);
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("DROP TRIGGER projects_new_has_kind;")
            .unwrap();
        conn.execute_batch(
            "INSERT INTO projects (id, name, root, created_at) VALUES ('P-1', 'old', '/r/old', 't0'), \
                 ('P-2', 'gone', '/r/gone', 't0');
             UPDATE projects SET removed_at = 't1' WHERE id = 'P-2';",
        )
        .unwrap();
    }
    let store = pf_store_sqlite::SqliteStore::open(&path).unwrap();
    assert_eq!(store.project("P-1").unwrap().unwrap().kind, None);

    let set = |cmd: &str, id: &str, kind: ProjectKind, key: Option<&str>| {
        store
            .commit(effect_commit(
                cmd,
                "project_kind",
                Effect::SetProjectKind {
                    id: id.to_string(),
                    kind,
                    remote_key: key.map(String::from),
                },
            ))
            .unwrap()
    };
    applied(set("k1", "P-1", ProjectKind::Git, Some("github.com/o/old")));
    let row = store.project("P-1").unwrap().unwrap();
    assert_eq!(row.kind, Some(ProjectKind::Git));
    assert_eq!(row.remote_key.as_deref(), Some("github.com/o/old"));

    let before = hierarchy(&store);
    // A second one, one on a removed project, and one on an unknown project.
    assert!(rejected(set("k2", "P-1", ProjectKind::Folder, None)).contains("P-1"));
    assert!(rejected(set("k3", "P-2", ProjectKind::Git, None)).contains("P-2"));
    assert!(rejected(set("k4", "P-9", ProjectKind::Git, None)).contains("P-9"));
    assert_unchanged(&store, &before);
    assert_eq!(
        store.project("P-1").unwrap().unwrap().kind,
        Some(ProjectKind::Git)
    );
}

#[test]
fn set_project_remote_key_sets_the_key_of_a_live_git_project_only() {
    let (_d, store) = open();
    add_project(&store, "c1", "one", "/r/one", ProjectKind::Git, None);
    add_project(&store, "c2", "two", "/r/two", ProjectKind::Folder, None);
    add_project(
        &store,
        "c3",
        "three",
        "/r/three",
        ProjectKind::Git,
        Some("h/p"),
    );
    let set = |cmd: &str, id: &str, key: Option<&str>| {
        store
            .commit(effect_commit(
                cmd,
                "chat_session_start",
                Effect::SetProjectRemoteKey {
                    id: id.to_string(),
                    remote_key: key.map(String::from),
                },
            ))
            .unwrap()
    };
    applied(set("k1", "P-1", Some("github.com/o/one")));
    assert_eq!(
        store.project("P-1").unwrap().unwrap().remote_key.as_deref(),
        Some("github.com/o/one")
    );
    // The key can go back to none, and back again.
    applied(set("k2", "P-1", None));
    assert_eq!(store.project("P-1").unwrap().unwrap().remote_key, None);

    applied(remove_project(&store, "c4", "P-3"));
    let before = hierarchy(&store);
    assert!(rejected(set("k3", "P-2", Some("h/p"))).contains("not a git project"));
    assert!(rejected(set("k4", "P-9", Some("h/p"))).contains("P-9"));
    assert!(rejected(set("k5", "P-3", Some("h/q"))).contains("P-3"));
    assert_unchanged(&store, &before);
    assert_eq!(store.project("P-2").unwrap().unwrap().remote_key, None);
    assert_eq!(
        store.project("P-3").unwrap().unwrap().remote_key.as_deref(),
        Some("h/p")
    );
}

#[test]
fn a_first_start_records_the_worktree_and_names_both_ids_in_the_line() {
    let (_d, store) = open();
    let p = add_project(&store, "c1", "one", "/r/one", ProjectKind::Git, None);
    assert_eq!(p, "P-1");
    assert!(store.worktrees("P-1").unwrap().is_empty());

    let (admission, line) = applied(start(&store, "s1", "P-1", record("/r/one")));
    assert_eq!(admission.unit_id.as_deref(), Some("S-1"));
    assert_eq!(admission.kind, "chat_session_start");
    assert_eq!(admission.run_id, CONTROLLER_SCOPE);
    let body = line.expect("a ledger line").body;
    assert_eq!(
        body,
        "chat_session_start S-1 admitted project=P-1 worktree=W-1 command=s1"
    );
    assert!(!body.contains('{'), "no stand-in is left: {body}");

    let w = store.worktree("W-1").unwrap().unwrap();
    assert_eq!(w.project_id, "P-1");
    assert_eq!(w.path, "/r/one");
    assert_eq!(w.created_by, WorktreeCreatedBy::Root);
    assert_eq!((w.gone_at.clone(), w.removed_at.clone()), (None, None));
    assert_eq!(store.worktrees("P-1").unwrap(), vec![w.clone()]);
    assert_eq!(store.live_worktree_at("/r/one").unwrap(), Some(w));
    assert_eq!(store.live_worktree_at("/r/other").unwrap(), None);

    // The session takes its settings from the agent row, starts idle and has no runtime ref.
    let agent = store.agent("orchestrator").unwrap().unwrap();
    let s = store.chat_session("S-1").unwrap().unwrap();
    assert_eq!(s.project_id, "P-1");
    assert_eq!(s.worktree_id, "W-1");
    assert_eq!(s.agent_id, "orchestrator");
    assert_eq!(s.runtime, agent.runtime);
    assert_eq!(s.model, agent.model);
    assert_eq!(s.effort, agent.effort);
    assert_eq!(s.permission_mode, agent.permission_mode);
    assert_eq!(s.state, ChatSessionState::Idle);
    assert_eq!(s.runtime_session_ref, None);
    assert_eq!(s.ended_at, None);
    assert_eq!(
        (
            s.input_tokens,
            s.output_tokens,
            s.cache_read_tokens,
            s.cache_write_tokens,
            s.cost_micros
        ),
        (0, 0, 0, 0, 0)
    );
    assert_eq!(s.created_at, "t3");
    assert_eq!(s.updated_at, "t3");
    assert_eq!(store.live_chat_sessions().unwrap(), vec![s.clone()]);
    assert_eq!(store.chat_sessions("P-1").unwrap(), vec![s]);

    // The stored admission and the outbox hold the substituted text.
    let outbox = store.unwritten_ledger_lines(LEDGER).unwrap();
    assert_eq!(outbox.last().unwrap().body, body);
}

#[test]
fn a_second_live_orchestrator_is_rejected_and_names_the_first() {
    let (_d, store) = open();
    add_project(&store, "c1", "one", "/r/one", ProjectKind::Git, None);
    applied(start(&store, "s1", "P-1", record("/r/one")));
    let before = hierarchy(&store);
    let why = rejected(start(&store, "s2", "P-1", existing("W-1")));
    assert!(why.contains("S-1"), "{why}");
    let why = rejected(start(&store, "s3", "P-1", record("/r/elsewhere")));
    assert!(why.contains("S-1"), "{why}");
    // No second session, no worktree row for `/r/elsewhere`, and no other row changed.
    assert_unchanged(&store, &before);
    assert_eq!(store.worktrees("P-1").unwrap().len(), 1);
    assert_eq!(store.chat_sessions("P-1").unwrap().len(), 1);
}

#[test]
fn a_start_after_an_end_is_a_new_row_with_no_runtime_ref() {
    let (_d, store) = open();
    add_project(&store, "c1", "one", "/r/one", ProjectKind::Git, None);
    applied(start(&store, "s1", "P-1", record("/r/one")));
    assert!(
        store
            .set_chat_runtime_ref("S-1", "cli-session-1", "t7")
            .unwrap()
    );
    applied(end(&store, "e1", "S-1"));
    let first = store.chat_session("S-1").unwrap().unwrap();
    assert_eq!(first.ended_at.as_deref(), Some("t4"));
    assert_eq!(first.runtime_session_ref.as_deref(), Some("cli-session-1"));
    assert!(store.live_chat_sessions().unwrap().is_empty());

    let (admission, line) = applied(start(&store, "s2", "P-1", existing("W-1")));
    assert_eq!(admission.unit_id.as_deref(), Some("S-2"));
    assert_eq!(
        line.unwrap().body,
        "chat_session_start S-2 admitted project=P-1 worktree=W-1 command=s2"
    );
    let second = store.chat_session("S-2").unwrap().unwrap();
    assert_eq!(
        second.runtime_session_ref, None,
        "nothing copies the old ref"
    );
    assert_eq!(second.worktree_id, "W-1");
    assert_eq!(
        store.worktrees("P-1").unwrap().len(),
        1,
        "no second worktree row"
    );
    assert_eq!(store.chat_sessions("P-1").unwrap().len(), 2);
    assert_eq!(store.live_chat_sessions().unwrap(), vec![second]);
}

#[test]
fn a_start_that_cannot_apply_is_rejected_and_writes_nothing() {
    let (_d, store) = open();
    add_project(&store, "c1", "one", "/r/one", ProjectKind::Git, None);
    add_project(&store, "c2", "two", "/r/two", ProjectKind::Git, None);
    add_project(&store, "c3", "three", "/r/three", ProjectKind::Git, None);
    // P-2 has a worktree of its own, and P-1 has one that goes gone.
    applied(start(&store, "s1", "P-2", record("/r/two")));
    applied(start(&store, "s2", "P-1", record("/r/one")));
    applied(end(&store, "e1", "S-2"));
    assert!(store.mark_worktree_gone("W-2", "t8").unwrap());
    remove_project(&store, "r1", "P-3");
    let before = hierarchy(&store);

    // A gone worktree starts no session.
    let why = rejected(start(&store, "s3", "P-1", existing("W-2")));
    assert!(why.contains("W-2"), "{why}");
    // Another project's worktree.
    let why = rejected(start(&store, "s4", "P-1", existing("W-1")));
    assert!(why.contains("W-1"), "{why}");
    // An unknown worktree.
    rejected(start(&store, "s5", "P-1", existing("W-9")));
    // A path that a live row holds already.
    let why = rejected(start(&store, "s6", "P-1", record("/r/two")));
    assert!(why.contains("/r/two"), "{why}");
    // An unknown agent.
    let c = store
        .commit(start_commit("s7", "P-1", "reviewer", existing("W-1")))
        .unwrap();
    assert!(rejected(c).contains("reviewer"));
    // An unknown project, and a removed project.
    let why = rejected(start(&store, "s8", "P-9", record("/r/nine")));
    assert!(why.contains("P-9"), "{why}");
    let why = rejected(start(&store, "s9", "P-3", record("/r/three")));
    assert!(why.contains("P-3"), "{why}");

    assert_unchanged(&store, &before);
    assert!(store.worktrees("P-3").unwrap().is_empty());
    assert!(store.worktrees("P-9").unwrap().is_empty());
}

#[test]
fn a_remove_waits_for_the_live_session_and_then_marks_the_worktrees_removed() {
    let (_d, store) = open();
    add_project(&store, "c1", "one", "/r/one", ProjectKind::Git, None);
    applied(start(&store, "s1", "P-1", record("/r/one")));
    applied(end(&store, "e1", "S-1"));
    applied(start(&store, "s2", "P-1", existing("W-1")));

    let before = hierarchy(&store);
    let why = rejected(remove_project(&store, "r1", "P-1"));
    assert_eq!(why, "project P-1 has a live chat session S-2; end it first");
    assert_unchanged(&store, &before);
    assert_eq!(store.projects().unwrap().len(), 1);
    assert_eq!(store.worktree("W-1").unwrap().unwrap().removed_at, None);

    applied(end(&store, "e2", "S-2"));
    applied(remove_project(&store, "r2", "P-1"));
    assert!(store.projects().unwrap().is_empty());
    let w = store.worktree("W-1").unwrap().unwrap();
    assert_eq!(w.removed_at.as_deref(), Some("t2"));
    assert_eq!(store.live_worktree_at("/r/one").unwrap(), None);

    // The same root, added again, is a new project, and its first start records a new row.
    let p = add_project(&store, "c2", "one", "/r/one", ProjectKind::Git, None);
    assert_eq!(p, "P-2");
    let (admission, line) = applied(start(&store, "s3", "P-2", record("/r/one")));
    assert_eq!(admission.unit_id.as_deref(), Some("S-3"));
    assert!(line.unwrap().body.contains("worktree=W-2"));
    assert_eq!(store.worktrees("P-2").unwrap().len(), 1);
    assert_eq!(
        store.worktrees("P-1").unwrap().len(),
        1,
        "the old row stays"
    );
}

#[test]
fn set_chat_session_runtime_changes_only_the_named_fields_of_a_live_session() {
    let (_d, store) = open();
    add_project(&store, "c1", "one", "/r/one", ProjectKind::Git, None);
    applied(start(&store, "s1", "P-1", record("/r/one")));
    let was = store.chat_session("S-1").unwrap().unwrap();
    let set =
        |cmd: &str, id: &str, model: Option<&str>, effort: Option<&str>, mode: Option<&str>| {
            store
                .commit(effect_commit(
                    cmd,
                    "chat_session_runtime",
                    Effect::SetChatSessionRuntime {
                        id: id.to_string(),
                        model: model.map(String::from),
                        effort: effort.map(String::from),
                        permission_mode: mode.map(String::from),
                        updated_at: "t9".to_string(),
                    },
                ))
                .unwrap()
        };
    // Each update changes the named fields and `updated_at`, and no other column of the row.
    applied(set("m1", "S-1", Some("claude-sonnet-5-5"), None, None));
    let mut expected = was.clone();
    expected.model = "claude-sonnet-5-5".to_string();
    expected.updated_at = "t9".to_string();
    let now = store.chat_session("S-1").unwrap().unwrap();
    assert_eq!(now, expected);
    assert_ne!(now, was, "the update wrote something");

    applied(set("m2", "S-1", None, Some("low"), Some("plan")));
    expected.effort = "low".to_string();
    expected.permission_mode = "plan".to_string();
    assert_eq!(store.chat_session("S-1").unwrap().unwrap(), expected);
    // The agent row keeps its defaults: the override belongs to the session.
    let agent = store.agent("orchestrator").unwrap().unwrap();
    assert_eq!(agent.model, was.model);

    // A session with a runtime ref and a state keeps both through an update.
    assert!(store.set_chat_runtime_ref("S-1", "cli-1", "t10").unwrap());
    assert!(
        store
            .set_chat_session_state("S-1", ChatSessionState::Working, "t11")
            .unwrap()
    );
    let held = store.chat_session("S-1").unwrap().unwrap();
    assert_eq!(held.runtime_session_ref.as_deref(), Some("cli-1"));
    applied(set("m5", "S-1", None, Some("high"), None));
    let mut expected = held.clone();
    expected.effort = "high".to_string();
    expected.updated_at = "t9".to_string();
    assert_eq!(store.chat_session("S-1").unwrap().unwrap(), expected);

    applied(end(&store, "e1", "S-1"));
    let ended = store.chat_session("S-1").unwrap().unwrap();
    assert!(ended.ended_at.is_some());
    let before = hierarchy(&store);
    assert!(rejected(set("m3", "S-1", Some("x"), None, None)).contains("S-1"));
    assert!(rejected(set("m4", "S-9", Some("x"), None, None)).contains("S-9"));
    assert_unchanged(&store, &before);
    assert_eq!(store.chat_session("S-1").unwrap().unwrap(), ended);
}

#[test]
fn the_four_writes_outside_admission_write_once_and_make_no_admission_or_line() {
    let (_d, store) = open();
    add_project(&store, "c1", "one", "/r/one", ProjectKind::Git, None);
    applied(start(&store, "s1", "P-1", record("/r/one")));
    let before = admission_state(&store);

    // mark_worktree_gone writes once, and answers false after.
    assert!(store.mark_worktree_gone("W-1", "t8").unwrap());
    assert!(!store.mark_worktree_gone("W-1", "t9").unwrap());
    assert!(!store.mark_worktree_gone("W-9", "t9").unwrap());
    assert_eq!(
        store.worktree("W-1").unwrap().unwrap().gone_at.as_deref(),
        Some("t8"),
        "the first mark stays"
    );
    // A removed row is never marked.
    add_project(&store, "c2", "two", "/r/two", ProjectKind::Git, None);
    applied(start(&store, "s2", "P-2", record("/r/two")));
    applied(end(&store, "e2", "S-2"));
    applied(remove_project(&store, "r2", "P-2"));
    let before = (before.0 + 4, before.1 + 4);
    assert!(!store.mark_worktree_gone("W-2", "t9").unwrap());
    assert_eq!(store.worktree("W-2").unwrap().unwrap().gone_at, None);

    // The state and the ref of a live session.
    add_project(&store, "c3", "three", "/r/three", ProjectKind::Folder, None);
    applied(start(&store, "s3", "P-3", record("/r/three")));
    let before = (before.0 + 2, before.1 + 2);
    assert!(
        store
            .set_chat_session_state("S-3", ChatSessionState::Working, "t10")
            .unwrap()
    );
    assert!(store.set_chat_runtime_ref("S-3", "abc", "t11").unwrap());
    assert!(store.set_chat_runtime_ref("S-3", "def", "t12").unwrap());
    let s = store.chat_session("S-3").unwrap().unwrap();
    assert_eq!(s.state, ChatSessionState::Working);
    assert_eq!(
        s.runtime_session_ref.as_deref(),
        Some("def"),
        "a later ref replaces an earlier one"
    );
    assert_eq!(s.updated_at, "t12");

    applied(end(&store, "e3", "S-3"));
    let before = (before.0 + 1, before.1 + 1);
    assert!(
        !store
            .set_chat_session_state("S-3", ChatSessionState::Idle, "t13")
            .unwrap()
    );
    assert!(!store.set_chat_runtime_ref("S-3", "ghi", "t13").unwrap());
    assert!(
        !store
            .set_chat_session_state("S-9", ChatSessionState::Idle, "t13")
            .unwrap()
    );
    assert!(!store.set_chat_runtime_ref("S-9", "ghi", "t13").unwrap());
    let s = store.chat_session("S-3").unwrap().unwrap();
    assert_eq!(s.state, ChatSessionState::Working);
    assert_eq!(s.runtime_session_ref.as_deref(), Some("def"));
    assert_eq!(s.updated_at, "t12");

    // None of the four made an admission or an outbox line: only the commands above did.
    assert_eq!(admission_state(&store), before);
}

#[test]
fn agents_lists_the_orchestrator_and_agent_reads_its_maps() {
    let (_d, store) = open();
    let all = store.agents().unwrap();
    assert_eq!(all.len(), 1);
    let a = store.agent("orchestrator").unwrap().unwrap();
    assert_eq!(all[0], a);
    assert!(a.built_in);
    assert_eq!(a.name, "Orchestrator");
    assert_eq!(a.title, "Runs this project");
    assert_eq!(a.instructions, None);
    assert_eq!(a.runtime, "claude-code");
    assert_eq!(a.model, "claude-opus-5-5");
    assert_eq!(a.effort, "high");
    assert_eq!(a.permission_mode, "bypassPermissions");
    assert!(a.skills.is_empty());
    assert_eq!(
        a.runtime_options.get("artifacts").map(String::as_str),
        Some("on")
    );
    assert_eq!(a.runtime_options.len(), 1);
    assert!(a.launch_env.is_empty());
    assert_eq!(store.agent("reviewer").unwrap(), None);
}

#[test]
fn an_agent_row_whose_map_does_not_fit_fails_the_read() {
    let (dir, store) = open();
    drop(store);
    {
        let conn = rusqlite::Connection::open(dir.path().join("store.sqlite")).unwrap();
        conn.execute_batch(
            "INSERT INTO agents VALUES ('odd', 'n', 't', 0, 'c', 'h', NULL, 'r', 'm', 'e', 'p', \
                 '[1]', '{\"k\":\"v\"}', '{\"n\":1}', 't')",
        )
        .unwrap();
    }
    let store = pf_store_sqlite::SqliteStore::open(dir.path().join("store.sqlite")).unwrap();
    // An array of numbers is no list of names, and a number is no value of the env map.
    assert!(store.agent("odd").is_err());
    assert!(store.agents().is_err());
    assert!(store.agent("orchestrator").unwrap().is_some());
}

#[test]
fn a_rejected_start_is_a_plain_rejection_and_a_replay_is_a_replay() {
    let (_d, store) = open();
    add_project(&store, "c1", "one", "/r/one", ProjectKind::Git, None);
    let first = start(&store, "s1", "P-1", record("/r/one"));
    let (admission, _) = applied(first);
    // The same command ID with the same content is a replay of the first admission.
    match start(&store, "s1", "P-1", existing("W-1")) {
        Committed::Replayed(row) => assert_eq!(row, admission),
        other => panic!("not a replay: {other:?}"),
    }
    // The same ID with other content is a conflict.
    let other = store
        .commit(common::commit(
            "chat_session_start",
            "s1",
            "{\"agent_id\":\"orchestrator\",\"project_id\":\"P-2\"}",
            AdmissionOutcome::Admitted,
            None,
            None,
            vec![],
            None,
        ))
        .unwrap();
    assert!(matches!(other, Committed::Conflict(_)), "{other:?}");
    assert_eq!(store.chat_sessions("P-1").unwrap().len(), 1);
}

#[test]
fn a_session_in_a_recorded_worktree_of_another_kind_of_creator_keeps_the_creator() {
    let (_d, store) = open();
    add_project(&store, "c1", "one", "/r/one", ProjectKind::Git, None);
    let c = start_commit(
        "s1",
        "P-1",
        "orchestrator",
        SessionWorktree::Record {
            path: "/r/one-wt".to_string(),
            created_by: WorktreeCreatedBy::External,
        },
    );
    applied(store.commit(c).unwrap());
    let w = store.worktree("W-1").unwrap().unwrap();
    assert_eq!(w.created_by, WorktreeCreatedBy::External);
    assert_eq!(w.path, "/r/one-wt");
}

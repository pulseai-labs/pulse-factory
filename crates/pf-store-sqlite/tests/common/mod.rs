//! Helpers for the hierarchy tests: commits of the controller's commands on the real store, built
//! as the controller builds them.
#![allow(dead_code)]

use pf_engine::port::store::{
    AdmissionOutcome, AdmissionRow, CONTROLLER_SCOPE, ChatItemKind, Commit, Committed, Effect,
    LedgerEntry, LedgerLine, NEW_ID, NewAdmission, NewChatItem, NewChatSession, NewProject,
    ORCHESTRATOR_AGENT, ProjectKind, SessionWorktree, Store, WORKTREE_ID, WorktreeCreatedBy,
};
use pf_store_sqlite::SqliteStore;

pub const LEDGER: &str = "controller.ledger";

pub fn open() -> (tempfile::TempDir, SqliteStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("store.sqlite")).unwrap();
    (dir, store)
}

/// One controller commit, with an optional ledger line. The arguments are the fields of a commit,
/// in the order of the type, so a call reads as the commit it makes.
#[allow(clippy::too_many_arguments)]
pub fn commit(
    kind: &str,
    command_id: &str,
    content: &str,
    outcome: AdmissionOutcome,
    reason: Option<&str>,
    unit_id: Option<&str>,
    effects: Vec<Effect>,
    line: Option<String>,
) -> Commit {
    Commit {
        run_id: CONTROLLER_SCOPE.to_string(),
        command_id: command_id.to_string(),
        content: content.to_string(),
        admission: NewAdmission {
            kind: kind.to_string(),
            outcome,
            reason: reason.map(String::from),
            unit_id: unit_id.map(String::from),
            role: None,
            model: None,
            attempt: None,
            admitted_at: "t1".to_string(),
        },
        effects,
        ledger: line.map(|body| LedgerEntry {
            ledger_file: LEDGER.to_string(),
            body,
        }),
    }
}

pub fn applied(c: Committed) -> (AdmissionRow, Option<LedgerLine>) {
    match c {
        Committed::Applied { admission, ledger } => (admission, ledger),
        other => panic!("not applied: {other:?}"),
    }
}

pub fn rejected(c: Committed) -> String {
    match c {
        Committed::Rejected(why) => why,
        other => panic!("not rejected: {other:?}"),
    }
}

/// Add a project, and answer its ID.
pub fn add_project(
    store: &SqliteStore,
    command_id: &str,
    name: &str,
    root: &str,
    kind: ProjectKind,
    remote_key: Option<&str>,
) -> String {
    let c = commit(
        "project_add",
        command_id,
        &format!("{{\"name\":\"{name}\",\"root\":\"{root}\"}}"),
        AdmissionOutcome::Admitted,
        None,
        Some(NEW_ID),
        vec![Effect::AddProject(NewProject {
            name: name.to_string(),
            root: root.to_string(),
            created_at: "t1".to_string(),
            kind,
            remote_key: remote_key.map(String::from),
        })],
        Some(format!(
            "project_add {NEW_ID} admitted command={command_id}"
        )),
    );
    applied(store.commit(c).unwrap()).0.unit_id.unwrap()
}

pub fn remove_project(store: &SqliteStore, command_id: &str, id: &str) -> Committed {
    store
        .commit(commit(
            "project_remove",
            command_id,
            &format!("{{\"id\":\"{id}\"}}"),
            AdmissionOutcome::Admitted,
            None,
            Some(id),
            vec![Effect::RemoveProject {
                id: id.to_string(),
                removed_at: "t2".to_string(),
            }],
            Some(format!("project_remove {id} admitted command={command_id}")),
        ))
        .unwrap()
}

/// The commit of a chat session start, as the controller builds it.
pub fn start_commit(
    command_id: &str,
    project_id: &str,
    agent_id: &str,
    worktree: SessionWorktree,
) -> Commit {
    commit(
        "chat_session_start",
        command_id,
        &format!("{{\"agent_id\":\"{agent_id}\",\"project_id\":\"{project_id}\"}}"),
        AdmissionOutcome::Admitted,
        None,
        Some(NEW_ID),
        vec![Effect::StartChatSession(NewChatSession {
            project_id: project_id.to_string(),
            agent_id: agent_id.to_string(),
            worktree,
            created_at: "t3".to_string(),
        })],
        Some(format!(
            "chat_session_start {NEW_ID} admitted project={project_id} worktree={WORKTREE_ID} command={command_id}"
        )),
    )
}

pub fn record(path: &str) -> SessionWorktree {
    SessionWorktree::Record {
        path: path.to_string(),
        created_by: WorktreeCreatedBy::Root,
    }
}

pub fn existing(id: &str) -> SessionWorktree {
    SessionWorktree::Existing { id: id.to_string() }
}

/// Start an Orchestrator session and answer the committed result.
pub fn start(
    store: &SqliteStore,
    command_id: &str,
    project_id: &str,
    worktree: SessionWorktree,
) -> Committed {
    store
        .commit(start_commit(
            command_id,
            project_id,
            ORCHESTRATOR_AGENT,
            worktree,
        ))
        .unwrap()
}

pub fn end(store: &SqliteStore, command_id: &str, session_id: &str) -> Committed {
    store
        .commit(commit(
            "chat_session_end",
            command_id,
            &format!("{{\"session_id\":\"{session_id}\"}}"),
            AdmissionOutcome::Admitted,
            None,
            Some(session_id),
            vec![Effect::EndChatSession {
                id: session_id.to_string(),
                ended_at: "t4".to_string(),
            }],
            Some(format!(
                "chat_session_end {session_id} admitted command={command_id}"
            )),
        ))
        .unwrap()
}

pub fn redact(store: &SqliteStore, command_id: &str, session_id: &str, seq: u64) -> Committed {
    store
        .commit(commit(
            "chat_item_redact",
            command_id,
            &format!("{{\"seq\":{seq},\"session_id\":\"{session_id}\"}}"),
            AdmissionOutcome::Admitted,
            None,
            Some(session_id),
            vec![Effect::RedactChatItem {
                session_id: session_id.to_string(),
                seq,
                redacted_at: "t5".to_string(),
            }],
            Some(format!(
                "chat_item_redact {session_id} seq={seq} admitted command={command_id}"
            )),
        ))
        .unwrap()
}

/// A chat item of `kind` with a JSON object payload, and no tool text.
pub fn item(session_id: &str, kind: ChatItemKind) -> NewChatItem {
    NewChatItem {
        session_id: session_id.to_string(),
        kind,
        payload: "{}".to_string(),
        tool_input: None,
        tool_output: None,
        answers_seq: None,
        usage: None,
        created_at: "t6".to_string(),
    }
}

/// An answer item to the request at `answers_seq`, with an outcome.
pub fn answer(
    session_id: &str,
    kind: ChatItemKind,
    answers_seq: u64,
    outcome: &str,
) -> NewChatItem {
    let mut a = item(session_id, kind);
    a.payload = format!("{{\"outcome\":\"{outcome}\"}}");
    a.answers_seq = Some(answers_seq);
    a
}

/// How much the store holds that a write outside admission must not change.
pub fn admission_state(store: &SqliteStore) -> (usize, usize) {
    (
        store.admissions(CONTROLLER_SCOPE).unwrap().len(),
        store.unwritten_ledger_lines(LEDGER).unwrap().len(),
    )
}

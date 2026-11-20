//! Helpers for the hierarchy tests: commits of the controller's commands on the real store, built
//! as the controller builds them.
#![allow(dead_code)]

use pf_engine::port::store::{
    AdmissionOutcome, AdmissionRow, CONTROLLER_SCOPE, ChatItemKind, Commit, Committed, Effect,
    LedgerEntry, LedgerLine, NEW_ID, NewAdmission, NewChatItem, NewChatSession, NewHost,
    NewLaunchRecord, NewProject, ORCHESTRATOR_AGENT, ProjectKind, SessionWorktree, Store,
    WORKTREE_ID, WorktreeCreatedBy,
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
        host: None,
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

// ------------------------------------------------------------------ launch records and hosts

/// A launch record body: a chat's launch, with the seat-less fields the record names. `secret` is
/// the value of the one secret name, `TOKEN`.
pub fn record_body(secret: &str) -> String {
    serde_json::json!({
        "argv": ["claude", "--output-format", "stream-json"],
        "cwd": "/work/alpha",
        "env": {"HOME": "/home/dev", "TOKEN": secret},
        "secret_names": ["TOKEN"],
        "transport": {"kind": "pipes"},
        "runtime": {
            "id": "claude-code",
            "version": "2.1.0",
            "binary_path": "/share/chat-runtime/2.1.0/claude",
            "binary_sha256": "ab".repeat(32),
        },
        "settings": {"chat.self_turn_cap": {"value": 5, "scope": "server"}},
        "plugins": [],
        "skills": [],
        "rules_sha256": "cd".repeat(32),
        "run_binding": null,
        "resume": "fresh",
    })
    .to_string()
}

/// A secret value the record may hold.
pub fn hashed_secret() -> String {
    format!("sha256:{}", "ef".repeat(32))
}

/// A launch record of a session, sealed: the store gets the canonical body and its hash.
pub fn new_record(
    kind: &str,
    session: &str,
    incarnation: u32,
    revision: u32,
    body: &str,
) -> NewLaunchRecord {
    let (_, hash) = NewLaunchRecord::seal(body).unwrap();
    NewLaunchRecord {
        session_kind: kind.to_string(),
        session_id: session.to_string(),
        incarnation,
        revision,
        launcher: "detached".to_string(),
        host_unit: format!("pf-host-srv-{session}-{incarnation}"),
        host_socket: format!("/run/pf-hosts/srv/{session}-{incarnation}/host.sock"),
        record: body.to_string(),
        record_sha256: hash,
        created_at: "t1".to_string(),
    }
}

/// The commit that writes one launch record, as the controller builds it.
pub fn record_commit(command_id: &str, record: NewLaunchRecord) -> Commit {
    let line = format!(
        "launch_record {} {} incarnation={} revision={} admitted command={command_id}",
        record.session_kind, record.session_id, record.incarnation, record.revision
    );
    let content = format!(
        "{{\"session\":\"{}\",\"incarnation\":{},\"revision\":{},\"sha256\":\"{}\"}}",
        record.session_id, record.incarnation, record.revision, record.record_sha256
    );
    commit(
        "launch_record_write",
        command_id,
        &content,
        AdmissionOutcome::Admitted,
        None,
        Some(&record.session_id.clone()),
        vec![Effect::LaunchRecordWrite(record)],
        Some(line),
    )
}

/// A host row started from a revision of a launch record.
pub fn new_host(
    unit: &str,
    kind: &str,
    session: &str,
    incarnation: u32,
    revision: u32,
    record_sha256: &str,
) -> NewHost {
    NewHost {
        unit: unit.to_string(),
        session_kind: kind.to_string(),
        session_id: session.to_string(),
        incarnation,
        launched_revision: revision,
        launched_record_sha256: record_sha256.to_string(),
        launcher: "detached".to_string(),
        socket: format!("/run/pf-hosts/srv/{session}-{incarnation}/host.sock"),
        invocation_id: "inv-1".to_string(),
        host_binary: "/share/hosts/aa/pf-host".to_string(),
        host_sha256: "aa".repeat(32),
        protocol: 1,
        started_at: "t2".to_string(),
    }
}

pub fn host_commit(command_id: &str, host: NewHost) -> Commit {
    let content = format!(
        "{{\"unit\":\"{}\",\"revision\":{},\"sha256\":\"{}\"}}",
        host.unit, host.launched_revision, host.launched_record_sha256
    );
    commit(
        "host_started",
        command_id,
        &content,
        AdmissionOutcome::Admitted,
        None,
        Some(&host.unit.clone()),
        vec![Effect::HostStarted(host)],
        Some(format!("host_started {command_id}")),
    )
}

pub fn host_end_commit(command_id: &str, unit: &str, exit: &str) -> Commit {
    commit(
        "host_ended",
        command_id,
        &format!("{{\"unit\":\"{unit}\",\"exit\":{exit}}}"),
        AdmissionOutcome::Admitted,
        None,
        Some(unit),
        vec![Effect::HostEnded {
            unit: unit.to_string(),
            ended_at: "t3".to_string(),
            exit: exit.to_string(),
        }],
        Some(format!("host_ended {unit} command={command_id}")),
    )
}

/// A live host named `unit`, with its launch record: a seat of the session `S-<unit>`.
pub fn live_host(store: &SqliteStore, unit: &str) {
    let session = format!("S-{unit}");
    let record = new_record("seat", &session, 1, 1, &record_body(&hashed_secret()));
    let hash = record.record_sha256.clone();
    applied(
        store
            .commit(record_commit(&format!("lr-{unit}"), record))
            .unwrap(),
    );
    applied(
        store
            .commit(host_commit(
                &format!("hs-{unit}"),
                new_host(unit, "seat", &session, 1, 1, &hash),
            ))
            .unwrap(),
    );
}

/// The commit that saves one position of a host, as the controller's host service builds it: no
/// ledger line, the command ID `host-position-<unit>-<seq>` by the caller's choice.
pub fn position_commit(command_id: &str, unit: &str, seq: u64, position: &str) -> Commit {
    commit(
        "host_position",
        command_id,
        &format!("{{\"unit\":\"{unit}\",\"seq\":{seq},\"position\":{position}}}"),
        AdmissionOutcome::Admitted,
        None,
        Some(unit),
        vec![Effect::HostPositionWrite {
            unit: unit.to_string(),
            seq,
            position: position.to_string(),
            updated_at: "t5".to_string(),
        }],
        None,
    )
}

// ------------------------------------------------------------------ an older store file

/// A store file at schema version 8, built from the text of migrations `0001` to `0008` alone, as the
/// binary of `r2.s1` left it: an ended run (`run-1`) with a unit, a project and one admission. The file
/// stays in the default journal mode, so the store converts it to WAL when it opens it.
pub fn version_eight_store(path: &std::path::Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
    for (version, name, sql) in [
        (1, "init", include_str!("../../migrations/0001_init.sql")),
        (
            2,
            "requests",
            include_str!("../../migrations/0002_requests.sql"),
        ),
        (
            3,
            "projects",
            include_str!("../../migrations/0003_projects.sql"),
        ),
        (
            4,
            "devices",
            include_str!("../../migrations/0004_devices.sql"),
        ),
        (
            5,
            "hierarchy",
            include_str!("../../migrations/0005_hierarchy.sql"),
        ),
        (
            6,
            "session_options",
            include_str!("../../migrations/0006_session_options.sql"),
        ),
        (
            7,
            "settings",
            include_str!("../../migrations/0007_settings.sql"),
        ),
        (
            8,
            "run_project",
            include_str!("../../migrations/0008_run_project.sql"),
        ),
    ] {
        conn.execute_batch(sql).unwrap();
        conn.execute(
            "INSERT INTO schema_version (version, name, applied_at) VALUES (?1, ?2, 't0')",
            rusqlite::params![version, name],
        )
        .unwrap();
    }
    conn.execute_batch(
        "INSERT INTO runs (id, workflow, ledger_file, state, created_at, updated_at) \
             VALUES ('run-1', 'wf', '/l.md', 'ended', 't0', 't0'); \
         INSERT INTO nodes (run_id, id, kind, status, detail, created_at, updated_at) \
             VALUES ('run-1', 'u1', 'unit', 'done', '{}', 't0', 't0'); \
         INSERT INTO projects (id, name, root, created_at, kind) \
             VALUES ('P-1', 'alpha', '/work/alpha', 't2', 'folder'); \
         INSERT INTO admissions (run_id, command_id, kind, content, outcome, admitted_at) \
             VALUES ('@controller', 'c1', 'project_add', '{}', 'admitted', 't1');",
    )
    .unwrap();
}

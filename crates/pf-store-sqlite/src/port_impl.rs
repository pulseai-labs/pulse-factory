//! The store port on SQLite.
//!
//! One commit is one `BEGIN IMMEDIATE` transaction on the writer thread: the receipt, the state
//! writes and the outbox line, together or not at all.

use std::collections::BTreeMap;

use pf_engine::port::store::{
    ANSWER_OUTCOMES, AdmissionOutcome, AdmissionRow, AgentRow, ChatItemKind, ChatItemRow,
    ChatSessionRow, ChatSessionState, ClaimRow, Commit, Committed, DeviceRow, Effect, LedgerLine,
    NEW_ID, NewChatItem, NewChatSession, NewDevice, NewProject, NewRequest, NodeKind, NodeRow,
    ORCHESTRATOR_AGENT, ProjectKind, ProjectRow, REDACTED_PAYLOAD, REDACTED_TEXT, RequestKind,
    RequestRow, RequestStatus, RunRow, RunState, SessionRow, SessionWorktree, Store, StoreError,
    TOOL_TEXT_MAX_BYTES, ToolText, WORKTREE_ID, WorktreeCreatedBy, WorktreeRow,
};
use pf_engine::status::Status;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::{SqliteStore, sql_error};

/// Counts and ceilings fit SQLite's signed 64-bit integer.
fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn to_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn bad(what: &str, value: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        format!("unknown {what} {value:?}").into(),
    )
}

const ADMISSION_COLUMNS: &str = "seq, run_id, command_id, kind, content, outcome, reason, unit_id, role, model, attempt, admitted_at";

fn admission_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<AdmissionRow> {
    let outcome: String = row.get(5)?;
    Ok(AdmissionRow {
        seq: row.get(0)?,
        run_id: row.get(1)?,
        command_id: row.get(2)?,
        kind: row.get(3)?,
        content: row.get(4)?,
        outcome: AdmissionOutcome::parse(&outcome).ok_or_else(|| bad("outcome", &outcome))?,
        reason: row.get(6)?,
        unit_id: row.get(7)?,
        role: row.get(8)?,
        model: row.get(9)?,
        attempt: row
            .get::<_, Option<i64>>(10)?
            .map(|a| u32::try_from(a).unwrap_or(u32::MAX)),
        admitted_at: row.get(11)?,
    })
}

fn find_admission(
    conn: &Connection,
    run_id: &str,
    command_id: &str,
) -> rusqlite::Result<Option<AdmissionRow>> {
    conn.query_row(
        &format!(
            "SELECT {ADMISSION_COLUMNS} FROM admissions WHERE run_id = ?1 AND command_id = ?2"
        ),
        params![run_id, command_id],
        admission_from,
    )
    .optional()
}

fn put_run(tx: &Transaction<'_>, r: &RunRow) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO runs (id, workflow, ledger_file, state, halted_reason, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
         ON CONFLICT (id) DO UPDATE SET workflow = excluded.workflow, ledger_file = excluded.ledger_file, \
             state = excluded.state, halted_reason = excluded.halted_reason, updated_at = excluded.updated_at",
        params![r.id, r.workflow, r.ledger_file, r.state.as_str(), r.halted_reason, r.created_at, r.updated_at],
    )?;
    Ok(())
}

fn put_node(tx: &Transaction<'_>, n: &NodeRow) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO nodes (run_id, id, parent_id, kind, status, reason, role, model, attempt, detail, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) \
         ON CONFLICT (run_id, id) DO UPDATE SET parent_id = excluded.parent_id, kind = excluded.kind, \
             status = excluded.status, reason = excluded.reason, role = excluded.role, model = excluded.model, \
             attempt = excluded.attempt, detail = excluded.detail, updated_at = excluded.updated_at",
        params![
            n.run_id, n.id, n.parent_id, n.kind.as_str(), n.status.as_str(), n.reason, n.role, n.model,
            i64::from(n.attempt), n.detail, n.created_at, n.updated_at
        ],
    )?;
    Ok(())
}

fn put_session(tx: &Transaction<'_>, s: &SessionRow) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO sessions (run_id, id, node_id, unit_id, process_ref, ended, input_tokens, output_tokens, \
             cache_read_tokens, cache_write_tokens, cost_micros, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) \
         ON CONFLICT (run_id, id) DO UPDATE SET node_id = excluded.node_id, unit_id = excluded.unit_id, \
             process_ref = excluded.process_ref, ended = excluded.ended, input_tokens = excluded.input_tokens, \
             output_tokens = excluded.output_tokens, cache_read_tokens = excluded.cache_read_tokens, \
             cache_write_tokens = excluded.cache_write_tokens, cost_micros = excluded.cost_micros, \
             updated_at = excluded.updated_at",
        params![
            s.run_id, s.id, s.node_id, s.unit_id, s.process_ref, i64::from(s.ended),
            to_i64(s.input_tokens), to_i64(s.output_tokens), to_i64(s.cache_read_tokens),
            to_i64(s.cache_write_tokens), to_i64(s.cost_micros), s.created_at, s.updated_at
        ],
    )?;
    Ok(())
}

const REQUEST_COLUMNS: &str = "id, revision, run_id, unit_id, stage, kind, reason, body, status, opened_at, answered_at, answer, answer_generation";

fn request_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<RequestRow> {
    let kind: String = r.get(5)?;
    let status: String = r.get(8)?;
    Ok(RequestRow {
        id: r.get(0)?,
        revision: u32::try_from(r.get::<_, i64>(1)?).unwrap_or(u32::MAX),
        run_id: r.get(2)?,
        unit_id: r.get(3)?,
        stage: r.get(4)?,
        kind: RequestKind::parse(&kind).ok_or_else(|| bad("request kind", &kind))?,
        reason: r.get(6)?,
        body: r.get(7)?,
        status: RequestStatus::parse(&status).ok_or_else(|| bad("request status", &status))?,
        opened_at: r.get(9)?,
        answered_at: r.get(10)?,
        answer: r.get(11)?,
        answer_generation: r.get::<_, Option<i64>>(12)?.map(to_u64),
    })
}

/// Give a new request the next `R-<n>` of the store and insert it, open at revision 1.
fn open_request(tx: &Transaction<'_>, n: &NewRequest) -> rusqlite::Result<()> {
    let next: i64 = tx.query_row(
        "SELECT COALESCE(MAX(CAST(substr(id, 3) AS INTEGER)), 0) + 1 FROM requests",
        [],
        |r| r.get(0),
    )?;
    tx.execute(
        "INSERT INTO requests (id, revision, run_id, unit_id, stage, kind, reason, body, status, opened_at) \
         VALUES (?1, 1, ?2, ?3, ?4, ?5, ?6, ?7, 'open', ?8)",
        params![
            format!("R-{next}"),
            n.run_id,
            n.unit_id,
            n.stage,
            n.kind.as_str(),
            n.reason,
            n.body,
            n.opened_at
        ],
    )?;
    Ok(())
}

/// Replace the request with this ID. A request that does not exist is an error: the commit
/// rolls back.
fn put_request(tx: &Transaction<'_>, r: &RequestRow) -> rusqlite::Result<()> {
    let changed = tx.execute(
        "UPDATE requests SET revision = ?2, run_id = ?3, unit_id = ?4, stage = ?5, kind = ?6, reason = ?7, \
             body = ?8, status = ?9, opened_at = ?10, answered_at = ?11, answer = ?12, answer_generation = ?13 \
         WHERE id = ?1",
        params![
            r.id,
            i64::from(r.revision),
            r.run_id,
            r.unit_id,
            r.stage,
            r.kind.as_str(),
            r.reason,
            r.body,
            r.status.as_str(),
            r.opened_at,
            r.answered_at,
            r.answer,
            r.answer_generation.map(to_i64)
        ],
    )?;
    if changed == 0 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    Ok(())
}

fn put_claim(tx: &Transaction<'_>, c: &ClaimRow) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO claims (generation, token_hash, holder, claimed_at, fenced_at) \
         VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT (generation) DO UPDATE SET fenced_at = excluded.fenced_at",
        params![
            to_i64(c.generation),
            c.token_hash,
            c.holder,
            c.claimed_at,
            c.fenced_at
        ],
    )?;
    Ok(())
}

const PROJECT_COLUMNS: &str = "id, name, root, created_at, removed_at, kind, remote_key";

fn project_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectRow> {
    let kind: Option<String> = r.get(5)?;
    Ok(ProjectRow {
        id: r.get(0)?,
        name: r.get(1)?,
        root: r.get(2)?,
        created_at: r.get(3)?,
        removed_at: r.get(4)?,
        kind: kind
            .map(|k| ProjectKind::parse(&k).ok_or_else(|| bad("project kind", &k)))
            .transpose()?,
        remote_key: r.get(6)?,
    })
}

/// Why one effect did not apply.
enum EffectError {
    /// The effect cannot apply to this state. The commit is refused and rolls back.
    Rejected(String),
    /// SQLite failed. The commit rolls back and the call fails.
    Sql(rusqlite::Error),
}

impl From<rusqlite::Error> for EffectError {
    fn from(e: rusqlite::Error) -> EffectError {
        EffectError::Sql(e)
    }
}

/// Add a project and return its new ID, `P-<n>`. The number is the highest of the table plus one.
/// A removed row stays in the table, so no ID is used twice.
fn add_project(tx: &Transaction<'_>, n: &NewProject) -> Result<String, EffectError> {
    let taken = |column: &str, value: &str| -> rusqlite::Result<bool> {
        tx.query_row(
            &format!(
                "SELECT EXISTS (SELECT 1 FROM projects WHERE {column} = ?1 AND removed_at IS NULL)"
            ),
            [value],
            |r| r.get(0),
        )
    };
    if taken("root", &n.root)? {
        return Err(EffectError::Rejected(format!(
            "a live project already has the root {}",
            n.root
        )));
    }
    if taken("name", &n.name)? {
        return Err(EffectError::Rejected(format!(
            "a live project already has the name {}",
            n.name
        )));
    }
    let next: i64 = tx.query_row(
        "SELECT COALESCE(MAX(CAST(substr(id, 3) AS INTEGER)), 0) + 1 FROM projects",
        [],
        |r| r.get(0),
    )?;
    let id = format!("P-{next}");
    let inserted = tx.execute(
        "INSERT INTO projects (id, name, root, created_at, removed_at, kind, remote_key) \
         VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6)",
        params![
            id,
            n.name,
            n.root,
            n.created_at,
            n.kind.as_str(),
            n.remote_key
        ],
    );
    match inserted {
        Ok(_) => Ok(id),
        // The unique indexes are the backstop of the two checks above.
        Err(rusqlite::Error::SqliteFailure(e, _))
            if e.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            Err(EffectError::Rejected(
                "a live project already has this root or name".to_string(),
            ))
        }
        Err(e) => Err(e.into()),
    }
}

/// Mark a live project removed. The row stays.
fn remove_project(tx: &Transaction<'_>, id: &str, removed_at: &str) -> Result<(), EffectError> {
    let row = tx
        .query_row(
            &format!("SELECT {PROJECT_COLUMNS} FROM projects WHERE id = ?1"),
            [id],
            project_from,
        )
        .optional()?;
    match row {
        None => Err(EffectError::Rejected(format!("no project {id}"))),
        Some(p) if p.removed_at.is_some() => Err(EffectError::Rejected(format!(
            "project {id} is removed already"
        ))),
        Some(_) => {
            // A live chat session holds the project: it is ended first (ADR-0015).
            let live: Option<String> = tx
                .query_row(
                    "SELECT id FROM chat_sessions WHERE project_id = ?1 AND ended_at IS NULL \
                     ORDER BY rowid LIMIT 1",
                    [id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(session) = live {
                return Err(EffectError::Rejected(format!(
                    "project {id} has a live chat session {session}; end it first"
                )));
            }
            tx.execute(
                "UPDATE projects SET removed_at = ?2 WHERE id = ?1 AND removed_at IS NULL",
                params![id, removed_at],
            )?;
            // The live worktree rows go with the project, so the same root can be added again and
            // its first session start records a new row.
            tx.execute(
                "UPDATE worktrees SET removed_at = ?2 \
                 WHERE project_id = ?1 AND removed_at IS NULL AND gone_at IS NULL",
                params![id, removed_at],
            )?;
            Ok(())
        }
    }
}

const DEVICE_COLUMNS: &str = "id, name, paired_at, last_seen_at, last_peer, revoked_at";

fn device_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<DeviceRow> {
    Ok(DeviceRow {
        id: r.get(0)?,
        name: r.get(1)?,
        paired_at: r.get(2)?,
        last_seen_at: r.get(3)?,
        last_peer: r.get(4)?,
        revoked_at: r.get(5)?,
    })
}

/// Add a device and return its new ID, `D-<n>`. The number is the highest of the table plus one.
/// A revoked row stays in the table, so no ID and no token hash is used twice.
fn add_device(tx: &Transaction<'_>, n: &NewDevice) -> Result<String, EffectError> {
    let next: i64 = tx.query_row(
        "SELECT COALESCE(MAX(CAST(substr(id, 3) AS INTEGER)), 0) + 1 FROM devices",
        [],
        |r| r.get(0),
    )?;
    let id = format!("D-{next}");
    let inserted = tx.execute(
        "INSERT INTO devices (id, name, token_sha256, paired_at) VALUES (?1, ?2, ?3, ?4)",
        params![id, n.name, n.token_sha256, n.paired_at],
    );
    match inserted {
        Ok(_) => Ok(id),
        // The unique index of `token_sha256` is the only constraint a caller can break here. The
        // text never holds the hash itself.
        Err(rusqlite::Error::SqliteFailure(e, _))
            if e.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            Err(EffectError::Rejected(
                "a device holds this token already".to_string(),
            ))
        }
        Err(e) => Err(e.into()),
    }
}

/// Mark a live device revoked. The row stays.
fn revoke_device(tx: &Transaction<'_>, id: &str, revoked_at: &str) -> Result<(), EffectError> {
    let row = tx
        .query_row(
            &format!("SELECT {DEVICE_COLUMNS} FROM devices WHERE id = ?1"),
            [id],
            device_from,
        )
        .optional()?;
    match row {
        None => Err(EffectError::Rejected(format!("no device {id}"))),
        Some(d) if d.revoked_at.is_some() => Err(EffectError::Rejected(format!(
            "device {id} is revoked already"
        ))),
        Some(_) => {
            tx.execute(
                "UPDATE devices SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
                params![id, revoked_at],
            )?;
            Ok(())
        }
    }
}

/// The next number of an ID sequence of `table`: the highest of the table plus one. A removed,
/// gone or ended row stays in its table, so no ID is used twice.
fn next_number(tx: &Transaction<'_>, table: &str) -> rusqlite::Result<i64> {
    tx.query_row(
        &format!("SELECT COALESCE(MAX(CAST(substr(id, 3) AS INTEGER)), 0) + 1 FROM {table}"),
        [],
        |r| r.get(0),
    )
}

/// A constraint or trigger that refuses an insert or an update is the backstop of the checks of
/// this file. It is a rejection with the constraint's own text, never a failure of the store.
fn backstop(e: rusqlite::Error) -> EffectError {
    match e {
        rusqlite::Error::SqliteFailure(f, msg)
            if f.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            EffectError::Rejected(
                msg.unwrap_or_else(|| "a constraint refused the write".to_string()),
            )
        }
        other => EffectError::Sql(other),
    }
}

/// A live project, or the reason it is not one.
fn live_project(tx: &Transaction<'_>, id: &str) -> Result<ProjectRow, EffectError> {
    let row = tx
        .query_row(
            &format!("SELECT {PROJECT_COLUMNS} FROM projects WHERE id = ?1"),
            [id],
            project_from,
        )
        .optional()?;
    match row {
        None => Err(EffectError::Rejected(format!("no project {id}"))),
        Some(p) if p.removed_at.is_some() => {
            Err(EffectError::Rejected(format!("project {id} is removed")))
        }
        Some(p) => Ok(p),
    }
}

/// Set the kind of a live project that has none.
fn set_project_kind(
    tx: &Transaction<'_>,
    id: &str,
    kind: ProjectKind,
    remote_key: &Option<String>,
) -> Result<(), EffectError> {
    let project = live_project(tx, id)?;
    if let Some(have) = project.kind {
        return Err(EffectError::Rejected(format!(
            "project {id} has the kind {} already",
            have.as_str()
        )));
    }
    tx.execute(
        "UPDATE projects SET kind = ?2, remote_key = ?3 WHERE id = ?1 AND kind IS NULL",
        params![id, kind.as_str(), remote_key],
    )
    .map_err(backstop)?;
    Ok(())
}

/// Replace the remote key of a live `git` project.
fn set_project_remote_key(
    tx: &Transaction<'_>,
    id: &str,
    remote_key: &Option<String>,
) -> Result<(), EffectError> {
    let project = live_project(tx, id)?;
    if project.kind != Some(ProjectKind::Git) {
        return Err(EffectError::Rejected(format!(
            "project {id} is not a git project"
        )));
    }
    tx.execute(
        "UPDATE projects SET remote_key = ?2 WHERE id = ?1",
        params![id, remote_key],
    )?;
    Ok(())
}

const WORKTREE_COLUMNS: &str = "id, project_id, path, created_by, created_at, gone_at, removed_at";

fn worktree_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<WorktreeRow> {
    let by: String = r.get(3)?;
    Ok(WorktreeRow {
        id: r.get(0)?,
        project_id: r.get(1)?,
        path: r.get(2)?,
        created_by: WorktreeCreatedBy::parse(&by).ok_or_else(|| bad("worktree creator", &by))?,
        created_at: r.get(4)?,
        gone_at: r.get(5)?,
        removed_at: r.get(6)?,
    })
}

const CHAT_SESSION_COLUMNS: &str = "id, project_id, worktree_id, agent_id, runtime, runtime_session_ref, \
     state, model, effort, permission_mode, input_tokens, output_tokens, cache_read_tokens, \
     cache_write_tokens, cost_micros, created_at, updated_at, ended_at, runtime_options";

/// A JSON object of text values, read from a row. Any other shape fails the read.
fn option_map(json: &str) -> rusqlite::Result<BTreeMap<String, String>> {
    serde_json::from_str::<BTreeMap<String, String>>(json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            18,
            rusqlite::types::Type::Text,
            format!("the runtime options are not an object of text values: {e}").into(),
        )
    })
}

fn chat_session_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<ChatSessionRow> {
    let state: String = r.get(6)?;
    Ok(ChatSessionRow {
        id: r.get(0)?,
        project_id: r.get(1)?,
        worktree_id: r.get(2)?,
        agent_id: r.get(3)?,
        runtime: r.get(4)?,
        runtime_session_ref: r.get(5)?,
        state: ChatSessionState::parse(&state).ok_or_else(|| bad("chat session state", &state))?,
        model: r.get(7)?,
        effort: r.get(8)?,
        permission_mode: r.get(9)?,
        input_tokens: to_u64(r.get(10)?),
        output_tokens: to_u64(r.get(11)?),
        cache_read_tokens: to_u64(r.get(12)?),
        cache_write_tokens: to_u64(r.get(13)?),
        cost_micros: to_u64(r.get(14)?),
        created_at: r.get(15)?,
        updated_at: r.get(16)?,
        ended_at: r.get(17)?,
        runtime_options: option_map(&r.get::<_, String>(18)?)?,
    })
}

const CHAT_ITEM_COLUMNS: &str = "session_id, seq, kind, payload, tool_input, tool_input_bytes, \
     tool_input_truncated, tool_output, tool_output_bytes, tool_output_truncated, answers_seq, \
     redacted_at, created_at";

fn chat_item_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<ChatItemRow> {
    Ok(ChatItemRow {
        session_id: r.get(0)?,
        seq: to_u64(r.get(1)?),
        kind: r.get(2)?,
        payload: r.get(3)?,
        tool_input: r.get(4)?,
        tool_input_bytes: r.get::<_, Option<i64>>(5)?.map(to_u64),
        tool_input_truncated: r.get::<_, i64>(6)? != 0,
        tool_output: r.get(7)?,
        tool_output_bytes: r.get::<_, Option<i64>>(8)?.map(to_u64),
        tool_output_truncated: r.get::<_, i64>(9)? != 0,
        answers_seq: r.get::<_, Option<i64>>(10)?.map(to_u64),
        redacted_at: r.get(11)?,
        created_at: r.get(12)?,
    })
}

/// What a chat session start assigned: the session, and the worktree row it used.
struct Started {
    session_id: String,
    worktree_id: String,
}

/// Start a chat session. The checks come in a fixed order, so a rejection names the first cause:
/// the project, the agent, a second live Orchestrator, then the worktree. The triggers and the
/// unique index are the backstop of these checks.
fn start_chat_session(tx: &Transaction<'_>, n: &NewChatSession) -> Result<Started, EffectError> {
    live_project(tx, &n.project_id)?;
    let agent: Option<(String, String, String, String, String)> = tx
        .query_row(
            "SELECT runtime, model, effort, permission_mode, runtime_options FROM agents WHERE id = ?1",
            [&n.agent_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((runtime, model, effort, permission_mode, runtime_options)) = agent else {
        return Err(EffectError::Rejected(format!("no agent {}", n.agent_id)));
    };
    if n.agent_id == ORCHESTRATOR_AGENT {
        let live: Option<String> = tx
            .query_row(
                "SELECT id FROM chat_sessions WHERE project_id = ?1 AND agent_id = ?2 \
                 AND ended_at IS NULL ORDER BY rowid LIMIT 1",
                params![n.project_id, n.agent_id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(session) = live {
            return Err(EffectError::Rejected(format!(
                "project {} has a live Orchestrator session {session}",
                n.project_id
            )));
        }
    }
    let worktree_id = match &n.worktree {
        SessionWorktree::Existing { id } => {
            let row = tx
                .query_row(
                    &format!("SELECT {WORKTREE_COLUMNS} FROM worktrees WHERE id = ?1"),
                    [id],
                    worktree_from,
                )
                .optional()?;
            let Some(w) = row else {
                return Err(EffectError::Rejected(format!("no worktree {id}")));
            };
            if w.project_id != n.project_id {
                return Err(EffectError::Rejected(format!(
                    "worktree {id} belongs to project {}, not {}",
                    w.project_id, n.project_id
                )));
            }
            if w.gone_at.is_some() {
                return Err(EffectError::Rejected(format!("worktree {id} is gone")));
            }
            if w.removed_at.is_some() {
                return Err(EffectError::Rejected(format!("worktree {id} is removed")));
            }
            w.id
        }
        SessionWorktree::Record { path, created_by } => {
            let held: Option<String> = tx
                .query_row(
                    "SELECT id FROM worktrees WHERE path = ?1 AND gone_at IS NULL AND removed_at IS NULL",
                    [path],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(id) = held {
                return Err(EffectError::Rejected(format!(
                    "the live worktree {id} holds the path {path}"
                )));
            }
            let id = format!("W-{}", next_number(tx, "worktrees")?);
            tx.execute(
                "INSERT INTO worktrees (id, project_id, path, created_by, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, n.project_id, path, created_by.as_str(), n.created_at],
            )
            .map_err(backstop)?;
            id
        }
    };
    let session_id = format!("S-{}", next_number(tx, "chat_sessions")?);
    tx.execute(
        "INSERT INTO chat_sessions (id, project_id, worktree_id, agent_id, runtime, state, model, \
             effort, permission_mode, runtime_options, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, 'idle', ?6, ?7, ?8, ?9, ?10, ?10)",
        params![
            session_id,
            n.project_id,
            worktree_id,
            n.agent_id,
            runtime,
            model,
            effort,
            permission_mode,
            runtime_options,
            n.created_at
        ],
    )
    .map_err(backstop)?;
    Ok(Started {
        session_id,
        worktree_id,
    })
}

/// A live chat session, or the reason it is not one.
fn live_chat_session(tx: &Transaction<'_>, id: &str) -> Result<ChatSessionRow, EffectError> {
    let row = tx
        .query_row(
            &format!("SELECT {CHAT_SESSION_COLUMNS} FROM chat_sessions WHERE id = ?1"),
            [id],
            chat_session_from,
        )
        .optional()?;
    match row {
        None => Err(EffectError::Rejected(format!("no chat session {id}"))),
        Some(s) if s.ended_at.is_some() => Err(EffectError::Rejected(format!(
            "chat session {id} is ended already"
        ))),
        Some(s) => Ok(s),
    }
}

fn end_chat_session(tx: &Transaction<'_>, id: &str, ended_at: &str) -> Result<(), EffectError> {
    live_chat_session(tx, id)?;
    tx.execute(
        "UPDATE chat_sessions SET ended_at = ?2 WHERE id = ?1 AND ended_at IS NULL",
        params![id, ended_at],
    )
    .map_err(backstop)?;
    Ok(())
}

fn set_chat_session_runtime(
    tx: &Transaction<'_>,
    id: &str,
    model: &Option<String>,
    effort: &Option<String>,
    permission_mode: &Option<String>,
    option_changes: &Option<BTreeMap<String, String>>,
    updated_at: &str,
) -> Result<(), EffectError> {
    live_chat_session(tx, id)?;
    // The changes merge into the stored map here, inside the commit, so no set builds its map
    // from a read that another set has since replaced. No changes leave the map as it is. An empty
    // stored map takes the agent's options as its base, as the controller reads it.
    let options = match option_changes {
        None => None,
        Some(changes) => {
            let (stored, agent): (String, String) = tx.query_row(
                "SELECT s.runtime_options, COALESCE(a.runtime_options, '{}') FROM chat_sessions s \
                 LEFT JOIN agents a ON a.id = s.agent_id WHERE s.id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let mut map = option_map(&stored)?;
            if map.is_empty() {
                map = option_map(&agent)?;
            }
            map.extend(changes.iter().map(|(k, v)| (k.clone(), v.clone())));
            Some(serde_json::to_string(&map).unwrap_or_else(|_| "{}".to_string()))
        }
    };
    tx.execute(
        "UPDATE chat_sessions SET model = COALESCE(?2, model), effort = COALESCE(?3, effort), \
             permission_mode = COALESCE(?4, permission_mode), \
             runtime_options = COALESCE(?5, runtime_options), updated_at = ?6 \
         WHERE id = ?1 AND ended_at IS NULL",
        params![id, model, effort, permission_mode, options, updated_at],
    )
    .map_err(backstop)?;
    Ok(())
}

/// Redact one chat item: the marks replace the payload and each tool text the item holds.
fn redact_chat_item(
    tx: &Transaction<'_>,
    session_id: &str,
    seq: u64,
    redacted_at: &str,
) -> Result<(), EffectError> {
    let row = tx
        .query_row(
            &format!(
                "SELECT {CHAT_ITEM_COLUMNS} FROM chat_items WHERE session_id = ?1 AND seq = ?2"
            ),
            params![session_id, to_i64(seq)],
            chat_item_from,
        )
        .optional()?;
    match row {
        None => Err(EffectError::Rejected(format!(
            "no chat item {seq} in session {session_id}"
        ))),
        Some(item) if item.redacted_at.is_some() => Err(EffectError::Rejected(format!(
            "chat item {seq} of session {session_id} is redacted already"
        ))),
        Some(_) => {
            tx.execute(
                "UPDATE chat_items SET redacted_at = ?3, payload = ?4, \
                     tool_input = CASE WHEN tool_input IS NULL THEN NULL ELSE ?5 END, \
                     tool_output = CASE WHEN tool_output IS NULL THEN NULL ELSE ?5 END \
                 WHERE session_id = ?1 AND seq = ?2 AND redacted_at IS NULL",
                params![
                    session_id,
                    to_i64(seq),
                    redacted_at,
                    REDACTED_PAYLOAD,
                    REDACTED_TEXT
                ],
            )
            .map_err(backstop)?;
            Ok(())
        }
    }
}

/// The IDs that one effect assigned.
#[derive(Default)]
struct Assigned {
    /// The ID that stands for [`NEW_ID`].
    new_id: Option<String>,
    /// The ID that stands for [`WORKTREE_ID`].
    worktree_id: Option<String>,
}

/// Apply one effect. It returns the IDs the effect assigned.
fn apply(tx: &Transaction<'_>, effect: &Effect) -> Result<Assigned, EffectError> {
    match effect {
        Effect::PutRun(r) => put_run(tx, r)?,
        Effect::PutNode(n) => put_node(tx, n)?,
        Effect::PutSession(s) => put_session(tx, s)?,
        Effect::OpenRequest(n) => open_request(tx, n)?,
        Effect::PutRequest(r) => put_request(tx, r)?,
        Effect::PutClaim(c) => put_claim(tx, c)?,
        Effect::AddProject(n) => {
            return Ok(Assigned {
                new_id: Some(add_project(tx, n)?),
                worktree_id: None,
            });
        }
        Effect::RemoveProject { id, removed_at } => remove_project(tx, id, removed_at)?,
        Effect::AddDevice(n) => {
            return Ok(Assigned {
                new_id: Some(add_device(tx, n)?),
                worktree_id: None,
            });
        }
        Effect::RevokeDevice { id, revoked_at } => revoke_device(tx, id, revoked_at)?,
        Effect::SetProjectKind {
            id,
            kind,
            remote_key,
        } => set_project_kind(tx, id, *kind, remote_key)?,
        Effect::SetProjectRemoteKey { id, remote_key } => {
            set_project_remote_key(tx, id, remote_key)?;
        }
        Effect::StartChatSession(n) => {
            let started = start_chat_session(tx, n)?;
            return Ok(Assigned {
                new_id: Some(started.session_id),
                worktree_id: Some(started.worktree_id),
            });
        }
        Effect::EndChatSession { id, ended_at } => end_chat_session(tx, id, ended_at)?,
        Effect::SetChatSessionRuntime {
            id,
            model,
            effort,
            permission_mode,
            option_changes,
            updated_at,
        } => set_chat_session_runtime(
            tx,
            id,
            model,
            effort,
            permission_mode,
            option_changes,
            updated_at,
        )?,
        Effect::RedactChatItem {
            session_id,
            seq,
            redacted_at,
        } => redact_chat_item(tx, session_id, *seq, redacted_at)?,
    }
    Ok(Assigned::default())
}

fn commit(conn: &mut Connection, c: Commit) -> rusqlite::Result<Committed> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Some(existing) = find_admission(&tx, &c.run_id, &c.command_id)? {
        // Nothing was written, so the transaction ends by itself.
        return Ok(if existing.content == c.content {
            Committed::Replayed(existing)
        } else {
            Committed::Conflict(existing)
        });
    }
    let a = &c.admission;
    tx.execute(
        "INSERT INTO admissions (run_id, command_id, kind, content, outcome, reason, unit_id, role, model, attempt, admitted_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            c.run_id, c.command_id, a.kind, c.content, a.outcome.as_str(), a.reason, a.unit_id,
            a.role, a.model, a.attempt.map(i64::from), a.admitted_at
        ],
    )?;
    let admission_seq = tx.last_insert_rowid();
    let mut assigned: Option<String> = None;
    let mut worktree_assigned: Option<String> = None;
    for effect in &c.effects {
        match apply(&tx, effect) {
            Ok(got) => {
                if got.new_id.is_some() {
                    assigned = got.new_id;
                }
                if got.worktree_id.is_some() {
                    worktree_assigned = got.worktree_id;
                }
            }
            // The transaction ends here without a commit, so it rolls back: nothing is written.
            Err(EffectError::Rejected(why)) => return Ok(Committed::Rejected(why)),
            Err(EffectError::Sql(e)) => return Err(e),
        }
    }
    if let Some(id) = &assigned {
        tx.execute(
            "UPDATE admissions SET unit_id = ?1 WHERE seq = ?2 AND unit_id = ?3",
            params![id, admission_seq, NEW_ID],
        )?;
    }
    let ledger = match &c.ledger {
        None => None,
        Some(entry) => {
            // The first stand-in of each only: a name or a path in the line may hold the same text.
            let mut body = match &assigned {
                Some(id) => entry.body.replacen(NEW_ID, id, 1),
                None => entry.body.clone(),
            };
            if let Some(id) = &worktree_assigned {
                body = body.replacen(WORKTREE_ID, id, 1);
            }
            let seq: i64 = tx.query_row(
                "SELECT COALESCE(MAX(seq), 0) + 1 FROM ledger_outbox WHERE ledger_file = ?1",
                [&entry.ledger_file],
                |r| r.get(0),
            )?;
            tx.execute(
                "INSERT INTO ledger_outbox (ledger_file, seq, run_id, admission_seq, body, written) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 0)",
                params![entry.ledger_file, seq, c.run_id, admission_seq, body],
            )?;
            Some(LedgerLine {
                ledger_file: entry.ledger_file.clone(),
                seq: to_u64(seq),
                run_id: c.run_id.clone(),
                body,
                written: false,
            })
        }
    };
    let admission = find_admission(&tx, &c.run_id, &c.command_id)?
        .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
    tx.commit()?;
    Ok(Committed::Applied { admission, ledger })
}

/// A tool text as the log stores it: the text, the size before any cut, and whether it was cut.
///
/// Text of at most the cap is stored as it is, with the size and the mark of the event, so a text
/// that the adapter capped is never cut again. Text over the cap can only be uncapped: a capped
/// text fits by its contract. The store cuts it at a character boundary, so that content plus the
/// mark fit the cap, and the original size stays in `bytes`.
fn capped(t: &ToolText, what: &str) -> Result<(String, u64, bool), StoreError> {
    if t.text.len() <= TOOL_TEXT_MAX_BYTES {
        return Ok((t.text.clone(), t.bytes, t.truncated));
    }
    if t.truncated {
        return Err(StoreError(format!(
            "the {what} is marked as capped and holds {} bytes, over the cap of {TOOL_TEXT_MAX_BYTES}",
            t.text.len()
        )));
    }
    // The store's own cut is the chat port's: one function and one mark for the adapter, the store
    // and the stream.
    let cut = pf_engine::port::chat::cap(&t.text);
    Ok((cut.text, t.bytes.max(cut.bytes), true))
}

fn store_err(what: impl std::fmt::Display) -> StoreError {
    StoreError(what.to_string())
}

/// Read a JSON object of text values. Any other shape fails the read.
fn text_map(
    conn: &Connection,
    id: &str,
    field: &str,
    json: &str,
) -> Result<BTreeMap<String, String>, StoreError> {
    let mut stmt = conn
        .prepare("SELECT key, value, type FROM json_each(?1)")
        .map_err(sql_error)?;
    let rows = stmt
        .query_map([json], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, rusqlite::types::Value>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(sql_error)?;
    let mut map = BTreeMap::new();
    for row in rows {
        let (key, value, kind) = row.map_err(sql_error)?;
        match (kind.as_str(), value) {
            ("text", rusqlite::types::Value::Text(v)) => {
                map.insert(key, v);
            }
            _ => {
                return Err(store_err(format!(
                    "agent {id}: {field} holds a value for {key:?} that is not text"
                )));
            }
        }
    }
    Ok(map)
}

/// Read a JSON array of text values. Any other shape fails the read.
fn text_list(conn: &Connection, id: &str, json: &str) -> Result<Vec<String>, StoreError> {
    let mut stmt = conn
        .prepare("SELECT value, type FROM json_each(?1) ORDER BY key")
        .map_err(sql_error)?;
    let rows = stmt
        .query_map([json], |r| {
            Ok((
                r.get::<_, rusqlite::types::Value>(0)?,
                r.get::<_, String>(1)?,
            ))
        })
        .map_err(sql_error)?;
    let mut list = Vec::new();
    for row in rows {
        match row.map_err(sql_error)? {
            (rusqlite::types::Value::Text(v), kind) if kind == "text" => list.push(v),
            _ => {
                return Err(store_err(format!(
                    "agent {id}: skills holds a value that is not text"
                )));
            }
        }
    }
    Ok(list)
}

const AGENT_COLUMNS: &str = "id, name, title, built_in, colour, head, instructions, runtime, model, effort, \
     permission_mode, skills, runtime_options, launch_env, created_at";

/// Read the agents that `where_sql` selects, in creation order.
fn read_agents(
    conn: &Connection,
    where_sql: &str,
    args: impl rusqlite::Params,
) -> Result<Vec<AgentRow>, StoreError> {
    let raw = collect(
        conn,
        &format!("SELECT {AGENT_COLUMNS} FROM agents {where_sql} ORDER BY rowid"),
        args,
        |r| {
            Ok((
                AgentRow {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    title: r.get(2)?,
                    built_in: r.get::<_, i64>(3)? != 0,
                    colour: r.get(4)?,
                    head: r.get(5)?,
                    instructions: r.get(6)?,
                    runtime: r.get(7)?,
                    model: r.get(8)?,
                    effort: r.get(9)?,
                    permission_mode: r.get(10)?,
                    skills: Vec::new(),
                    runtime_options: BTreeMap::new(),
                    launch_env: BTreeMap::new(),
                    created_at: r.get(14)?,
                },
                r.get::<_, String>(11)?,
                r.get::<_, String>(12)?,
                r.get::<_, String>(13)?,
            ))
        },
    )
    .map_err(sql_error)?;
    raw.into_iter()
        .map(|(mut a, skills, options, env)| {
            a.skills = text_list(conn, &a.id, &skills)?;
            a.runtime_options = text_map(conn, &a.id, "runtime_options", &options)?;
            a.launch_env = text_map(conn, &a.id, "launch_env", &env)?;
            Ok(a)
        })
        .collect()
}

/// Append one item to a session's display log, in one transaction. Every refusal returns an error
/// before the first write, and the transaction ends without a commit.
fn append_item(conn: &mut Connection, item: &NewChatItem) -> Result<ChatItemRow, StoreError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql_error)?;
    let known: bool = tx
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM chat_sessions WHERE id = ?1)",
            [&item.session_id],
            |r| r.get(0),
        )
        .map_err(sql_error)?;
    if !known {
        return Err(store_err(format!("no chat session {}", item.session_id)));
    }
    let valid: bool = tx
        .query_row("SELECT json_valid(?1)", [&item.payload], |r| r.get(0))
        .map_err(sql_error)?;
    if !valid {
        return Err(store_err("the payload is not JSON"));
    }
    if item.usage.is_some() && item.kind != ChatItemKind::TurnResult {
        return Err(store_err(format!(
            "a {} item carries no usage",
            item.kind.as_str()
        )));
    }
    let seq: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM chat_items WHERE session_id = ?1",
            [&item.session_id],
            |r| r.get(0),
        )
        .map_err(sql_error)?;

    // Requests and answers (C2).
    match (item.kind.answers(), item.answers_seq) {
        (None, None) => {}
        (None, Some(_)) => {
            return Err(store_err(format!(
                "a {} item answers no request",
                item.kind.as_str()
            )));
        }
        (Some(_), None) => {
            return Err(store_err(format!(
                "a {} item names the request it answers",
                item.kind.as_str()
            )));
        }
        (Some(request_kind), Some(answers)) => {
            let outcome: Option<String> = tx
                .query_row(
                    "SELECT json_extract(?1, '$.outcome')",
                    [&item.payload],
                    |r| r.get(0),
                )
                .map_err(sql_error)?;
            if !outcome
                .as_deref()
                .is_some_and(|o| ANSWER_OUTCOMES.contains(&o))
            {
                return Err(store_err(format!(
                    "an answer item's outcome is one of {}",
                    ANSWER_OUTCOMES.join(", ")
                )));
            }
            let found: Option<String> = tx
                .query_row(
                    "SELECT kind FROM chat_items WHERE session_id = ?1 AND seq = ?2",
                    params![item.session_id, to_i64(answers)],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sql_error)?;
            if found.as_deref() != Some(request_kind.as_str()) {
                return Err(store_err(format!(
                    "item {answers} of session {} is no {}",
                    item.session_id,
                    request_kind.as_str()
                )));
            }
            let answered: bool = tx
                .query_row(
                    "SELECT EXISTS (SELECT 1 FROM chat_items WHERE session_id = ?1 AND answers_seq = ?2)",
                    params![item.session_id, to_i64(answers)],
                    |r| r.get(0),
                )
                .map_err(sql_error)?;
            if answered {
                return Err(store_err(format!(
                    "request {answers} of session {} is answered already",
                    item.session_id
                )));
            }
        }
    }

    let input = item
        .tool_input
        .as_ref()
        .map(|t| capped(t, "tool input"))
        .transpose()?;
    let output = item
        .tool_output
        .as_ref()
        .map(|t| capped(t, "tool output"))
        .transpose()?;
    let (in_text, in_bytes, in_cut) = match input {
        Some((text, bytes, cut)) => (Some(text), Some(to_i64(bytes)), cut),
        None => (None, None, false),
    };
    let (out_text, out_bytes, out_cut) = match output {
        Some((text, bytes, cut)) => (Some(text), Some(to_i64(bytes)), cut),
        None => (None, None, false),
    };
    tx.execute(
        "INSERT INTO chat_items (session_id, seq, kind, payload, tool_input, tool_input_bytes, \
             tool_input_truncated, tool_output, tool_output_bytes, tool_output_truncated, \
             answers_seq, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            item.session_id,
            seq,
            item.kind.as_str(),
            item.payload,
            in_text,
            in_bytes,
            i64::from(in_cut),
            out_text,
            out_bytes,
            i64::from(out_cut),
            item.answers_seq.map(to_i64),
            item.created_at
        ],
    )
    .map_err(sql_error)?;
    if let Some(u) = &item.usage {
        tx.execute(
            "UPDATE chat_sessions SET input_tokens = input_tokens + ?2, \
                 output_tokens = output_tokens + ?3, cache_read_tokens = cache_read_tokens + ?4, \
                 cache_write_tokens = cache_write_tokens + ?5, cost_micros = cost_micros + ?6 \
             WHERE id = ?1",
            params![
                item.session_id,
                to_i64(u.input_tokens),
                to_i64(u.output_tokens),
                to_i64(u.cache_read_tokens),
                to_i64(u.cache_write_tokens),
                to_i64(u.cost_micros)
            ],
        )
        .map_err(sql_error)?;
    }
    let row = tx
        .query_row(
            &format!(
                "SELECT {CHAT_ITEM_COLUMNS} FROM chat_items WHERE session_id = ?1 AND seq = ?2"
            ),
            params![item.session_id, seq],
            chat_item_from,
        )
        .map_err(sql_error)?;
    tx.commit().map_err(sql_error)?;
    Ok(row)
}

fn collect<T>(
    conn: &Connection,
    sql: &str,
    args: impl rusqlite::Params,
    map: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> rusqlite::Result<Vec<T>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(args, map)?;
    rows.collect()
}

impl Store for SqliteStore {
    fn commit(&self, c: Commit) -> Result<Committed, StoreError> {
        self.call(move |conn| commit(conn, c).map_err(sql_error))?
    }

    fn run(&self, run_id: &str) -> Result<Option<RunRow>, StoreError> {
        let run_id = run_id.to_string();
        self.call(move |conn| {
            conn.query_row(
                "SELECT id, workflow, ledger_file, state, halted_reason, created_at, updated_at FROM runs WHERE id = ?1",
                [&run_id],
                |r| {
                    let state: String = r.get(3)?;
                    Ok(RunRow {
                        id: r.get(0)?,
                        workflow: r.get(1)?,
                        ledger_file: r.get(2)?,
                        state: RunState::parse(&state).ok_or_else(|| bad("run state", &state))?,
                        halted_reason: r.get(4)?,
                        created_at: r.get(5)?,
                        updated_at: r.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(sql_error)
        })?
    }

    fn nodes(&self, run_id: &str) -> Result<Vec<NodeRow>, StoreError> {
        let run_id = run_id.to_string();
        self.call(move |conn| {
            collect(
                conn,
                "SELECT run_id, id, parent_id, kind, status, reason, role, model, attempt, detail, created_at, updated_at \
                 FROM nodes WHERE run_id = ?1 ORDER BY rowid",
                [&run_id],
                |r| {
                    let kind: String = r.get(3)?;
                    let status: String = r.get(4)?;
                    Ok(NodeRow {
                        run_id: r.get(0)?,
                        id: r.get(1)?,
                        parent_id: r.get(2)?,
                        kind: NodeKind::parse(&kind).ok_or_else(|| bad("node kind", &kind))?,
                        status: Status::parse(&status).ok_or_else(|| bad("status", &status))?,
                        reason: r.get(5)?,
                        role: r.get(6)?,
                        model: r.get(7)?,
                        attempt: u32::try_from(r.get::<_, i64>(8)?).unwrap_or(u32::MAX),
                        detail: r.get(9)?,
                        created_at: r.get(10)?,
                        updated_at: r.get(11)?,
                    })
                },
            )
            .map_err(sql_error)
        })?
    }

    fn sessions(&self, run_id: &str) -> Result<Vec<SessionRow>, StoreError> {
        let run_id = run_id.to_string();
        self.call(move |conn| {
            collect(
                conn,
                "SELECT run_id, id, node_id, unit_id, process_ref, ended, input_tokens, output_tokens, \
                     cache_read_tokens, cache_write_tokens, cost_micros, created_at, updated_at \
                 FROM sessions WHERE run_id = ?1 ORDER BY rowid",
                [&run_id],
                |r| {
                    Ok(SessionRow {
                        run_id: r.get(0)?,
                        id: r.get(1)?,
                        node_id: r.get(2)?,
                        unit_id: r.get(3)?,
                        process_ref: r.get(4)?,
                        ended: r.get::<_, i64>(5)? != 0,
                        input_tokens: to_u64(r.get(6)?),
                        output_tokens: to_u64(r.get(7)?),
                        cache_read_tokens: to_u64(r.get(8)?),
                        cache_write_tokens: to_u64(r.get(9)?),
                        cost_micros: to_u64(r.get(10)?),
                        created_at: r.get(11)?,
                        updated_at: r.get(12)?,
                    })
                },
            )
            .map_err(sql_error)
        })?
    }

    fn requests(&self, run_id: &str) -> Result<Vec<RequestRow>, StoreError> {
        let run_id = run_id.to_string();
        self.call(move |conn| {
            collect(
                conn,
                &format!("SELECT {REQUEST_COLUMNS} FROM requests WHERE run_id = ?1 ORDER BY rowid"),
                [&run_id],
                request_from,
            )
            .map_err(sql_error)
        })?
    }

    fn request(&self, id: &str) -> Result<Option<RequestRow>, StoreError> {
        let id = id.to_string();
        self.call(move |conn| {
            conn.query_row(
                &format!("SELECT {REQUEST_COLUMNS} FROM requests WHERE id = ?1"),
                [&id],
                request_from,
            )
            .optional()
            .map_err(sql_error)
        })?
    }

    fn claims(&self) -> Result<Vec<ClaimRow>, StoreError> {
        self.call(move |conn| {
            collect(
                conn,
                "SELECT generation, token_hash, holder, claimed_at, fenced_at FROM claims ORDER BY generation",
                [],
                |r| {
                    Ok(ClaimRow {
                        generation: to_u64(r.get(0)?),
                        token_hash: r.get(1)?,
                        holder: r.get(2)?,
                        claimed_at: r.get(3)?,
                        fenced_at: r.get(4)?,
                    })
                },
            )
            .map_err(sql_error)
        })?
    }

    fn projects(&self) -> Result<Vec<ProjectRow>, StoreError> {
        self.call(move |conn| {
            collect(
                conn,
                &format!(
                    "SELECT {PROJECT_COLUMNS} FROM projects WHERE removed_at IS NULL ORDER BY rowid"
                ),
                [],
                project_from,
            )
            .map_err(sql_error)
        })?
    }

    fn project(&self, id: &str) -> Result<Option<ProjectRow>, StoreError> {
        let id = id.to_string();
        self.call(move |conn| {
            conn.query_row(
                &format!("SELECT {PROJECT_COLUMNS} FROM projects WHERE id = ?1"),
                [&id],
                project_from,
            )
            .optional()
            .map_err(sql_error)
        })?
    }

    fn devices(&self) -> Result<Vec<DeviceRow>, StoreError> {
        self.call(move |conn| {
            collect(
                conn,
                &format!(
                    "SELECT {DEVICE_COLUMNS} FROM devices \
                     ORDER BY revoked_at IS NOT NULL, rowid"
                ),
                [],
                device_from,
            )
            .map_err(sql_error)
        })?
    }

    fn device_by_token(&self, token_sha256: &str) -> Result<Option<DeviceRow>, StoreError> {
        let token_sha256 = token_sha256.to_string();
        self.call(move |conn| {
            conn.query_row(
                &format!(
                    "SELECT {DEVICE_COLUMNS} FROM devices \
                     WHERE token_sha256 = ?1 AND revoked_at IS NULL"
                ),
                [&token_sha256],
                device_from,
            )
            .optional()
            .map_err(sql_error)
        })?
    }

    fn touch_device(&self, id: &str, at: &str, peer: &str) -> Result<bool, StoreError> {
        let (id, at, peer) = (id.to_string(), at.to_string(), peer.to_string());
        self.call(move |conn| {
            // One statement decides and writes, so the minute holds under any interleaving. A time
            // that SQLite cannot read makes the comparison NULL, and nothing is written.
            conn.execute(
                "UPDATE devices SET last_seen_at = ?2, last_peer = ?3 \
                 WHERE id = ?1 AND revoked_at IS NULL AND (last_seen_at IS NULL OR \
                     CAST(strftime('%s', ?2) AS INTEGER) - CAST(strftime('%s', last_seen_at) AS INTEGER) >= 60)",
                params![id, at, peer],
            )
            .map(|changed| changed > 0)
            .map_err(sql_error)
        })?
    }

    fn admissions(&self, run_id: &str) -> Result<Vec<AdmissionRow>, StoreError> {
        let run_id = run_id.to_string();
        self.call(move |conn| {
            collect(
                conn,
                &format!(
                    "SELECT {ADMISSION_COLUMNS} FROM admissions WHERE run_id = ?1 ORDER BY seq"
                ),
                [&run_id],
                admission_from,
            )
            .map_err(sql_error)
        })?
    }

    fn admission(
        &self,
        run_id: &str,
        command_id: &str,
    ) -> Result<Option<AdmissionRow>, StoreError> {
        let (run_id, command_id) = (run_id.to_string(), command_id.to_string());
        self.call(move |conn| find_admission(conn, &run_id, &command_id).map_err(sql_error))?
    }

    fn unwritten_ledger_lines(&self, ledger_file: &str) -> Result<Vec<LedgerLine>, StoreError> {
        let ledger_file = ledger_file.to_string();
        self.call(move |conn| {
            collect(
                conn,
                "SELECT ledger_file, seq, run_id, body FROM ledger_outbox \
                 WHERE ledger_file = ?1 AND written = 0 ORDER BY seq",
                [&ledger_file],
                |r| {
                    Ok(LedgerLine {
                        ledger_file: r.get(0)?,
                        seq: to_u64(r.get(1)?),
                        run_id: r.get(2)?,
                        body: r.get(3)?,
                        written: false,
                    })
                },
            )
            .map_err(sql_error)
        })?
    }

    fn mark_ledger_written(&self, ledger_file: &str, seq: u64) -> Result<(), StoreError> {
        let ledger_file = ledger_file.to_string();
        self.call(move |conn| {
            conn.execute(
                "UPDATE ledger_outbox SET written = 1 WHERE ledger_file = ?1 AND seq = ?2",
                params![ledger_file, to_i64(seq)],
            )
            .map(|_| ())
            .map_err(sql_error)
        })?
    }

    fn worktree(&self, id: &str) -> Result<Option<WorktreeRow>, StoreError> {
        let id = id.to_string();
        self.call(move |conn| {
            conn.query_row(
                &format!("SELECT {WORKTREE_COLUMNS} FROM worktrees WHERE id = ?1"),
                [&id],
                worktree_from,
            )
            .optional()
            .map_err(sql_error)
        })?
    }

    fn worktrees(&self, project_id: &str) -> Result<Vec<WorktreeRow>, StoreError> {
        let project_id = project_id.to_string();
        self.call(move |conn| {
            collect(
                conn,
                &format!(
                    "SELECT {WORKTREE_COLUMNS} FROM worktrees WHERE project_id = ?1 ORDER BY rowid"
                ),
                [&project_id],
                worktree_from,
            )
            .map_err(sql_error)
        })?
    }

    fn live_worktree_at(&self, path: &str) -> Result<Option<WorktreeRow>, StoreError> {
        let path = path.to_string();
        self.call(move |conn| {
            conn.query_row(
                &format!(
                    "SELECT {WORKTREE_COLUMNS} FROM worktrees \
                     WHERE path = ?1 AND gone_at IS NULL AND removed_at IS NULL"
                ),
                [&path],
                worktree_from,
            )
            .optional()
            .map_err(sql_error)
        })?
    }

    fn agents(&self) -> Result<Vec<AgentRow>, StoreError> {
        self.call(move |conn| read_agents(conn, "", []))?
    }

    fn agent(&self, id: &str) -> Result<Option<AgentRow>, StoreError> {
        let id = id.to_string();
        self.call(move |conn| {
            Ok(read_agents(conn, "WHERE id = ?1", [&id])?
                .into_iter()
                .next())
        })?
    }

    fn chat_session(&self, id: &str) -> Result<Option<ChatSessionRow>, StoreError> {
        let id = id.to_string();
        self.call(move |conn| {
            conn.query_row(
                &format!("SELECT {CHAT_SESSION_COLUMNS} FROM chat_sessions WHERE id = ?1"),
                [&id],
                chat_session_from,
            )
            .optional()
            .map_err(sql_error)
        })?
    }

    fn chat_sessions(&self, project_id: &str) -> Result<Vec<ChatSessionRow>, StoreError> {
        let project_id = project_id.to_string();
        self.call(move |conn| {
            collect(
                conn,
                &format!(
                    "SELECT {CHAT_SESSION_COLUMNS} FROM chat_sessions WHERE project_id = ?1 \
                     ORDER BY rowid"
                ),
                [&project_id],
                chat_session_from,
            )
            .map_err(sql_error)
        })?
    }

    fn live_chat_sessions(&self) -> Result<Vec<ChatSessionRow>, StoreError> {
        self.call(move |conn| {
            collect(
                conn,
                &format!(
                    "SELECT {CHAT_SESSION_COLUMNS} FROM chat_sessions WHERE ended_at IS NULL \
                     ORDER BY rowid"
                ),
                [],
                chat_session_from,
            )
            .map_err(sql_error)
        })?
    }

    fn chat_items_after(
        &self,
        session_id: &str,
        after_seq: u64,
        limit: u32,
    ) -> Result<Vec<ChatItemRow>, StoreError> {
        let session_id = session_id.to_string();
        self.call(move |conn| {
            collect(
                conn,
                &format!(
                    "SELECT {CHAT_ITEM_COLUMNS} FROM chat_items \
                     WHERE session_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3"
                ),
                params![session_id, to_i64(after_seq), i64::from(limit)],
                chat_item_from,
            )
            .map_err(sql_error)
        })?
    }

    fn chat_item(&self, session_id: &str, seq: u64) -> Result<Option<ChatItemRow>, StoreError> {
        let session_id = session_id.to_string();
        self.call(move |conn| {
            conn.query_row(
                &format!(
                    "SELECT {CHAT_ITEM_COLUMNS} FROM chat_items WHERE session_id = ?1 AND seq = ?2"
                ),
                params![session_id, to_i64(seq)],
                chat_item_from,
            )
            .optional()
            .map_err(sql_error)
        })?
    }

    fn pending_requests(&self, session_id: &str) -> Result<Vec<ChatItemRow>, StoreError> {
        let session_id = session_id.to_string();
        self.call(move |conn| {
            collect(
                conn,
                &format!(
                    "SELECT {CHAT_ITEM_COLUMNS} FROM chat_items \
                     WHERE session_id = ?1 AND kind IN ('approval_request', 'question_request') \
                       AND NOT EXISTS (SELECT 1 FROM chat_items a \
                                       WHERE a.session_id = chat_items.session_id \
                                         AND a.answers_seq = chat_items.seq) \
                     ORDER BY seq"
                ),
                [&session_id],
                chat_item_from,
            )
            .map_err(sql_error)
        })?
    }

    fn mark_worktree_gone(&self, id: &str, at: &str) -> Result<bool, StoreError> {
        let (id, at) = (id.to_string(), at.to_string());
        self.call(move |conn| {
            conn.execute(
                "UPDATE worktrees SET gone_at = ?2 \
                 WHERE id = ?1 AND gone_at IS NULL AND removed_at IS NULL",
                params![id, at],
            )
            .map(|changed| changed > 0)
            .map_err(sql_error)
        })?
    }

    fn set_chat_session_state(
        &self,
        id: &str,
        state: ChatSessionState,
        at: &str,
    ) -> Result<bool, StoreError> {
        let (id, at) = (id.to_string(), at.to_string());
        self.call(move |conn| {
            conn.execute(
                "UPDATE chat_sessions SET state = ?2, updated_at = ?3 \
                 WHERE id = ?1 AND ended_at IS NULL",
                params![id, state.as_str(), at],
            )
            .map(|changed| changed > 0)
            .map_err(sql_error)
        })?
    }

    fn set_chat_runtime_ref(
        &self,
        id: &str,
        runtime_session_ref: &str,
        at: &str,
    ) -> Result<bool, StoreError> {
        let (id, runtime_ref, at) = (
            id.to_string(),
            runtime_session_ref.to_string(),
            at.to_string(),
        );
        self.call(move |conn| {
            conn.execute(
                "UPDATE chat_sessions SET runtime_session_ref = ?2, updated_at = ?3 \
                 WHERE id = ?1 AND ended_at IS NULL",
                params![id, runtime_ref, at],
            )
            .map(|changed| changed > 0)
            .map_err(sql_error)
        })?
    }

    fn append_chat_item(&self, item: NewChatItem) -> Result<ChatItemRow, StoreError> {
        self.call(move |conn| append_item(conn, &item))?
    }
}

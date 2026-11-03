//! The store port on SQLite.
//!
//! One commit is one `BEGIN IMMEDIATE` transaction on the writer thread: the receipt, the state
//! writes and the outbox line, together or not at all.

use pf_engine::port::store::{
    AdmissionOutcome, AdmissionRow, ClaimRow, Commit, Committed, Effect, LedgerLine, NewRequest,
    NodeKind, NodeRow, RequestKind, RequestRow, RequestStatus, RunRow, RunState, SessionRow, Store,
    StoreError,
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
    for effect in &c.effects {
        match effect {
            Effect::PutRun(r) => put_run(&tx, r)?,
            Effect::PutNode(n) => put_node(&tx, n)?,
            Effect::PutSession(s) => put_session(&tx, s)?,
            Effect::OpenRequest(n) => open_request(&tx, n)?,
            Effect::PutRequest(r) => put_request(&tx, r)?,
            Effect::PutClaim(c) => put_claim(&tx, c)?,
        }
    }
    let ledger = match &c.ledger {
        None => None,
        Some(entry) => {
            let seq: i64 = tx.query_row(
                "SELECT COALESCE(MAX(seq), 0) + 1 FROM ledger_outbox WHERE ledger_file = ?1",
                [&entry.ledger_file],
                |r| r.get(0),
            )?;
            tx.execute(
                "INSERT INTO ledger_outbox (ledger_file, seq, run_id, admission_seq, body, written) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 0)",
                params![entry.ledger_file, seq, c.run_id, admission_seq, entry.body],
            )?;
            Some(LedgerLine {
                ledger_file: entry.ledger_file.clone(),
                seq: to_u64(seq),
                run_id: c.run_id.clone(),
                body: entry.body.clone(),
                written: false,
            })
        }
    };
    let admission = find_admission(&tx, &c.run_id, &c.command_id)?
        .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
    tx.commit()?;
    Ok(Committed::Applied { admission, ledger })
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
}

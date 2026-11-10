//! Migration `0005` adds the project hierarchy (ADR-0003, ADR-0007: forward-only, and the store
//! never migrates under an active run; ADR-0015): the project kind and remote key, worktrees,
//! agents with the built-in Orchestrator, chat sessions and chat items.
//!
//! Every migration case builds its version-4 store file by hand, from the text of migrations `0001`
//! to `0004` alone, as the binary of `r1.s2` left it. The trigger cases write to a migrated file
//! with plain SQL, so a trigger or a constraint refuses what the store port would never send.

use std::path::Path;

use pf_store_sqlite::{MIGRATIONS, OpenError, SqliteStore};
use rusqlite::{Connection, params};

const MIGRATION_0001: &str = include_str!("../migrations/0001_init.sql");
const MIGRATION_0002: &str = include_str!("../migrations/0002_requests.sql");
const MIGRATION_0003: &str = include_str!("../migrations/0003_projects.sql");
const MIGRATION_0004: &str = include_str!("../migrations/0004_devices.sql");

/// A store file at schema version 4: a run, a request, a claim, two projects (one removed), a
/// device and two admissions (one under `@controller`).
fn version_four(path: &Path, run_state: &str) {
    let conn = Connection::open(path).unwrap();
    for (version, name, sql) in [
        (1, "init", MIGRATION_0001),
        (2, "requests", MIGRATION_0002),
        (3, "projects", MIGRATION_0003),
        (4, "devices", MIGRATION_0004),
    ] {
        conn.execute_batch(sql).unwrap();
        conn.execute(
            "INSERT INTO schema_version (version, name, applied_at) VALUES (?1, ?2, 't0')",
            params![version, name],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO runs (id, workflow, ledger_file, state, created_at, updated_at) \
         VALUES ('run-1', 'wf', '/l.md', ?1, 't0', 't0')",
        [run_state],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO nodes (run_id, id, kind, status, detail, created_at, updated_at) \
         VALUES ('run-1', 'u1', 'unit', 'parked', '{}', 't0', 't0')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO requests (id, revision, run_id, unit_id, stage, kind, reason, body, status, opened_at) \
         VALUES ('R-1', 1, 'run-1', 'u1', 'implement', 'park', 'why', '{}', 'open', 't1')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO claims (generation, token_hash, holder, claimed_at) VALUES (1, ?1, 'me', 't1')",
        [&"a".repeat(64)],
    )
    .unwrap();
    for (id, name, root, removed) in [
        ("P-1", "alpha", "/work/alpha", None),
        ("P-2", "beta", "/work/beta", Some("t3")),
    ] {
        conn.execute(
            "INSERT INTO projects (id, name, root, created_at, removed_at) VALUES (?1, ?2, ?3, 't2', ?4)",
            params![id, name, root, removed],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO devices (id, name, token_sha256, paired_at) VALUES ('D-1', 'macbook', ?1, 't2')",
        [&"b".repeat(64)],
    )
    .unwrap();
    for (run, command, kind) in [
        ("run-1", "open-run:run-1", "open_run"),
        ("@controller", "cmd-1", "project_add"),
    ] {
        conn.execute(
            "INSERT INTO admissions (run_id, command_id, kind, content, outcome, admitted_at) \
             VALUES (?1, ?2, ?3, '{}', 'admitted', 't0')",
            params![run, command, kind],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO ledger_outbox (ledger_file, seq, run_id, admission_seq, body, written) \
         VALUES ('/l.md', 1, 'run-1', 1, 'run opened', 1)",
        [],
    )
    .unwrap();
}

/// Every row of one table, as text, with the named columns only.
fn rows(conn: &Connection, table: &str, columns: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("SELECT {columns} FROM {table} ORDER BY rowid"))
        .unwrap();
    let n = stmt.column_count();
    stmt.query_map([], |r| {
        let mut cells = Vec::new();
        for i in 0..n {
            cells.push(format!("{:?}", r.get_ref(i)?));
        }
        Ok(cells.join("|"))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

/// The tables and columns a version-4 file holds. `projects` is read by its version-4 columns.
fn dump_v4(conn: &Connection) -> Vec<(String, Vec<String>)> {
    let tables = [
        ("runs", "*"),
        ("nodes", "*"),
        ("sessions", "*"),
        ("admissions", "*"),
        ("ledger_outbox", "*"),
        ("requests", "*"),
        ("claims", "*"),
        ("devices", "*"),
        ("projects", "id, name, root, created_at, removed_at"),
    ];
    tables
        .iter()
        .map(|(t, cols)| ((*t).to_string(), rows(conn, t, cols)))
        .collect()
}

/// Every row of every table, to prove that a refused statement wrote nothing.
fn dump_all(conn: &Connection) -> Vec<(String, Vec<String>)> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap();
    let names: Vec<String> = stmt
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    names
        .into_iter()
        .map(|t| {
            let r = rows(conn, &t, "*");
            (t, r)
        })
        .collect()
}

fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

fn versions(conn: &Connection) -> Vec<i64> {
    let mut stmt = conn
        .prepare("SELECT version FROM schema_version ORDER BY version")
        .unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// A migrated, empty store file, opened as plain SQLite.
fn fresh() -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    drop(SqliteStore::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();
    (dir, conn)
}

/// Run `sql`, and check that it fails with `why` in its message and writes nothing.
fn refuses(conn: &Connection, sql: &str, why: &str) {
    let before = dump_all(conn);
    let err = conn
        .execute_batch(sql)
        .expect_err(&format!("this must be refused: {sql}"));
    assert!(
        err.to_string().contains(why),
        "{sql}\n  gave: {err}\n  wanted: {why}"
    );
    assert_eq!(
        dump_all(conn),
        before,
        "a refused statement wrote nothing: {sql}"
    );
}

fn add_project(conn: &Connection, id: &str, root: &str, kind: &str) {
    conn.execute(
        "INSERT INTO projects (id, name, root, created_at, kind) VALUES (?1, ?1, ?2, 't', ?3)",
        params![id, root, kind],
    )
    .unwrap();
}

fn add_worktree(conn: &Connection, id: &str, project: &str, path: &str) {
    conn.execute(
        "INSERT INTO worktrees (id, project_id, path, created_by, created_at) \
         VALUES (?1, ?2, ?3, 'root', 't')",
        params![id, project, path],
    )
    .unwrap();
}

fn session_sql(id: &str, project: &str, worktree: &str, agent: &str) -> String {
    format!(
        "INSERT INTO chat_sessions (id, project_id, worktree_id, agent_id, runtime, state, model, \
             effort, permission_mode, created_at, updated_at) \
         VALUES ('{id}', '{project}', '{worktree}', '{agent}', 'claude-code', 'idle', 'm', 'high', \
             'bypassPermissions', 't', 't')"
    )
}

fn add_session(conn: &Connection, id: &str, project: &str, worktree: &str) {
    conn.execute_batch(&session_sql(id, project, worktree, "orchestrator"))
        .unwrap();
}

fn item_sql(session: &str, seq: u32, kind: &str, answers: Option<u32>) -> String {
    let answers = answers.map_or("NULL".to_string(), |a| a.to_string());
    format!(
        "INSERT INTO chat_items (session_id, seq, kind, payload, answers_seq, created_at) \
         VALUES ('{session}', {seq}, '{kind}', '{{}}', {answers}, 't')"
    )
}

/// One project, one worktree, one session and one tool-call item with both texts.
fn with_a_session() -> (tempfile::TempDir, Connection) {
    let (dir, conn) = fresh();
    add_project(&conn, "P-1", "/work/alpha", "git");
    add_worktree(&conn, "W-1", "P-1", "/work/alpha");
    add_session(&conn, "S-1", "P-1", "W-1");
    conn.execute_batch(
        "INSERT INTO chat_items (session_id, seq, kind, payload, tool_input, tool_input_bytes, \
             tool_input_truncated, tool_output, tool_output_bytes, tool_output_truncated, created_at) \
         VALUES ('S-1', 1, 'tool_call', '{\"name\":\"Bash\"}', 'ls', 2, 0, 'out', 40000, 1, 't')",
    )
    .unwrap();
    (dir, conn)
}

#[test]
fn the_migration_list_has_the_hierarchy_as_version_five() {
    assert_eq!(MIGRATIONS.len(), 7);
    assert_eq!(MIGRATIONS[4].version, 5);
    assert_eq!(MIGRATIONS[4].name, "hierarchy");
}

#[test]
fn a_version_four_store_migrates_to_five_and_keeps_every_row() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    version_four(&path, "ended");
    let before = dump_v4(&Connection::open(&path).unwrap());

    let store = SqliteStore::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), 7);
    drop(store);

    let conn = Connection::open(&path).unwrap();
    assert_eq!(versions(&conn), vec![1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(dump_v4(&conn), before, "no row of version 4 changed");

    // The two new columns are NULL on both projects: the backfill is the controller's (it runs git).
    let kinds: Vec<(Option<String>, Option<String>)> = {
        let mut stmt = conn
            .prepare("SELECT kind, remote_key FROM projects ORDER BY rowid")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(kinds, vec![(None, None), (None, None)]);

    for table in ["worktrees", "chat_sessions", "chat_items"] {
        assert_eq!(count(&conn, table), 0, "{table} starts empty");
    }
    assert_eq!(
        count(&conn, "agents"),
        1,
        "agents holds the Orchestrator alone"
    );
    let seed: (String, String, String, i64, String, String, Option<String>) = conn
        .query_row(
            "SELECT id, name, title, built_in, colour, head, instructions FROM agents",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        seed,
        (
            "orchestrator".to_string(),
            "Orchestrator".to_string(),
            "Runs this project".to_string(),
            1,
            "accent".to_string(),
            "orchestrator".to_string(),
            None,
        )
    );
    let setting: (String, String, String, String, String, String, String) = conn
        .query_row(
            "SELECT runtime, model, effort, permission_mode, skills, runtime_options, launch_env FROM agents",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        setting,
        (
            "claude-code".to_string(),
            "claude-opus-5-5".to_string(),
            "high".to_string(),
            "bypassPermissions".to_string(),
            "[]".to_string(),
            "{\"artifacts\":\"on\"}".to_string(),
            "{}".to_string(),
        )
    );
    let created: String = conn
        .query_row("SELECT created_at FROM agents", [], |r| r.get(0))
        .unwrap();
    assert!(created.ends_with('Z') && created.contains('T'), "{created}");
}

#[test]
fn a_fresh_file_opens_at_five() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    let store = SqliteStore::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), 7);
    drop(store);
    let conn = Connection::open(&path).unwrap();
    assert_eq!(versions(&conn), vec![1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(count(&conn, "agents"), 1);
    assert_eq!(count(&conn, "chat_sessions"), 0);
}

#[test]
fn a_version_four_store_with_an_active_run_refuses_and_stays_at_four() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    version_four(&path, "active");
    let before = dump_v4(&Connection::open(&path).unwrap());

    let err = SqliteStore::open(&path)
        .err()
        .expect("migration 5 must wait for the active run");
    assert_eq!(
        err,
        OpenError::RunActive {
            current: 4,
            pending: 7
        }
    );

    let conn = Connection::open(&path).unwrap();
    assert_eq!(versions(&conn), vec![1, 2, 3, 4], "no version was recorded");
    let made: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name IN ('worktrees', 'agents', 'chat_sessions', 'chat_items')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(made, 0, "no table was made");
    assert_eq!(dump_v4(&conn), before);
}

#[test]
fn a_new_project_has_a_kind_and_a_kind_never_changes() {
    let (_dir, conn) = fresh();
    refuses(
        &conn,
        "INSERT INTO projects (id, name, root, created_at) VALUES ('P-1', 'a', '/a', 't')",
        "a new project has a kind",
    );
    refuses(
        &conn,
        "INSERT INTO projects (id, name, root, created_at, kind) VALUES ('P-1', 'a', '/a', 't', 'repo')",
        "CHECK constraint failed",
    );
    add_project(&conn, "P-1", "/a", "git");
    refuses(
        &conn,
        "UPDATE projects SET kind = 'folder' WHERE id = 'P-1'",
        "a project kind never changes",
    );
    refuses(
        &conn,
        "UPDATE projects SET kind = NULL WHERE id = 'P-1'",
        "a project kind never changes",
    );
    // The same value is not a change, and the key may change any time.
    conn.execute_batch(
        "UPDATE projects SET kind = 'git', remote_key = 'github.com/o/r' WHERE id = 'P-1'",
    )
    .unwrap();
}

#[test]
fn a_kind_is_set_once_on_a_project_that_the_migration_left_without_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    version_four(&path, "ended");
    drop(SqliteStore::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("UPDATE projects SET kind = 'folder' WHERE id = 'P-1'")
        .unwrap();
    refuses(
        &conn,
        "UPDATE projects SET kind = 'git' WHERE id = 'P-1'",
        "a project kind never changes",
    );
}

#[test]
fn a_worktree_belongs_to_a_live_project_and_a_gone_mark_stays() {
    let (_dir, conn) = fresh();
    add_project(&conn, "P-1", "/a", "git");
    conn.execute_batch("UPDATE projects SET removed_at = 't9' WHERE id = 'P-1'")
        .unwrap();
    refuses(
        &conn,
        "INSERT INTO worktrees (id, project_id, path, created_by, created_at) VALUES ('W-1', 'P-1', '/a', 'root', 't')",
        "a worktree belongs to a live project",
    );
    refuses(
        &conn,
        "INSERT INTO worktrees (id, project_id, path, created_by, created_at) VALUES ('W-1', 'P-9', '/a', 'root', 't')",
        "a worktree belongs to a live project",
    );
    add_project(&conn, "P-2", "/b", "git");
    refuses(
        &conn,
        "INSERT INTO worktrees (id, project_id, path, created_by, created_at) VALUES ('W-1', 'P-2', '/b', 'other', 't')",
        "CHECK constraint failed",
    );
    add_worktree(&conn, "W-1", "P-2", "/b");
    conn.execute_batch("UPDATE worktrees SET gone_at = 't1' WHERE id = 'W-1'")
        .unwrap();
    refuses(
        &conn,
        "UPDATE worktrees SET gone_at = NULL WHERE id = 'W-1'",
        "a gone worktree stays gone",
    );
    refuses(
        &conn,
        "UPDATE worktrees SET gone_at = 't2' WHERE id = 'W-1'",
        "a gone worktree stays gone",
    );
}

#[test]
fn the_live_path_index_allows_a_live_row_at_a_path_that_a_gone_row_holds() {
    let (_dir, conn) = fresh();
    add_project(&conn, "P-1", "/a", "git");
    add_worktree(&conn, "W-1", "P-1", "/a");
    refuses(
        &conn,
        "INSERT INTO worktrees (id, project_id, path, created_by, created_at) VALUES ('W-2', 'P-1', '/a', 'root', 't')",
        "UNIQUE constraint failed",
    );
    conn.execute_batch("UPDATE worktrees SET gone_at = 't1' WHERE id = 'W-1'")
        .unwrap();
    add_worktree(&conn, "W-2", "P-1", "/a");
    // A removed row frees its path as well.
    conn.execute_batch("UPDATE worktrees SET removed_at = 't2' WHERE id = 'W-2'")
        .unwrap();
    add_worktree(&conn, "W-3", "P-1", "/a");
    assert_eq!(count(&conn, "worktrees"), 3);
}

#[test]
fn a_chat_session_keeps_its_owner_and_its_end_and_starts_only_in_a_live_worktree() {
    let (_dir, conn) = fresh();
    add_project(&conn, "P-1", "/a", "git");
    add_project(&conn, "P-2", "/b", "git");
    add_worktree(&conn, "W-1", "P-1", "/a");
    add_worktree(&conn, "W-2", "P-2", "/b");
    add_worktree(&conn, "W-3", "P-1", "/c");
    conn.execute_batch("UPDATE worktrees SET gone_at = 't1' WHERE id = 'W-3'")
        .unwrap();

    // The project of the session is the project of its worktree.
    refuses(
        &conn,
        &session_sql("S-1", "P-1", "W-2", "orchestrator"),
        "a chat session's project is its worktree's project",
    );
    refuses(
        &conn,
        &session_sql("S-1", "P-1", "W-9", "orchestrator"),
        "a chat session's project is its worktree's project",
    );
    // A gone worktree starts no session, and neither does a removed one.
    refuses(
        &conn,
        &session_sql("S-1", "P-1", "W-3", "orchestrator"),
        "a chat session cannot start in a gone or removed worktree",
    );
    conn.execute_batch("UPDATE worktrees SET removed_at = 't2' WHERE id = 'W-2'")
        .unwrap();
    refuses(
        &conn,
        &session_sql("S-1", "P-2", "W-2", "orchestrator"),
        "a chat session cannot start in a gone or removed worktree",
    );
    refuses(
        &conn,
        "INSERT INTO chat_sessions (id, project_id, worktree_id, agent_id, runtime, state, model, effort, \
             permission_mode, created_at, updated_at) \
         VALUES ('S-1', 'P-1', 'W-1', 'orchestrator', 'claude-code', 'asleep', 'm', 'high', 'bypassPermissions', 't', 't')",
        "CHECK constraint failed",
    );

    add_session(&conn, "S-1", "P-1", "W-1");
    // The owner never changes, even to the same value.
    refuses(
        &conn,
        "UPDATE chat_sessions SET worktree_id = 'W-3' WHERE id = 'S-1'",
        "a chat session keeps its project, worktree and agent",
    );
    refuses(
        &conn,
        "UPDATE chat_sessions SET project_id = 'P-2' WHERE id = 'S-1'",
        "a chat session keeps its project, worktree and agent",
    );
    refuses(
        &conn,
        "UPDATE chat_sessions SET agent_id = 'other' WHERE id = 'S-1'",
        "a chat session keeps its project, worktree and agent",
    );
    // A state, a ref and an end are plain updates. An end is final.
    conn.execute_batch(
        "UPDATE chat_sessions SET state = 'working', runtime_session_ref = 'abc', updated_at = 't2' WHERE id = 'S-1'",
    )
    .unwrap();
    conn.execute_batch("UPDATE chat_sessions SET ended_at = 't3' WHERE id = 'S-1'")
        .unwrap();
    refuses(
        &conn,
        "UPDATE chat_sessions SET ended_at = NULL WHERE id = 'S-1'",
        "an ended chat session stays ended",
    );
    refuses(
        &conn,
        "UPDATE chat_sessions SET ended_at = 't4' WHERE id = 'S-1'",
        "an ended chat session stays ended",
    );
}

#[test]
fn the_orchestrator_index_allows_a_second_orchestrator_once_the_first_has_ended() {
    let (_dir, conn) = fresh();
    add_project(&conn, "P-1", "/a", "git");
    add_project(&conn, "P-2", "/b", "git");
    add_worktree(&conn, "W-1", "P-1", "/a");
    add_worktree(&conn, "W-2", "P-2", "/b");
    add_session(&conn, "S-1", "P-1", "W-1");
    refuses(
        &conn,
        &session_sql("S-2", "P-1", "W-1", "orchestrator"),
        "UNIQUE constraint failed",
    );
    // Another project has its own Orchestrator, and another agent has no such limit.
    add_session(&conn, "S-2", "P-2", "W-2");
    conn.execute_batch(&session_sql("S-3", "P-1", "W-1", "reviewer"))
        .unwrap();
    conn.execute_batch(&session_sql("S-4", "P-1", "W-1", "reviewer"))
        .unwrap();
    conn.execute_batch("UPDATE chat_sessions SET ended_at = 't9' WHERE id = 'S-1'")
        .unwrap();
    add_session(&conn, "S-5", "P-1", "W-1");
    assert_eq!(count(&conn, "chat_sessions"), 5);
}

#[test]
fn a_chat_item_has_a_session_an_earlier_request_to_answer_and_is_never_deleted() {
    let (_dir, conn) = fresh();
    add_project(&conn, "P-1", "/a", "git");
    add_worktree(&conn, "W-1", "P-1", "/a");
    add_session(&conn, "S-1", "P-1", "W-1");

    refuses(
        &conn,
        &item_sql("S-9", 1, "assistant_text", None),
        "a chat item belongs to a chat session",
    );
    conn.execute_batch(&item_sql("S-1", 1, "assistant_text", None))
        .unwrap();
    conn.execute_batch(&item_sql("S-1", 2, "approval_request", None))
        .unwrap();
    // An answer names an earlier item of its session, and the item must exist.
    refuses(
        &conn,
        &item_sql("S-1", 3, "approval_answer", Some(7)),
        "an answer names an earlier item of its session",
    );
    refuses(
        &conn,
        &item_sql("S-1", 3, "approval_answer", Some(3)),
        "an answer names an earlier item of its session",
    );
    conn.execute_batch(&item_sql("S-1", 3, "approval_answer", Some(2)))
        .unwrap();
    // One answer for one request.
    refuses(
        &conn,
        &item_sql("S-1", 4, "approval_answer", Some(2)),
        "UNIQUE constraint failed",
    );
    // The same sequence number twice is the primary key.
    refuses(
        &conn,
        &item_sql("S-1", 3, "assistant_text", None),
        "UNIQUE constraint failed",
    );
    refuses(
        &conn,
        "INSERT INTO chat_items (session_id, seq, kind, payload, created_at) VALUES ('S-1', 4, 'notice', 'not json', 't')",
        "CHECK constraint failed",
    );
    // The kind is the port's closed list, with no CHECK: a later kind needs no table rebuild.
    conn.execute_batch(&item_sql("S-1", 4, "a_later_kind", None))
        .unwrap();
    refuses(
        &conn,
        "DELETE FROM chat_items WHERE session_id = 'S-1' AND seq = 1",
        "a chat item is never deleted",
    );
    refuses(
        &conn,
        "DELETE FROM chat_items",
        "a chat item is never deleted",
    );
    assert_eq!(count(&conn, "chat_items"), 4);
}

#[test]
fn the_redaction_trigger_allows_exactly_one_update_shape() {
    let (_dir, conn) = with_a_session();
    let guard = "a chat item changes only by one redaction";

    // A plain edit with `redacted_at` left NULL.
    refuses(
        &conn,
        "UPDATE chat_items SET payload = '{\"name\":\"rm\"}' WHERE seq = 1",
        guard,
    );
    refuses(
        &conn,
        "UPDATE chat_items SET tool_output = 'other' WHERE seq = 1",
        guard,
    );
    // A redaction that also changes another column.
    for set in [
        "kind = 'notice'",
        "seq = 9",
        "answers_seq = 1",
        "tool_input_bytes = 3",
        "tool_input_truncated = 1",
        "tool_output_bytes = 5",
        "tool_output_truncated = 0",
        "created_at = 't9'",
        "session_id = 'S-2'",
    ] {
        refuses(
            &conn,
            &format!(
                "UPDATE chat_items SET redacted_at = 't5', payload = '{{\"redacted\":true}}', \
                     tool_input = '[redacted]', tool_output = '[redacted]', {set} WHERE seq = 1"
            ),
            guard,
        );
    }
    // A redaction with another mark text, or one that drops a text it should mark.
    for (payload, input, output) in [
        ("{\"redacted\":false}", "[redacted]", "[redacted]"),
        ("{}", "[redacted]", "[redacted]"),
        ("{\"redacted\":true}", "[gone]", "[redacted]"),
        ("{\"redacted\":true}", "", "[redacted]"),
        ("{\"redacted\":true}", "[redacted]", "[gone]"),
        ("{\"redacted\":true}", "[redacted]", ""),
    ] {
        refuses(
            &conn,
            &format!(
                "UPDATE chat_items SET redacted_at = 't5', payload = '{payload}', \
                     tool_input = '{input}', tool_output = '{output}' WHERE seq = 1"
            ),
            guard,
        );
    }
    for (input, output) in [("NULL", "'[redacted]'"), ("'[redacted]'", "NULL")] {
        refuses(
            &conn,
            &format!(
                "UPDATE chat_items SET redacted_at = 't5', payload = '{{\"redacted\":true}}', \
                     tool_input = {input}, tool_output = {output} WHERE seq = 1"
            ),
            guard,
        );
    }
    // A redaction with no `redacted_at`.
    refuses(
        &conn,
        "UPDATE chat_items SET payload = '{\"redacted\":true}', tool_input = '[redacted]', tool_output = '[redacted]' WHERE seq = 1",
        guard,
    );

    // The one shape: `redacted_at` from NULL to a value, the marks, and nothing else.
    conn.execute_batch(
        "UPDATE chat_items SET redacted_at = 't5', payload = '{\"redacted\":true}', \
             tool_input = '[redacted]', tool_output = '[redacted]' WHERE seq = 1",
    )
    .unwrap();
    let kept: (String, i64, i64, i64, i64, String) = conn
        .query_row(
            "SELECT kind, tool_input_bytes, tool_input_truncated, tool_output_bytes, \
                 tool_output_truncated, created_at FROM chat_items WHERE seq = 1",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        kept,
        ("tool_call".to_string(), 2, 0, 40000, 1, "t".to_string())
    );

    // A second redaction, even to the same values.
    refuses(
        &conn,
        "UPDATE chat_items SET redacted_at = 't6', payload = '{\"redacted\":true}', \
             tool_input = '[redacted]', tool_output = '[redacted]' WHERE seq = 1",
        guard,
    );
    refuses(
        &conn,
        "UPDATE chat_items SET redacted_at = 't5' WHERE seq = 1",
        guard,
    );
}

#[test]
fn a_redaction_does_not_make_a_text_where_the_item_had_none() {
    let (_dir, conn) = with_a_session();
    conn.execute_batch(&item_sql("S-1", 2, "assistant_text", None))
        .unwrap();
    // The item has no tool text: a redaction that gives it one is not a redaction.
    refuses(
        &conn,
        "UPDATE chat_items SET redacted_at = 't5', payload = '{\"redacted\":true}', \
             tool_input = '[redacted]' WHERE seq = 2",
        "a chat item changes only by one redaction",
    );
    conn.execute_batch(
        "UPDATE chat_items SET redacted_at = 't5', payload = '{\"redacted\":true}' WHERE seq = 2",
    )
    .unwrap();
}

#[test]
fn a_tool_text_of_16385_bytes_fails_its_check_and_16384_bytes_fit() {
    let (_dir, conn) = with_a_session();
    for (n, column) in ["tool_input", "tool_output"].into_iter().enumerate() {
        let base = 50 + 10 * u32::try_from(n).unwrap();
        let insert = |seq: u32, text: &str| {
            conn.execute(
                &format!(
                    "INSERT INTO chat_items (session_id, seq, kind, payload, {column}, created_at) \
                     VALUES ('S-1', ?1, 'tool_call', '{{}}', ?2, 't')"
                ),
                params![seq, text],
            )
        };
        let before = dump_all(&conn);
        let err = insert(base, &"a".repeat(16385)).expect_err("16385 bytes");
        assert!(err.to_string().contains("CHECK constraint failed"), "{err}");
        assert_eq!(dump_all(&conn), before);
        // The cap counts bytes, not characters: 8193 two-byte characters are 16386 bytes.
        assert!(insert(base, &"é".repeat(8193)).is_err());
        insert(base + 1, &"a".repeat(16384)).unwrap();
        insert(base + 2, &"é".repeat(8192)).unwrap();
    }
}

#[test]
fn an_agent_keeps_its_maps_as_json_of_the_right_shape() {
    let (_dir, conn) = fresh();
    let insert = |skills: &str, options: &str, env: &str| {
        conn.execute_batch(&format!(
            "INSERT INTO agents VALUES ('a-{}', 'n', 't', 0, 'c', 'h', NULL, 'r', 'm', 'e', 'p', \
                 '{skills}', '{options}', '{env}', 't')",
            skills.len() + options.len() + env.len()
        ))
    };
    insert("[]", "{}", "{}").unwrap();
    insert("[\"a\"]", "{\"k\":\"v\"}", "{\"K\":\"V\"}").unwrap();
    for (skills, options, env) in [
        ("{}", "{}", "{}"),
        ("[]", "[]", "{}"),
        ("[]", "{}", "[]"),
        ("nope", "{}", "{}"),
        ("[]", "nope", "{}"),
        ("[]", "{}", "nope"),
    ] {
        let err = insert(skills, options, env).expect_err("a map of the wrong shape");
        assert!(err.to_string().contains("CHECK constraint failed"), "{err}");
    }
    assert_eq!(count(&conn, "agents"), 3);
}

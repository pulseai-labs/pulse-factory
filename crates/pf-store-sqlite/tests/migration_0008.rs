//! Migration `0008` adds `runs.project_id` (ADR-0003, ADR-0007: forward-only, and the store never
//! migrates under an active run; ADR-0015, `r1.s4.w2`).
//!
//! The migration cases build their version-7 store file by hand, from the text of migrations `0001`
//! to `0007` alone, as the binary of `r1.s4.w4` left it. The guard cases write to a migrated file with
//! plain SQL, so a trigger or a constraint refuses what the store port would never send.

use std::path::Path;

use pf_store_sqlite::{MIGRATIONS, OpenError, SqliteStore};
use rusqlite::{Connection, params};

const MIGRATION_0001: &str = include_str!("../migrations/0001_init.sql");
const MIGRATION_0002: &str = include_str!("../migrations/0002_requests.sql");
const MIGRATION_0003: &str = include_str!("../migrations/0003_projects.sql");
const MIGRATION_0004: &str = include_str!("../migrations/0004_devices.sql");
const MIGRATION_0005: &str = include_str!("../migrations/0005_hierarchy.sql");
const MIGRATION_0006: &str = include_str!("../migrations/0006_session_options.sql");
const MIGRATION_0007: &str = include_str!("../migrations/0007_settings.sql");

/// A store file at schema version 7: runs, a request, a claim, two projects (one removed), a
/// device, a worktree, a chat session with items, a setting and two admissions (one under
/// `@controller`). `run-1` has the given state, and `run-2` is ended.
fn version_seven(path: &Path, run_state: &str) {
    let conn = Connection::open(path).unwrap();
    for (version, name, sql) in [
        (1, "init", MIGRATION_0001),
        (2, "requests", MIGRATION_0002),
        (3, "projects", MIGRATION_0003),
        (4, "devices", MIGRATION_0004),
        (5, "hierarchy", MIGRATION_0005),
        (6, "session_options", MIGRATION_0006),
        (7, "settings", MIGRATION_0007),
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
        "INSERT INTO runs (id, workflow, ledger_file, state, created_at, updated_at) \
         VALUES ('run-2', 'wf', '/l2.md', 'ended', 't0', 't0')",
        [],
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
            "INSERT INTO projects (id, name, root, created_at, removed_at, kind) \
             VALUES (?1, ?2, ?3, 't2', ?4, 'folder')",
            params![id, name, root, removed],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO devices (id, name, token_sha256, paired_at) VALUES ('D-1', 'macbook', ?1, 't2')",
        [&"b".repeat(64)],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO worktrees (id, project_id, path, created_by, created_at) \
         VALUES ('W-1', 'P-1', '/work/alpha', 'root', 't2')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO chat_sessions (id, project_id, worktree_id, agent_id, runtime, state, model, \
         effort, permission_mode, created_at, updated_at) \
         VALUES ('S-1', 'P-1', 'W-1', 'orchestrator', 'claude-code', 'idle', 'm', 'high', 'bypass', 't2', 't2')",
        [],
    )
    .unwrap();
    for seq in 1..=2 {
        conn.execute(
            "INSERT INTO chat_items (session_id, seq, kind, payload, created_at) \
             VALUES ('S-1', ?1, 'operator_message', '{}', 't2')",
            [seq],
        )
        .unwrap();
    }
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
        "INSERT INTO settings (project_id, agent_id, key, value, set_by, set_at) \
         VALUES ('P-1', '', 'chat.self_turn_cap', '3', 'cli', 't2')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ledger_outbox (ledger_file, seq, run_id, admission_seq, body, written) \
         VALUES ('/l.md', 1, 'run-1', 1, 'run opened', 1)",
        [],
    )
    .unwrap();
}

/// Every row of one table, as text.
fn rows(conn: &Connection, table: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
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

const TABLES_OF_7: [&str; 15] = [
    "runs",
    "nodes",
    "sessions",
    "admissions",
    "ledger_outbox",
    "requests",
    "claims",
    "projects",
    "devices",
    "worktrees",
    "agents",
    "chat_sessions",
    "chat_items",
    "settings",
    "schema_version",
];

fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

/// The `runs` columns that exist before 0008, as text, for the unchanged-row check.
fn old_run_cells(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, workflow, ledger_file, state, halted_reason, created_at, updated_at \
             FROM runs ORDER BY rowid",
        )
        .unwrap();
    stmt.query_map([], |r| {
        let mut cells = Vec::new();
        for i in 0..7 {
            cells.push(format!("{:?}", r.get_ref(i)?));
        }
        Ok(cells.join("|"))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

#[test]
fn a_version_seven_file_migrates_to_eight_with_every_row_unchanged_and_no_run_in_a_project() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    version_seven(&path, "ended");
    let (before, old_runs): (Vec<(String, Vec<String>)>, Vec<String>) = {
        let conn = Connection::open(&path).unwrap();
        (
            TABLES_OF_7
                .iter()
                .filter(|t| **t != "runs")
                .map(|t| (t.to_string(), rows(&conn, t)))
                .collect(),
            old_run_cells(&conn),
        )
    };
    {
        let store = SqliteStore::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), 8);
    }
    let conn = Connection::open(&path).unwrap();
    for (table, was) in &before {
        let now = rows(&conn, table);
        if table == "schema_version" {
            // The old rows stay, and one row for 0008 follows.
            assert_eq!(&now[..was.len()], &was[..], "{table}");
            assert_eq!(now.len(), was.len() + 1);
        } else {
            assert_eq!(&now, was, "{table} changed");
        }
    }
    assert_eq!(old_run_cells(&conn), old_runs, "the old columns of runs");
    assert_eq!(count(&conn, "runs"), 2);
    let with_project: i64 = conn
        .query_row(
            "SELECT count(*) FROM runs WHERE project_id IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(with_project, 0, "no existing run is moved to a project");
    let name: String = conn
        .query_row(
            "SELECT name FROM schema_version WHERE version = 8",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(name, "run_project");
    assert_eq!(MIGRATIONS[7].name, "run_project");
    assert_eq!(MIGRATIONS.len(), 8);
}

#[test]
fn a_fresh_file_opens_at_eight() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    let store = SqliteStore::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), 8);
    drop(store);
    let conn = Connection::open(&path).unwrap();
    let has: i64 = conn
        .query_row(
            "SELECT count(*) FROM pragma_table_info('runs') WHERE name = 'project_id'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(has, 1);
}

#[test]
fn a_version_seven_file_with_an_active_run_refuses_and_stays_at_seven() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    version_seven(&path, "active");
    let err = SqliteStore::open(&path).err().expect("must refuse");
    assert_eq!(
        err,
        OpenError::RunActive {
            current: 7,
            pending: 8
        }
    );
    let conn = Connection::open(&path).unwrap();
    let top: i64 = conn
        .query_row("SELECT max(version) FROM schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(top, 7);
    let has: i64 = conn
        .query_row(
            "SELECT count(*) FROM pragma_table_info('runs') WHERE name = 'project_id'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(has, 0, "the refusal changed nothing");
}

/// A fresh migrated file with a live P-1, a removed P-2 and two ended runs.
fn migrated() -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    version_seven(&path, "ended");
    drop(SqliteStore::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();
    (dir, conn)
}

fn set_project(conn: &Connection, run: &str, project: Option<&str>) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE runs SET project_id = ?2 WHERE id = ?1",
        params![run, project],
    )
}

fn insert_run(conn: &Connection, id: &str, project: Option<&str>) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO runs (id, workflow, ledger_file, state, project_id, created_at, updated_at) \
         VALUES (?1, 'wf', '/l.md', 'ended', ?2, 't9', 't9')",
        params![id, project],
    )
}

fn project_of(conn: &Connection, run: &str) -> Option<String> {
    conn.query_row("SELECT project_id FROM runs WHERE id = ?1", [run], |r| {
        r.get(0)
    })
    .unwrap()
}

fn refused(r: rusqlite::Result<usize>, what: &str) {
    assert!(r.is_err(), "{what} must be refused");
}

#[test]
fn each_trigger_fires_on_a_direct_write_that_breaks_it_and_writes_nothing() {
    let (_dir, conn) = migrated();
    refused(
        set_project(&conn, "run-1", Some("P-2")),
        "a removed project",
    );
    refused(
        set_project(&conn, "run-1", Some("P-9")),
        "an unknown project",
    );
    refused(
        insert_run(&conn, "run-3", Some("P-2")),
        "an insert naming a removed project",
    );
    refused(
        insert_run(&conn, "run-3", Some("P-9")),
        "an insert naming an unknown project",
    );
    assert_eq!(count(&conn, "runs"), 2, "a refused insert wrote nothing");
    assert_eq!(project_of(&conn, "run-1"), None);

    // NULL to a live project passes, and the same project again passes.
    set_project(&conn, "run-1", Some("P-1")).unwrap();
    assert_eq!(project_of(&conn, "run-1"), Some("P-1".to_string()));
    set_project(&conn, "run-1", Some("P-1")).unwrap();
    // A change from one project to another, and from a project to NULL, are refused.
    conn.execute(
        "INSERT INTO projects (id, name, root, created_at, removed_at, kind) \
         VALUES ('P-3', 'gamma', '/work/gamma', 't4', NULL, 'folder')",
        [],
    )
    .unwrap();
    refused(
        set_project(&conn, "run-1", Some("P-3")),
        "a change from one project to another",
    );
    refused(
        set_project(&conn, "run-1", None),
        "a change from a project to NULL",
    );
    assert_eq!(project_of(&conn, "run-1"), Some("P-1".to_string()));
    // An insert with a live project passes.
    insert_run(&conn, "run-3", Some("P-3")).unwrap();
    assert_eq!(project_of(&conn, "run-3"), Some("P-3".to_string()));
}

#[test]
fn an_upsert_of_a_run_with_a_project_leaves_the_project_in_place() {
    let (_dir, conn) = migrated();
    set_project(&conn, "run-2", Some("P-1")).unwrap();
    // The statement `put_run` runs: it names no `project_id`, so the column is never overwritten.
    conn.execute(
        "INSERT INTO runs (id, workflow, ledger_file, state, halted_reason, created_at, updated_at) \
         VALUES ('run-2', 'wf2', '/l2.md', 'ended', 'why', 't0', 't7') \
         ON CONFLICT (id) DO UPDATE SET workflow = excluded.workflow, ledger_file = excluded.ledger_file, \
             state = excluded.state, halted_reason = excluded.halted_reason, updated_at = excluded.updated_at",
        [],
    )
    .unwrap();
    assert_eq!(project_of(&conn, "run-2"), Some("P-1".to_string()));
    let workflow: String = conn
        .query_row("SELECT workflow FROM runs WHERE id = 'run-2'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(workflow, "wf2");
}

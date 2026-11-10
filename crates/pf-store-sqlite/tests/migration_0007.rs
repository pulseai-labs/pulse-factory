//! Migration `0007` adds the scoped settings (ADR-0003, ADR-0007: forward-only, and the store never
//! migrates under an active run; ADR-0016).
//!
//! The migration cases build their version-6 store file by hand, from the text of migrations `0001`
//! to `0006` alone, as the binary of `r1.s3` left it. The guard cases write to a migrated file with
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

/// A store file at schema version 6: a run, a request, a claim, two projects (one removed), a
/// device, a worktree, a chat session with items and two admissions (one under `@controller`).
fn version_six(path: &Path, run_state: &str) {
    let conn = Connection::open(path).unwrap();
    for (version, name, sql) in [
        (1, "init", MIGRATION_0001),
        (2, "requests", MIGRATION_0002),
        (3, "projects", MIGRATION_0003),
        (4, "devices", MIGRATION_0004),
        (5, "hierarchy", MIGRATION_0005),
        (6, "session_options", MIGRATION_0006),
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
        "INSERT INTO ledger_outbox (ledger_file, seq, run_id, admission_seq, body, written) \
         VALUES ('/l.md', 1, 'run-1', 1, 'run opened', 1)",
        [],
    )
    .unwrap();
}

/// Every row of one table, as text.
fn rows(conn: &Connection, table: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {} FROM {table} ORDER BY rowid",
            // Migration 0008 adds `project_id` to `runs`: a version-6 dump reads the old columns.
            if table == "runs" {
                "id, workflow, ledger_file, state, halted_reason, created_at, updated_at"
            } else {
                "*"
            }
        ))
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

const TABLES_OF_6: [&str; 14] = [
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
    "schema_version",
];

fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn a_version_six_file_migrates_to_seven_with_every_row_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    version_six(&path, "ended");
    let before: Vec<(String, Vec<String>)> = {
        let conn = Connection::open(&path).unwrap();
        TABLES_OF_6
            .iter()
            .map(|t| (t.to_string(), rows(&conn, t)))
            .collect()
    };
    {
        let store = SqliteStore::open(&path).unwrap();
        // 0008 follows 0007, and the plain open applies both.
        assert_eq!(store.schema_version().unwrap(), 8);
    }
    let conn = Connection::open(&path).unwrap();
    for (table, was) in &before {
        let now = rows(&conn, table);
        if table == "schema_version" {
            // The old rows stay, and one row each for 0007 and 0008 follows.
            assert_eq!(&now[..was.len()], &was[..], "{table}");
            assert_eq!(now.len(), was.len() + 2);
        } else {
            assert_eq!(&now, was, "{table} changed");
        }
    }
    let name: String = conn
        .query_row(
            "SELECT name FROM schema_version WHERE version = 7",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(name, "settings");
    assert_eq!(MIGRATIONS[6].name, "settings");
    assert_eq!(count(&conn, "settings"), 0, "the table starts empty");
}

#[test]
fn a_fresh_file_opens_at_seven_with_an_empty_settings_table() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    let store = SqliteStore::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), 8);
    drop(store);
    let conn = Connection::open(&path).unwrap();
    assert_eq!(count(&conn, "settings"), 0);
}

#[test]
fn a_version_six_file_with_an_active_run_refuses_and_stays_at_six() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    version_six(&path, "active");
    let err = SqliteStore::open(&path).err().expect("must refuse");
    assert_eq!(
        err,
        OpenError::RunActive {
            current: 6,
            pending: 8
        }
    );
    let conn = Connection::open(&path).unwrap();
    let top: i64 = conn
        .query_row("SELECT max(version) FROM schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(top, 6);
    let has: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'settings'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(has, 0, "the refusal changed nothing");
}

/// A fresh migrated file with a live P-1, a removed P-2 and the built-in Orchestrator.
fn migrated() -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    version_six(&path, "ended");
    drop(SqliteStore::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();
    (dir, conn)
}

fn put(
    conn: &Connection,
    project: &str,
    agent: &str,
    key: &str,
    value: &str,
    by: &str,
) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO settings (project_id, agent_id, key, value, set_by, set_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, 't9')",
        params![project, agent, key, value, by],
    )
}

fn refused(r: rusqlite::Result<usize>, what: &str) {
    assert!(r.is_err(), "{what} must be refused");
}

#[test]
fn each_guard_fires_on_a_direct_write_that_breaks_it_and_writes_nothing() {
    let (_dir, conn) = migrated();
    refused(
        put(&conn, "P-2", "", "chat.self_turn_cap", "3", "cli"),
        "a removed project",
    );
    refused(
        put(&conn, "P-9", "", "chat.self_turn_cap", "3", "cli"),
        "an unknown project",
    );
    refused(
        put(&conn, "", "ghost", "chat.self_turn_cap", "3", "cli"),
        "an unknown agent",
    );
    refused(
        put(&conn, "", "", "chat.self_turn_cap", "not json", "cli"),
        "a value that is not JSON",
    );
    refused(
        put(&conn, "", "", "Chat.Cap", "3", "cli"),
        "an upper-case key",
    );
    refused(
        put(&conn, "", "", "chat cap", "3", "cli"),
        "a key with a space",
    );
    refused(put(&conn, "", "", "", "3", "cli"), "an empty key");
    refused(
        put(&conn, "", "", &"k".repeat(65), "3", "cli"),
        "a 65-character key",
    );
    refused(
        put(&conn, "", "", "chat.self_turn_cap", "3", "seat"),
        "a set_by of seat",
    );
    assert_eq!(count(&conn, "settings"), 0, "a refused write wrote nothing");

    // A second row at the same scope and key is a primary-key conflict.
    put(&conn, "", "", "chat.self_turn_cap", "3", "cli").unwrap();
    refused(
        put(&conn, "", "", "chat.self_turn_cap", "4", "cli"),
        "a second row at the same scope and key",
    );
    // An update of the scope or the key is refused; an update of the value is not.
    for column in [
        "project_id = 'P-1'",
        "agent_id = 'orchestrator'",
        "key = 'other'",
    ] {
        let r = conn.execute(&format!("UPDATE settings SET {column}"), []);
        assert!(r.is_err(), "UPDATE SET {column} must be refused");
    }
    conn.execute("UPDATE settings SET value = '5'", []).unwrap();
    assert_eq!(count(&conn, "settings"), 1);
}

#[test]
fn a_server_row_a_project_row_an_agent_row_and_a_pair_row_of_one_key_coexist() {
    let (_dir, conn) = migrated();
    for (project, agent) in [
        ("", ""),
        ("P-1", ""),
        ("", "orchestrator"),
        ("P-1", "orchestrator"),
    ] {
        put(&conn, project, agent, "chat.self_turn_cap", "3", "cli").unwrap();
    }
    assert_eq!(count(&conn, "settings"), 4);
    // `D-<n>` is a valid `set_by` as well.
    put(&conn, "", "", "chat.deny_commands", "[]", "D-12").unwrap();
    assert_eq!(count(&conn, "settings"), 5);
}

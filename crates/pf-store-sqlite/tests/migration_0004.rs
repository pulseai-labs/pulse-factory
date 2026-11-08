//! Migration `0004` adds devices (ADR-0003, ADR-0007: forward-only, and the store never migrates
//! under an active run).
//!
//! Every case builds its version-3 store file by hand, from the text of migrations `0001` to `0003`
//! alone, as the binary of `r1.s2.w1` left it.

use std::path::Path;

use pf_store_sqlite::{MIGRATIONS, OpenError, SqliteStore};
use rusqlite::{Connection, params};

const MIGRATION_0001: &str = include_str!("../migrations/0001_init.sql");
const MIGRATION_0002: &str = include_str!("../migrations/0002_requests.sql");
const MIGRATION_0003: &str = include_str!("../migrations/0003_projects.sql");

/// A store file at schema version 3: a run, a unit, two projects (one removed), a request, a
/// claim, two admissions (one under `@controller`) and an outbox line.
fn version_three(path: &Path, run_state: &str) {
    let conn = Connection::open(path).unwrap();
    for (version, name, sql) in [
        (1, "init", MIGRATION_0001),
        (2, "requests", MIGRATION_0002),
        (3, "projects", MIGRATION_0003),
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

/// Every row of the tables a version-3 file holds, as text, table by table.
fn dump(conn: &Connection) -> Vec<(String, Vec<String>)> {
    let tables = [
        "runs",
        "nodes",
        "sessions",
        "admissions",
        "ledger_outbox",
        "requests",
        "claims",
        "projects",
    ];
    tables
        .iter()
        .map(|t| {
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT {} FROM {t} ORDER BY rowid",
                    // Migration 0005 adds two columns to `projects`: a version-N dump reads the old ones.
                    if *t == "projects" {
                        "id, name, root, created_at, removed_at"
                    } else {
                        "*"
                    }
                ))
                .unwrap();
            let n = stmt.column_count();
            let rows = stmt
                .query_map([], |r| {
                    let mut cells = Vec::new();
                    for i in 0..n {
                        cells.push(format!("{:?}", r.get_ref(i)?));
                    }
                    Ok(cells.join("|"))
                })
                .unwrap()
                .map(Result::unwrap)
                .collect();
            ((*t).to_string(), rows)
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

fn insert_device(conn: &Connection, id: &str, token_sha256: &str) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO devices (id, name, token_sha256, paired_at) VALUES (?1, 'macbook', ?2, 't1')",
        params![id, token_sha256],
    )
}

#[test]
fn the_migration_list_has_devices_as_version_four() {
    assert_eq!(MIGRATIONS.len(), 5);
    assert_eq!(MIGRATIONS[3].version, 4);
    assert_eq!(MIGRATIONS[3].name, "devices");
}

#[test]
fn a_version_three_store_migrates_to_the_latest_and_keeps_every_row() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    version_three(&path, "ended");
    let before = dump(&Connection::open(&path).unwrap());

    let store = SqliteStore::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), 5);
    drop(store);

    let conn = Connection::open(&path).unwrap();
    assert_eq!(versions(&conn), vec![1, 2, 3, 4, 5]);
    assert_eq!(dump(&conn), before, "no row of version 3 changed");
    assert_eq!(count(&conn, "devices"), 0, "the devices table is empty");
    // The columns are exactly the ones the spec names, in its order, and the table is strict.
    let mut stmt = conn.prepare("PRAGMA table_info(devices)").unwrap();
    let columns: Vec<(String, String, bool)> = stmt
        .query_map([], |r| Ok((r.get(1)?, r.get(2)?, r.get::<_, i64>(3)? != 0)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let named = |c: &str, kind: &str, not_null: bool| (c.to_string(), kind.to_string(), not_null);
    assert_eq!(
        columns,
        vec![
            // A STRICT table holds no NULL in its primary key.
            named("id", "TEXT", true),
            named("name", "TEXT", true),
            named("token_sha256", "TEXT", true),
            named("paired_at", "TEXT", true),
            named("last_seen_at", "TEXT", false),
            named("last_peer", "TEXT", false),
            named("revoked_at", "TEXT", false),
        ]
    );
    let strict: i64 = conn
        .query_row(
            "SELECT strict FROM pragma_table_list WHERE name = 'devices'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(strict, 1, "the table is STRICT");
}

#[test]
fn a_fresh_file_opens_at_the_latest() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    let store = SqliteStore::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), 5);
    drop(store);
    let conn = Connection::open(&path).unwrap();
    assert_eq!(versions(&conn), vec![1, 2, 3, 4, 5]);
    assert_eq!(count(&conn, "devices"), 0);
}

#[test]
fn a_version_three_store_with_an_active_run_refuses_and_stays_at_three() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    version_three(&path, "active");
    let before = dump(&Connection::open(&path).unwrap());

    let err = SqliteStore::open(&path)
        .err()
        .expect("migration 4 must wait for the active run");
    assert_eq!(
        err,
        OpenError::RunActive {
            current: 3,
            pending: 5
        }
    );

    let conn = Connection::open(&path).unwrap();
    assert_eq!(versions(&conn), vec![1, 2, 3], "no version was recorded");
    let devices: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'devices'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(devices, 0, "no table was made");
    assert_eq!(dump(&conn), before);
}

#[test]
fn the_token_hash_is_unique_and_the_id_is_the_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    drop(SqliteStore::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();

    insert_device(&conn, "D-1", &"a".repeat(64)).unwrap();
    // The same token hash under another ID is refused, and so is the same ID.
    let err = insert_device(&conn, "D-2", &"a".repeat(64)).expect_err("token_sha256 is unique");
    assert!(err.to_string().contains("token_sha256"), "{err}");
    assert!(insert_device(&conn, "D-1", &"b".repeat(64)).is_err());
    assert_eq!(count(&conn, "devices"), 1);

    // A revoked row still holds its token hash: a revoked token can never be issued again.
    conn.execute("UPDATE devices SET revoked_at = 't2' WHERE id = 'D-1'", [])
        .unwrap();
    assert!(insert_device(&conn, "D-3", &"a".repeat(64)).is_err());
    insert_device(&conn, "D-3", &"c".repeat(64)).unwrap();
    assert_eq!(count(&conn, "devices"), 2);
}

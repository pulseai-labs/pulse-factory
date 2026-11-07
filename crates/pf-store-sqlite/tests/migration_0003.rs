//! Migration `0003` adds projects and the `@controller` guard (ADR-0003, ADR-0007: forward-only,
//! and the store never migrates under an active run).
//!
//! Every case builds its version-2 store file by hand, from the text of migrations `0001` and
//! `0002` alone, as the binary of `r1.s1` left it.

use std::path::Path;

use pf_store_sqlite::{MIGRATIONS, OpenError, SqliteStore};
use rusqlite::{Connection, params};

const MIGRATION_0001: &str = include_str!("../migrations/0001_init.sql");
const MIGRATION_0002: &str = include_str!("../migrations/0002_requests.sql");

/// A store file at schema version 2: a run, two units, one stage, one request, one claim, an
/// admission and an outbox line.
fn version_two(path: &Path, run_state: &str) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(MIGRATION_0001).unwrap();
    conn.execute(
        "INSERT INTO schema_version (version, name, applied_at) VALUES (1, 'init', 't0')",
        [],
    )
    .unwrap();
    conn.execute_batch(MIGRATION_0002).unwrap();
    conn.execute(
        "INSERT INTO schema_version (version, name, applied_at) VALUES (2, 'requests', 't0')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO runs (id, workflow, ledger_file, state, created_at, updated_at) \
         VALUES ('run-1', 'wf', '/l.md', ?1, 't0', 't0')",
        [run_state],
    )
    .unwrap();
    for unit in ["u1", "u2"] {
        conn.execute(
            "INSERT INTO nodes (run_id, id, kind, status, detail, created_at, updated_at) \
             VALUES ('run-1', ?1, 'unit', 'parked', '{}', 't0', 't0')",
            [unit],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO nodes (run_id, id, parent_id, kind, status, detail, created_at, updated_at) \
         VALUES ('run-1', 'u1/implement#1', 'u1', 'stage', 'failed', 'implement', 't0', 't0')",
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
    conn.execute(
        "INSERT INTO admissions (run_id, command_id, kind, content, outcome, admitted_at) \
         VALUES ('run-1', 'open-run:run-1', 'open_run', '{}', 'admitted', 't0')",
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

/// Every row of the tables a version-2 file holds, as text, table by table.
fn dump(conn: &Connection) -> Vec<(String, Vec<String>)> {
    let tables = [
        "runs",
        "nodes",
        "sessions",
        "admissions",
        "ledger_outbox",
        "requests",
        "claims",
    ];
    tables
        .iter()
        .map(|t| {
            let mut stmt = conn
                .prepare(&format!("SELECT * FROM {t} ORDER BY rowid"))
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

fn insert_project(
    conn: &Connection,
    id: &str,
    name: &str,
    root: &str,
    removed_at: Option<&str>,
) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO projects (id, name, root, created_at, removed_at) VALUES (?1, ?2, ?3, 't1', ?4)",
        params![id, name, root, removed_at],
    )
}

#[test]
fn the_migration_list_has_projects_as_version_three() {
    assert_eq!(MIGRATIONS.len(), 3);
    assert_eq!(MIGRATIONS[2].version, 3);
    assert_eq!(MIGRATIONS[2].name, "projects");
}

#[test]
fn a_version_two_store_migrates_to_three_and_keeps_every_row() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    version_two(&path, "ended");
    let before = dump(&Connection::open(&path).unwrap());

    let store = SqliteStore::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), 3);
    drop(store);

    let conn = Connection::open(&path).unwrap();
    assert_eq!(versions(&conn), vec![1, 2, 3]);
    assert_eq!(dump(&conn), before, "no row of version 2 changed");
    assert_eq!(count(&conn, "projects"), 0);
    // The new columns are exactly the ones the spec names, and the table is strict.
    let mut stmt = conn.prepare("PRAGMA table_info(projects)").unwrap();
    let columns: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        columns,
        ["id", "name", "root", "created_at", "removed_at"].map(String::from)
    );
}

#[test]
fn a_fresh_file_opens_at_three() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    let store = SqliteStore::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), 3);
    drop(store);
    let conn = Connection::open(&path).unwrap();
    assert_eq!(versions(&conn), vec![1, 2, 3]);
    assert_eq!(count(&conn, "projects"), 0);
}

#[test]
fn a_version_two_store_with_an_active_run_refuses_and_stays_at_two() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    version_two(&path, "active");
    let before = dump(&Connection::open(&path).unwrap());

    let err = SqliteStore::open(&path)
        .err()
        .expect("migration 3 must wait for the active run");
    assert_eq!(
        err,
        OpenError::RunActive {
            current: 2,
            pending: 3
        }
    );

    let conn = Connection::open(&path).unwrap();
    assert_eq!(versions(&conn), vec![1, 2], "no version was recorded");
    let projects: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'projects'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(projects, 0, "no table was made");
    assert_eq!(dump(&conn), before);
}

#[test]
fn the_two_partial_unique_indexes_hold_for_live_rows_and_not_for_removed_ones() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    drop(SqliteStore::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();

    for index in ["projects_live_root", "projects_live_name"] {
        let sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?1",
                [index],
                |r| r.get(0),
            )
            .unwrap_or_else(|e| panic!("index {index} is missing: {e}"));
        assert!(sql.contains("UNIQUE"), "{sql}");
        assert!(sql.contains("removed_at IS NULL"), "{sql}");
    }

    insert_project(&conn, "P-1", "alpha", "/work/alpha", None).unwrap();
    // A live row with the same root, or the same name, is refused.
    assert!(insert_project(&conn, "P-2", "beta", "/work/alpha", None).is_err());
    assert!(insert_project(&conn, "P-2", "alpha", "/work/beta", None).is_err());

    // A removed row does not hold its root or its name. A live row may take both.
    conn.execute("UPDATE projects SET removed_at = 't2' WHERE id = 'P-1'", [])
        .unwrap();
    insert_project(&conn, "P-2", "alpha", "/work/alpha", None).unwrap();
    // Removed rows may repeat each other.
    insert_project(&conn, "P-3", "alpha", "/work/alpha", Some("t3")).unwrap();
    assert_eq!(count(&conn, "projects"), 3);
}

#[test]
fn a_run_id_with_an_at_sign_is_refused_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    drop(SqliteStore::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();
    let insert = |id: &str| {
        conn.execute(
            "INSERT INTO runs (id, workflow, ledger_file, state, created_at, updated_at) \
             VALUES (?1, 'wf', '/l.md', 'active', 't', 't')",
            [id],
        )
    };

    for id in ["@controller", "run@1", "@"] {
        let err = insert(id).expect_err("a run id with @ must be refused");
        assert!(err.to_string().contains("@"), "{err}");
    }
    assert_eq!(count(&conn, "runs"), 0, "nothing was written");

    // An ordinary id still goes in, and an upsert by id still works.
    insert("run-1").unwrap();
    conn.execute("UPDATE runs SET state = 'ended' WHERE id = 'run-1'", [])
        .unwrap();
    assert_eq!(count(&conn, "runs"), 1);
}

//! The migration that adds the run's armed launch count (ADR-0003's amendment of 2026-11-16,
//! ADR-0007: forward-only; `r2.s3.w2`): one nullable column of `runs`, and the trigger that holds
//! "set once" in the database.
//!
//! Another spine may land a migration first, so this test names no number. It finds the migration
//! by its name in `MIGRATIONS`, builds a store file by hand from every migration before it, as the
//! binary before this item left it, and opens that file with this binary.

use std::collections::BTreeSet;
use std::path::Path;

use pf_engine::port::store::{RunState, Store};
use pf_store_sqlite::{MIGRATIONS, SqliteStore};
use rusqlite::{Connection, params};

const NAME: &str = "run_launch_bound";

fn position() -> usize {
    MIGRATIONS
        .iter()
        .position(|m| m.name == NAME)
        .expect("a migration named run_launch_bound")
}

/// A store file as the binary before this migration left it: every earlier migration and two runs.
fn before_the_migration(path: &Path) {
    let conn = Connection::open(path).unwrap();
    for m in &MIGRATIONS[..position()] {
        conn.execute_batch(m.sql).unwrap();
        conn.execute(
            "INSERT INTO schema_version (version, name, applied_at) VALUES (?1, ?2, 't0')",
            params![m.version, m.name],
        )
        .unwrap();
    }
    for id in ["run-a", "run-b"] {
        conn.execute(
            "INSERT INTO runs (id, workflow, ledger_file, state, created_at, updated_at) \
             VALUES (?1, 'wf', '/l.md', 'ended', 't1', 't1')",
            [id],
        )
        .unwrap();
    }
}

fn columns(conn: &Connection, table: &str) -> BTreeSet<String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    stmt.query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn the_migrations_are_numbered_with_no_gap_and_this_one_adds_the_column() {
    let at = position();
    assert!(at > 0, "this migration is not the first");
    for (i, m) in MIGRATIONS.iter().enumerate() {
        assert_eq!(m.version as usize, i + 1, "{}", m.name);
    }
    assert!(
        MIGRATIONS[at].sql.contains("launch_bound"),
        "the migration holds the column"
    );
}

#[test]
fn a_store_at_the_version_before_it_moves_up_by_one_and_keeps_every_row() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.db");
    before_the_migration(&path);
    let before = Connection::open(&path).unwrap();
    assert!(!columns(&before, "runs").contains("launch_bound"));
    let version_before: u32 = before
        .query_row("SELECT max(version) FROM schema_version", [], |r| r.get(0))
        .unwrap();
    drop(before);

    let store = SqliteStore::open(&path).unwrap();
    let version_after = store.schema_version().unwrap();
    assert_eq!(version_after, version_before + 1, "it moves up by one");
    assert_eq!(
        version_after as usize,
        MIGRATIONS.len(),
        "the store is at the newest version"
    );
    for id in ["run-a", "run-b"] {
        let run = store.run(id).unwrap().expect("the run is kept");
        assert_eq!(run.state, RunState::Ended);
        assert_eq!(run.launch_bound, None, "every run reads NULL");
    }
    let conn = Connection::open(&path).unwrap();
    assert!(columns(&conn, "runs").contains("launch_bound"));
    let rows: i64 = conn
        .query_row("SELECT count(*) FROM runs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 2);
    let applied: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT name FROM schema_version ORDER BY version")
            .unwrap();
        stmt.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(applied.last().map(String::as_str), Some(NAME));
}

#[test]
fn the_database_holds_set_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.db");
    drop(SqliteStore::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();
    conn.execute(
        "INSERT INTO runs (id, workflow, ledger_file, state, created_at, updated_at) \
         VALUES ('run-1', 'wf', '/l.md', 'active', 't1', 't1')",
        [],
    )
    .unwrap();
    // NULL to a value is the one allowed change, and the same value again changes nothing.
    conn.execute("UPDATE runs SET launch_bound = 7 WHERE id = 'run-1'", [])
        .unwrap();
    conn.execute("UPDATE runs SET launch_bound = 7 WHERE id = 'run-1'", [])
        .unwrap();
    for sql in [
        "UPDATE runs SET launch_bound = 8 WHERE id = 'run-1'",
        "UPDATE runs SET launch_bound = NULL WHERE id = 'run-1'",
    ] {
        let err = conn.execute(sql, []).unwrap_err().to_string();
        assert!(err.contains("a run keeps its launch bound"), "{sql}: {err}");
    }
    // A value below 0 is no count.
    let err = conn
        .execute(
            "INSERT INTO runs (id, workflow, ledger_file, state, created_at, updated_at, launch_bound) \
             VALUES ('run-2', 'wf', '/l.md', 'active', 't1', 't1', -1)",
            [],
        )
        .unwrap_err()
        .to_string();
    assert!(err.to_lowercase().contains("constraint"), "{err}");
    let held: i64 = conn
        .query_row(
            "SELECT launch_bound FROM runs WHERE id = 'run-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(held, 7);
}

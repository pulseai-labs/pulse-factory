//! AC-15 of `r0.s1`, kept up to date: the migrations apply once and record their schema versions;
//! a second open changes nothing; a test-supplied migration after the built-in ones is refused
//! while a run is active. The tables are those of `0001` (the spec's Approach 3), `0002`
//! (requests and claims) and `0003` (projects), and none is named for agents or devices.

use std::collections::BTreeSet;
use std::path::Path;

use pf_store_sqlite::{MIGRATIONS, Migration, OpenError, SqliteStore};
use rusqlite::Connection;

/// A migration after the built-in `0001`, `0002` and `0003`.
const SECOND: Migration = Migration {
    version: 4,
    name: "test second",
    sql: "CREATE TABLE extra_for_test (x INTEGER NOT NULL) STRICT;",
};

fn tables(path: &Path) -> BTreeSet<String> {
    let conn = Connection::open(path).unwrap();
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")
        .unwrap();
    stmt.query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn versions(path: &Path) -> Vec<(i64, String)> {
    let conn = Connection::open(path).unwrap();
    let mut stmt = conn
        .prepare("SELECT version, applied_at FROM schema_version ORDER BY version")
        .unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn insert_run(path: &Path, id: &str, state: &str) {
    let conn = Connection::open(path).unwrap();
    conn.execute(
        "INSERT INTO runs (id, workflow, ledger_file, state, halted_reason, created_at, updated_at) \
         VALUES (?1, 'wf', 'ledger.md', ?2, NULL, 't', 't')",
        [id, state],
    )
    .unwrap();
}

#[test]
fn the_migrations_apply_once_and_record_their_schema_versions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.db");
    assert_eq!(MIGRATIONS.len(), 3);
    assert_eq!(MIGRATIONS[0].version, 1);
    assert_eq!(MIGRATIONS[1].version, 2);
    assert_eq!(MIGRATIONS[2].version, 3);

    let store = SqliteStore::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), 3);
    drop(store);
    let first = versions(&path);
    assert_eq!(first.len(), 3);
    assert_eq!(first[0].0, 1);
    assert_eq!(first[1].0, 2);
    assert_eq!(first[2].0, 3);

    // A second open changes nothing: the same rows, the same time stamps, the same tables.
    let tables_before = tables(&path);
    let store = SqliteStore::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), 3);
    drop(store);
    assert_eq!(versions(&path), first);
    assert_eq!(tables(&path), tables_before);
}

#[test]
fn the_tables_of_0001_to_0003_are_exactly_those_of_the_approach_and_no_more() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.db");
    drop(SqliteStore::open(&path).unwrap());
    let found = tables(&path);
    let expected: BTreeSet<String> = [
        "runs",
        "nodes",
        "sessions",
        "admissions",
        "ledger_outbox",
        "schema_version",
        "requests",
        "claims",
        "projects",
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    assert_eq!(found, expected);
    for name in &found {
        for banned in ["agent", "device"] {
            assert!(!name.contains(banned), "table {name} is named for {banned}");
        }
    }
}

#[test]
fn the_columns_the_spec_names_are_there() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.db");
    drop(SqliteStore::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();
    let columns = |table: &str| -> BTreeSet<String> {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap();
        stmt.query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    // nodes: the tree, status, reason, role, model, attempt, timestamps
    for c in [
        "id",
        "parent_id",
        "status",
        "reason",
        "role",
        "model",
        "attempt",
        "created_at",
        "updated_at",
    ] {
        assert!(columns("nodes").contains(c), "nodes.{c}");
    }
    // sessions: token usage by kind and dollar cost
    for c in [
        "input_tokens",
        "output_tokens",
        "cache_read_tokens",
        "cache_write_tokens",
        "cost_micros",
    ] {
        assert!(columns("sessions").contains(c), "sessions.{c}");
    }
    // admissions: command ID as a unique key, content bytes, record
    for c in ["command_id", "content", "outcome", "reason"] {
        assert!(columns("admissions").contains(c), "admissions.{c}");
    }
    // The command ID is unique: a second row with the same run and command ID is refused.
    conn.execute(
        "INSERT INTO admissions (run_id, command_id, kind, content, outcome, admitted_at) \
         VALUES ('r', 'c', 'k', 'x', 'admitted', 't')",
        [],
    )
    .unwrap();
    let dup = conn.execute(
        "INSERT INTO admissions (run_id, command_id, kind, content, outcome, admitted_at) \
         VALUES ('r', 'c', 'k', 'y', 'admitted', 't')",
        [],
    );
    assert!(dup.is_err(), "the command ID must be a unique key");
}

#[test]
fn a_second_migration_is_refused_while_a_run_is_active_and_applies_when_none_is() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.db");
    drop(SqliteStore::open(&path).unwrap());
    insert_run(&path, "run-1", "active");

    let err = SqliteStore::open_with_migrations(&path, &[SECOND])
        .err()
        .expect("a migration must wait for the active run");
    assert!(matches!(err, OpenError::RunActive { .. }), "{err:?}");
    assert!(err.to_string().contains("active"), "{err}");
    assert_eq!(versions(&path).len(), 3, "no version was recorded");
    assert!(!tables(&path).contains("extra_for_test"));

    // The plain open of the same file still works: it has nothing to migrate.
    drop(SqliteStore::open(&path).unwrap());

    // When the run has ended, the migration applies, once.
    Connection::open(&path)
        .unwrap()
        .execute("UPDATE runs SET state = 'ended'", [])
        .unwrap();
    let store = SqliteStore::open_with_migrations(&path, &[SECOND]).unwrap();
    assert_eq!(store.schema_version().unwrap(), 4);
    drop(store);
    assert!(tables(&path).contains("extra_for_test"));
    assert_eq!(
        versions(&path).iter().map(|v| v.0).collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
    // A third open with the same list changes nothing.
    drop(SqliteStore::open_with_migrations(&path, &[SECOND]).unwrap());
    assert_eq!(versions(&path).len(), 4);
}

#[test]
fn a_fresh_file_takes_every_migration_together() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.db");
    let store = SqliteStore::open_with_migrations(&path, &[SECOND]).unwrap();
    assert_eq!(store.schema_version().unwrap(), 4);
}

#[test]
fn migrations_are_forward_only_and_numbered_without_a_gap() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.db");

    let gap = Migration {
        version: 5,
        name: "gap",
        sql: "SELECT 1;",
    };
    assert!(matches!(
        SqliteStore::open_with_migrations(&path, &[gap])
            .err()
            .unwrap(),
        OpenError::BadMigrations(_)
    ));
    let repeat = Migration {
        version: 1,
        name: "again",
        sql: "SELECT 1;",
    };
    assert!(matches!(
        SqliteStore::open_with_migrations(&path, &[repeat])
            .err()
            .unwrap(),
        OpenError::BadMigrations(_)
    ));

    // A file at version 4 does not open in a binary that knows version 3 only: no roll back.
    drop(SqliteStore::open_with_migrations(&path, &[SECOND]).unwrap());
    let err = SqliteStore::open(&path).err().unwrap();
    assert_eq!(
        err,
        OpenError::SchemaNewer {
            database: 4,
            supported: 3
        }
    );
}

#[test]
fn a_migration_that_fails_leaves_no_trace() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.db");
    drop(SqliteStore::open(&path).unwrap());
    let broken = Migration {
        version: 4,
        name: "broken",
        sql: "CREATE TABLE half_done (x INTEGER) STRICT; THIS IS NOT SQL;",
    };
    let err = SqliteStore::open_with_migrations(&path, &[broken])
        .err()
        .unwrap();
    assert!(matches!(err, OpenError::Sqlite(_)), "{err:?}");
    assert!(!tables(&path).contains("half_done"));
    assert_eq!(versions(&path).len(), 3);
}

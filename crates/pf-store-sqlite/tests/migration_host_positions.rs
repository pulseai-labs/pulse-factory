//! Migration `0010` adds the host positions (ADR-0003's amendment of 2026-11-16, ADR-0007:
//! forward-only; ADR-0011's kept-and-resent event test; `r2.s2.w4`).
//!
//! The migration case builds its version-9 store file by hand, from the text of migrations `0001` to
//! `0009` alone, as the binary of `r2.s2.w3` left it, with a launch record and a live host in it.

use std::collections::BTreeSet;
use std::path::Path;

use pf_engine::port::store::Store;
use pf_store_sqlite::{MIGRATIONS, Migration, OpenError, SqliteStore};
use rusqlite::{Connection, params};

const MIGRATION_0001: &str = include_str!("../migrations/0001_init.sql");
const MIGRATION_0002: &str = include_str!("../migrations/0002_requests.sql");
const MIGRATION_0003: &str = include_str!("../migrations/0003_projects.sql");
const MIGRATION_0004: &str = include_str!("../migrations/0004_devices.sql");
const MIGRATION_0005: &str = include_str!("../migrations/0005_hierarchy.sql");
const MIGRATION_0006: &str = include_str!("../migrations/0006_session_options.sql");
const MIGRATION_0007: &str = include_str!("../migrations/0007_settings.sql");
const MIGRATION_0008: &str = include_str!("../migrations/0008_run_project.sql");
const MIGRATION_0009: &str = include_str!("../migrations/0009_launch_records.sql");

const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// A store file at schema version 9: a launch record and a live host.
fn version_nine(path: &Path) {
    let conn = Connection::open(path).unwrap();
    for (version, name, sql) in [
        (1, "init", MIGRATION_0001),
        (2, "requests", MIGRATION_0002),
        (3, "projects", MIGRATION_0003),
        (4, "devices", MIGRATION_0004),
        (5, "hierarchy", MIGRATION_0005),
        (6, "session_options", MIGRATION_0006),
        (7, "settings", MIGRATION_0007),
        (8, "run_project", MIGRATION_0008),
        (9, "launch_records", MIGRATION_0009),
    ] {
        conn.execute_batch(sql).unwrap();
        conn.execute(
            "INSERT INTO schema_version (version, name, applied_at) VALUES (?1, ?2, 't0')",
            params![version, name],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO launch_records (session_kind, session_id, incarnation, revision, launcher, \
         host_unit, host_socket, record, record_sha256, command_id, created_at) \
         VALUES ('seat', 'R-1/u1', 1, 1, 'detached', 'pf-host-s-1', '/run/s.sock', '{}', ?1, 'cmd-r', 't1')",
        [HASH],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO hosts (unit, session_kind, session_id, incarnation, launched_revision, \
         launched_record_sha256, launcher, socket, invocation_id, host_binary, host_sha256, \
         protocol, started_at, command_id) \
         VALUES ('pf-host-s-1', 'seat', 'R-1/u1', 1, 1, ?1, 'detached', '/run/s.sock', 'inv-1', \
         '/bin/pf-host', ?1, 1, 't2', 'cmd-h')",
        [HASH],
    )
    .unwrap();
}

fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

fn names(conn: &Connection, kind: &str) -> BTreeSet<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = ?1 AND name NOT LIKE 'sqlite_%'")
        .unwrap();
    stmt.query_map([kind], |r| r.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn a_version_nine_store_with_a_live_host_migrates_to_ten_and_keeps_every_row() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.db");
    version_nine(&path);
    let before = Connection::open(&path).unwrap();
    let counts_before: Vec<(String, i64)> = names(&before, "table")
        .iter()
        .map(|t| (t.clone(), count(&before, t)))
        .collect();
    drop(before);

    assert_eq!(MIGRATIONS[9].version, 10);
    assert_eq!(MIGRATIONS[9].name, "host_positions");
    let store = SqliteStore::open(&path).unwrap();
    // The plain open applies every later migration too: 0011 to 0013 follow 0010.
    assert_eq!(store.schema_version().unwrap(), 13);
    // The old rows read as before.
    let host = store.host("pf-host-s-1").unwrap().expect("the host row");
    assert!(host.ended_at.is_none());
    assert_eq!(store.live_hosts().unwrap().len(), 1);
    assert!(
        store
            .launch_record("seat", "R-1/u1", 1, 1)
            .unwrap()
            .is_some()
    );
    // No position yet: the host kept none before.
    assert_eq!(store.host_position("pf-host-s-1").unwrap(), None);
    drop(store);

    let after = Connection::open(&path).unwrap();
    for (table, n) in &counts_before {
        if table == "schema_version" {
            assert_eq!(
                count(&after, table),
                n + 4,
                "versions 10 to 13 are recorded"
            );
        } else {
            assert_eq!(count(&after, table), *n, "{table} keeps every row");
        }
    }
    assert_eq!(count(&after, "host_positions"), 0);
    let tables = names(&after, "table");
    assert!(tables.contains("host_positions"));
    let triggers = names(&after, "trigger");
    for t in [
        "host_positions_next_seq",
        "host_positions_never_delete",
        "host_positions_never_replace",
    ] {
        assert!(triggers.contains(t), "trigger {t} in {triggers:?}");
    }
    // 0009's guards are still there.
    for t in ["hosts_end_once", "launch_records_never_update"] {
        assert!(triggers.contains(t), "trigger {t} in {triggers:?}");
    }
}

#[test]
fn the_table_holds_no_key_and_no_secret() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.db");
    drop(SqliteStore::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();
    let mut stmt = conn.prepare("PRAGMA table_info(host_positions)").unwrap();
    let columns: BTreeSet<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let expected: BTreeSet<String> = ["unit", "seq", "position", "command_id", "updated_at"]
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(columns, expected);
    // A position is JSON text, and the unit and the seq are not null.
    assert!(
        conn.execute(
            "INSERT INTO host_positions (unit, seq, position, command_id, updated_at) \
             VALUES ('u', 1, 'not json', 'c', 't')",
            [],
        )
        .is_err()
    );
    assert!(
        conn.execute(
            "INSERT INTO host_positions (unit, seq, position, command_id, updated_at) \
             VALUES ('u', 0, '{}', 'c', 't')",
            [],
        )
        .is_err()
    );
}

#[test]
fn migration_ten_is_forward_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.db");
    let fourteen = Migration {
        version: 14,
        name: "next",
        sql: "CREATE TABLE next_for_test (x INTEGER) STRICT;",
    };
    drop(SqliteStore::open_with_migrations(&path, &[fourteen]).unwrap());
    // A file at version 14 does not open in a binary that knows version 13 only: no roll back.
    let err = SqliteStore::open(&path).err().unwrap();
    assert_eq!(
        err,
        OpenError::SchemaNewer {
            database: 14,
            supported: 13
        }
    );
    // The migration list has no gap, and 0010 sits in the tenth place.
    assert_eq!(MIGRATIONS.len(), 13);
    for (i, m) in MIGRATIONS.iter().enumerate() {
        assert_eq!(m.version as usize, i + 1);
    }
}

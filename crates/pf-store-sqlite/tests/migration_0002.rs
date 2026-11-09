//! Migration `0002` adds requests and claims (ADR-0007: forward-only; ADR-0003: the store never
//! migrates under an active run).
//!
//! Every case builds its store file by hand: a version-1 file is made from migration `0001`'s text
//! alone, as `r0.s1`'s binary left it.

use pf_store_sqlite::{MIGRATIONS, OpenError, SqliteStore};
use rusqlite::{Connection, params};

const MIGRATION_0001: &str = include_str!("../migrations/0001_init.sql");

/// A store file at schema version 1 with one run, two units, one stage and an outbox line.
fn version_one(path: &std::path::Path, run_state: &str) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(MIGRATION_0001).unwrap();
    conn.execute(
        "INSERT INTO schema_version (version, name, applied_at) VALUES (1, 'init', 't0')",
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

fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

fn insert_request(
    conn: &Connection,
    id: &str,
    kind: &str,
    status: &str,
    unit: Option<&str>,
) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO requests (id, revision, run_id, unit_id, stage, kind, reason, body, status, opened_at) \
         VALUES (?1, 1, 'run-1', ?2, 'implement', ?3, 'why', '{}', ?4, 't1')",
        params![id, unit, kind, status],
    )
}

#[test]
fn a_version_one_store_migrates_to_the_latest_and_keeps_every_row() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    version_one(&path, "ended");
    {
        // Migrations 0003 to 0007 follow 0002 in the same open: the file ends at version 7.
        let store = SqliteStore::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), 7);
    }
    let conn = Connection::open(&path).unwrap();
    assert_eq!(count(&conn, "runs"), 1);
    assert_eq!(count(&conn, "nodes"), 3);
    assert_eq!(count(&conn, "admissions"), 1);
    assert_eq!(count(&conn, "ledger_outbox"), 1);
    assert_eq!(count(&conn, "schema_version"), 7);
    let name: String = conn
        .query_row(
            "SELECT name FROM schema_version WHERE version = 2",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(name, "requests");
    // The old data reads back through the port as before.
    let store = SqliteStore::open(&path).unwrap();
    use pf_engine::port::store::Store;
    assert_eq!(store.nodes("run-1").unwrap().len(), 3);
    assert_eq!(store.admissions("run-1").unwrap().len(), 1);
}

#[test]
fn the_migration_list_holds_version_two_after_version_one() {
    let versions: Vec<u32> = MIGRATIONS.iter().map(|m| m.version).collect();
    assert_eq!(versions, vec![1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(MIGRATIONS[1].name, "requests");
}

#[test]
fn a_store_never_migrates_under_an_active_run() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    version_one(&path, "active");
    let err = SqliteStore::open(&path).err().expect("must refuse");
    assert_eq!(
        err,
        OpenError::RunActive {
            current: 1,
            pending: 7
        }
    );
    // The refusal changed nothing.
    let conn = Connection::open(&path).unwrap();
    let tables: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name IN ('requests', 'claims')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tables, 0);
}

#[test]
fn both_tables_exist_with_their_columns() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    drop(SqliteStore::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();
    let columns = |table: &str| -> Vec<String> {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap();
        stmt.query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(
        columns("requests"),
        [
            "id",
            "revision",
            "run_id",
            "unit_id",
            "stage",
            "kind",
            "reason",
            "body",
            "status",
            "opened_at",
            "answered_at",
            "answer",
            "answer_generation"
        ]
    );
    assert_eq!(
        columns("claims"),
        [
            "generation",
            "token_hash",
            "holder",
            "claimed_at",
            "fenced_at"
        ]
    );
}

#[test]
fn a_bad_status_or_kind_fails_its_check() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    drop(SqliteStore::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();

    insert_request(&conn, "R-1", "park", "open", Some("u1")).unwrap();
    let bad_status = insert_request(&conn, "R-2", "park", "pending", Some("u2")).unwrap_err();
    assert!(bad_status.to_string().contains("CHECK"), "{bad_status}");
    let bad_kind = insert_request(&conn, "R-3", "wish", "open", Some("u2")).unwrap_err();
    assert!(bad_kind.to_string().contains("CHECK"), "{bad_kind}");
    // The whole closed list is accepted.
    for (i, kind) in [
        "park",
        "gaps",
        "stop",
        "red_gate",
        "stall",
        "merge_failed",
        "seat_lost",
        "merge_ack",
    ]
    .iter()
    .enumerate()
    {
        let id = format!("R-{}", 10 + i);
        insert_request(&conn, &id, kind, "answered", Some("u9")).unwrap();
    }
    // An id outside `R-<n>` and a claim hash of the wrong length are refused too.
    let bad_id = insert_request(&conn, "X-1", "park", "answered", None).unwrap_err();
    assert!(bad_id.to_string().contains("CHECK"), "{bad_id}");
    let bad_hash = conn
        .execute(
            "INSERT INTO claims (generation, token_hash, holder, claimed_at) VALUES (1, 'abc', 'h', 't')",
            [],
        )
        .unwrap_err();
    assert!(bad_hash.to_string().contains("CHECK"), "{bad_hash}");
}

#[test]
fn a_unit_has_at_most_one_open_request_and_one_claim_is_current() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    drop(SqliteStore::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();

    insert_request(&conn, "R-1", "park", "open", Some("u1")).unwrap();
    let second = insert_request(&conn, "R-2", "stop", "open", Some("u1")).unwrap_err();
    assert!(second.to_string().contains("UNIQUE"), "{second}");
    // An answered request does not count, and a run-level request has no unit.
    insert_request(&conn, "R-3", "stop", "answered", Some("u1")).unwrap();
    insert_request(&conn, "R-4", "merge_ack", "open", None).unwrap();
    insert_request(&conn, "R-5", "merge_ack", "open", None).unwrap();

    let hash = "a".repeat(64);
    let claim = |generation: i64, fenced: Option<&str>| {
        conn.execute(
            "INSERT INTO claims (generation, token_hash, holder, claimed_at, fenced_at) VALUES (?1, ?2, 'h', 't', ?3)",
            params![generation, hash, fenced],
        )
    };
    claim(1, Some("t2")).unwrap();
    claim(2, None).unwrap();
    let two_current = claim(3, None).unwrap_err();
    assert!(two_current.to_string().contains("UNIQUE"), "{two_current}");
    let reused = claim(2, Some("t3")).unwrap_err();
    assert!(reused.to_string().contains("UNIQUE"), "{reused}");
}

#[test]
fn a_store_at_version_six_refuses_to_open_and_names_both_versions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    drop(SqliteStore::open(&path).unwrap());
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "INSERT INTO schema_version (version, name, applied_at) VALUES (8, 'future', 't')",
            [],
        )
        .unwrap();
    }
    let err = SqliteStore::open(&path).err().expect("must refuse");
    assert_eq!(
        err,
        OpenError::SchemaNewer {
            database: 8,
            supported: 7
        }
    );
    let text = err.to_string();
    assert!(
        text.contains("version 8") && text.contains("version 7"),
        "{text}"
    );
}

#[test]
fn migrating_twice_is_a_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    version_one(&path, "ended");
    drop(SqliteStore::open(&path).unwrap());
    let before = {
        let conn = Connection::open(&path).unwrap();
        let applied: String = conn
            .query_row(
                "SELECT applied_at FROM schema_version WHERE version = 2",
                [],
                |r| r.get(0),
            )
            .unwrap();
        (count(&conn, "schema_version"), applied)
    };
    let store = SqliteStore::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), 7);
    drop(store);
    let conn = Connection::open(&path).unwrap();
    let applied: String = conn
        .query_row(
            "SELECT applied_at FROM schema_version WHERE version = 2",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!((count(&conn, "schema_version"), applied), before);
}

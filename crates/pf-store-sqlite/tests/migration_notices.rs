//! Migration `0012` adds the notices (ADR-0003's amendment of 2026-11-21, ADR-0007: forward-only;
//! `r2.s11.w3`): four tables, one forward-only migration, and the triggers that hold each rule.
//!
//! The migration case builds its version-11 store file by hand, from the text of migrations `0001` to
//! `0011` alone, as the binary of `r2.s11.w1` left it, with a request-less run, a project and a
//! live host in it.

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
const MIGRATION_0010: &str = include_str!("../migrations/0010_host_positions.sql");
const MIGRATION_0011: &str = include_str!("../migrations/0011_chat_host_cursor.sql");

/// A store file at schema version 11: a project and a device, as the binary before this item left it.
fn version_eleven(path: &Path) {
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
        (10, "host_positions", MIGRATION_0010),
        (11, "chat_host_cursor", MIGRATION_0011),
    ] {
        conn.execute_batch(sql).unwrap();
        conn.execute(
            "INSERT INTO schema_version (version, name, applied_at) VALUES (?1, ?2, 't0')",
            params![version, name],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO projects (id, name, root, created_at, kind) \
         VALUES ('P-1', 'demo', '/work/demo', 't1', 'folder')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO devices (id, name, token_sha256, paired_at) \
         VALUES ('D-1', 'phone', 'hash-1', 't2')",
        [],
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

fn columns(conn: &Connection, table: &str) -> BTreeSet<String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    stmt.query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(ToString::to_string).collect()
}

/// A fresh store at version 12 and a raw connection to it.
fn fresh() -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.db");
    drop(SqliteStore::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();
    (dir, conn)
}

const NOTICE: &str = "INSERT INTO notices (request_id, revision, event, run_id, project_id, \
     created_at, command_id) VALUES ('R-1', 1, 'opened', 'run-1', 'P-1', 't1', 'c1')";

fn with_notice(conn: &Connection) {
    conn.execute(NOTICE, []).unwrap();
    conn.execute(
        "INSERT INTO notice_deliveries (position, channel, text, state, why, updated_at) \
         VALUES (1, 'app', 'Open.', 'waiting', NULL, 't1')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO notice_targets (target, channel, acked, outcome, updated_at) \
         VALUES ('device:D-1', 'app', 0, 'joined', 't1')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO notice_sends (position, target, sends, last_sent_at) \
         VALUES (1, 'device:D-1', 1, 't1')",
        [],
    )
    .unwrap();
}

fn refused(conn: &Connection, sql: &str) -> String {
    match conn.execute(sql, []) {
        Err(e) => e.to_string(),
        Ok(n) => panic!("`{sql}` changed {n} rows, and must be refused"),
    }
}

#[test]
fn a_version_eleven_store_migrates_to_twelve_and_keeps_every_row() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.db");
    version_eleven(&path);
    let before = Connection::open(&path).unwrap();
    let counts_before: Vec<(String, i64)> = names(&before, "table")
        .iter()
        .map(|t| (t.clone(), count(&before, t)))
        .collect();
    drop(before);

    assert_eq!(MIGRATIONS[11].version, 12);
    assert_eq!(MIGRATIONS[11].name, "notices");
    let store = SqliteStore::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), 14);
    assert_eq!(store.projects().unwrap().len(), 1);
    assert_eq!(store.devices().unwrap().len(), 1);
    // No notice yet, and no target.
    assert_eq!(store.notices_after(0, 10).unwrap().len(), 0);
    assert_eq!(store.newest_notice().unwrap(), 0);
    assert!(store.notice_targets().unwrap().is_empty());
    drop(store);

    let after = Connection::open(&path).unwrap();
    for (table, n) in &counts_before {
        if table == "schema_version" {
            assert_eq!(
                count(&after, table),
                n + 3,
                "versions 12 to 14 are recorded"
            );
        } else {
            assert_eq!(count(&after, table), *n, "{table} keeps every row");
        }
    }
    let tables = names(&after, "table");
    // The plain open also applies 0013, which adds `clear_grants`: that table is not 0012's.
    let added: BTreeSet<String> = tables
        .difference(&counts_before.iter().map(|(t, _)| t.clone()).collect())
        .filter(|t| t.as_str() != "clear_grants")
        .cloned()
        .collect();
    assert_eq!(
        added,
        set(&[
            "notices",
            "notice_deliveries",
            "notice_targets",
            "notice_sends"
        ]),
        "the migration adds the four tables and no other"
    );
    // 0010's guards are still there.
    let triggers = names(&after, "trigger");
    assert!(triggers.contains("host_positions_next_seq"), "{triggers:?}");
}

#[test]
fn the_tables_hold_the_columns_of_the_approach_and_no_secret() {
    let (_d, conn) = fresh();
    assert_eq!(
        columns(&conn, "notices"),
        set(&[
            "position",
            "request_id",
            "revision",
            "event",
            "run_id",
            "project_id",
            "created_at",
            "command_id"
        ])
    );
    assert_eq!(
        columns(&conn, "notice_deliveries"),
        set(&["position", "channel", "text", "state", "why", "updated_at"])
    );
    assert_eq!(
        columns(&conn, "notice_targets"),
        set(&["target", "channel", "acked", "outcome", "updated_at"])
    );
    assert_eq!(
        columns(&conn, "notice_sends"),
        set(&["position", "target", "sends", "last_sent_at"])
    );
    // The check constraints: an event and a state come from a closed list, and a state and its
    // reason agree.
    assert!(
        conn.execute(
            "INSERT INTO notices (request_id, revision, event, run_id, created_at, command_id) \
             VALUES ('R-1', 1, 'pinged', 'run-1', 't', 'c')",
            [],
        )
        .is_err(),
        "an event is opened, revised or closed"
    );
    conn.execute(NOTICE, []).unwrap();
    for (state, why) in [
        ("waiting", "'why'"),
        ("superseded", "NULL"),
        ("done", "NULL"),
    ] {
        assert!(
            conn.execute(
                &format!(
                    "INSERT INTO notice_deliveries (position, channel, text, state, why, updated_at) \
                     VALUES (1, 'app', 'x', '{state}', {why}, 't')"
                ),
                [],
            )
            .is_err(),
            "state {state} with why {why}"
        );
    }
}

#[test]
fn a_notice_is_unique_by_request_revision_and_event_and_never_changes() {
    let (_d, conn) = fresh();
    conn.execute(NOTICE, []).unwrap();
    // The same event of the same revision, a second time.
    assert!(
        conn.execute(
            "INSERT INTO notices (request_id, revision, event, run_id, created_at, command_id) \
             VALUES ('R-1', 1, 'opened', 'run-1', 't', 'c2')",
            [],
        )
        .is_err()
    );
    // Another event of the same revision, and the same event of another revision, are other notices.
    conn.execute(
        "INSERT INTO notices (request_id, revision, event, run_id, created_at, command_id) \
         VALUES ('R-1', 1, 'closed', 'run-1', 't', 'c3')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO notices (request_id, revision, event, run_id, created_at, command_id) \
         VALUES ('R-1', 2, 'revised', 'run-1', 't', 'c4')",
        [],
    )
    .unwrap();
    // The position is one sequence from 1.
    let positions: Vec<i64> = conn
        .prepare("SELECT position FROM notices ORDER BY position")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(positions, vec![1, 2, 3]);
    // A notice never changes and is never deleted.
    assert!(
        refused(
            &conn,
            "UPDATE notices SET run_id = 'run-2' WHERE position = 1"
        )
        .contains("never")
    );
    assert!(refused(&conn, "UPDATE notices SET position = 9 WHERE position = 1").contains("never"));
    assert!(refused(&conn, "DELETE FROM notices WHERE position = 1").contains("never"));
    // `INSERT OR REPLACE` cannot rewrite a row, by position or by event.
    assert!(
        refused(
            &conn,
            "INSERT OR REPLACE INTO notices (position, request_id, revision, event, run_id, \
             created_at, command_id) VALUES (1, 'R-9', 1, 'opened', 'run-9', 't', 'c')"
        )
        .contains("never")
    );
    assert!(
        refused(
            &conn,
            "INSERT OR REPLACE INTO notices (request_id, revision, event, run_id, created_at, \
             command_id) VALUES ('R-1', 1, 'opened', 'run-9', 't', 'c')"
        )
        .contains("never")
    );
    assert_eq!(count(&conn, "notices"), 3);
}

#[test]
fn a_delivery_goes_from_waiting_to_superseded_and_no_other_way() {
    let (_d, conn) = fresh();
    with_notice(&conn);
    // Another change of a waiting row, with its state kept, is refused.
    assert!(
        refused(
            &conn,
            "UPDATE notice_deliveries SET text = 'other' WHERE position = 1"
        )
        .contains("supersed")
    );
    assert!(
        refused(
            &conn,
            "UPDATE notice_deliveries SET channel = 'x' WHERE position = 1"
        )
        .contains("supersed")
    );
    // The one change: waiting to superseded, with a reason.
    conn.execute(
        "UPDATE notice_deliveries SET state = 'superseded', why = 'request_closed', updated_at = 't2' \
         WHERE position = 1 AND channel = 'app'",
        [],
    )
    .unwrap();
    // It never goes back, never changes its reason, and is never deleted or replaced.
    assert!(
        refused(
            &conn,
            "UPDATE notice_deliveries SET state = 'waiting', why = NULL WHERE position = 1"
        )
        .contains("supersed")
    );
    assert!(
        refused(
            &conn,
            "UPDATE notice_deliveries SET why = 'channel_off' WHERE position = 1"
        )
        .contains("supersed")
    );
    assert!(refused(&conn, "DELETE FROM notice_deliveries WHERE position = 1").contains("never"));
    assert!(
        refused(
            &conn,
            "INSERT OR REPLACE INTO notice_deliveries (position, channel, text, state, why, updated_at) \
             VALUES (1, 'app', 'new', 'waiting', NULL, 't3')"
        )
        .contains("never")
    );
    let (state, why): (String, String) = conn
        .query_row(
            "SELECT state, why FROM notice_deliveries WHERE position = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (state.as_str(), why.as_str()),
        ("superseded", "request_closed")
    );
}

#[test]
fn a_target_position_only_rises() {
    let (_d, conn) = fresh();
    with_notice(&conn);
    conn.execute(
        "INSERT INTO notices (request_id, revision, event, run_id, created_at, command_id) \
         VALUES ('R-2', 1, 'opened', 'run-1', 't', 'c9')",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE notice_targets SET acked = 2, outcome = 'shown', updated_at = 't2' \
         WHERE target = 'device:D-1'",
        [],
    )
    .unwrap();
    // A position that falls, or stays, is refused; so is a change of the key or the channel.
    for sql in [
        "UPDATE notice_targets SET acked = 1 WHERE target = 'device:D-1'",
        "UPDATE notice_targets SET acked = 2, outcome = 'blocked' WHERE target = 'device:D-1'",
        "UPDATE notice_targets SET target = 'device:D-2', acked = 3 WHERE target = 'device:D-1'",
        "UPDATE notice_targets SET channel = 'orchestrator', acked = 3 WHERE target = 'device:D-1'",
    ] {
        assert!(refused(&conn, sql).contains("rise"), "{sql}");
    }
    assert!(
        refused(
            &conn,
            "DELETE FROM notice_targets WHERE target = 'device:D-1'"
        )
        .contains("never")
    );
    assert!(
        refused(
            &conn,
            "INSERT OR REPLACE INTO notice_targets (target, channel, acked, outcome, updated_at) \
             VALUES ('device:D-1', 'app', 0, 'joined', 't')"
        )
        .contains("never")
    );
    let acked: i64 = conn
        .query_row(
            "SELECT acked FROM notice_targets WHERE target = 'device:D-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(acked, 2);
    // A negative position is no position.
    assert!(
        conn.execute(
            "INSERT INTO notice_targets (target, channel, acked, outcome, updated_at) \
             VALUES ('device:D-2', 'app', -1, 'joined', 't')",
            [],
        )
        .is_err()
    );
}

#[test]
fn a_send_count_rises_by_one_and_a_send_row_is_never_deleted_or_replaced() {
    let (_d, conn) = fresh();
    with_notice(&conn);
    // The count skips from 1 to 3, repeats, or falls: refused. The key never changes.
    for sql in [
        "UPDATE notice_sends SET sends = 3 WHERE position = 1",
        "UPDATE notice_sends SET sends = 1 WHERE position = 1",
        "UPDATE notice_sends SET sends = 0 WHERE position = 1",
        "UPDATE notice_sends SET sends = 2, target = 'device:D-2' WHERE position = 1",
    ] {
        assert!(refused(&conn, sql).contains("next"), "{sql}");
    }
    conn.execute(
        "UPDATE notice_sends SET sends = 2, last_sent_at = 't2' WHERE position = 1",
        [],
    )
    .unwrap();
    assert!(refused(&conn, "DELETE FROM notice_sends WHERE position = 1").contains("never"));
    assert!(
        refused(
            &conn,
            "INSERT OR REPLACE INTO notice_sends (position, target, sends, last_sent_at) \
             VALUES (1, 'device:D-1', 1, 't9')"
        )
        .contains("never")
    );
    // A first send starts at 1.
    assert!(
        conn.execute(
            "INSERT INTO notice_sends (position, target, sends, last_sent_at) \
             VALUES (1, 'device:D-2', 2, 't')",
            [],
        )
        .is_err()
    );
}

#[test]
fn the_triggers_are_named_and_migration_twelve_is_forward_only() {
    let (_d, conn) = fresh();
    let triggers = names(&conn, "trigger");
    for t in [
        "notices_never_update",
        "notices_never_delete",
        "notices_never_replace",
        "notice_deliveries_supersede_only",
        "notice_deliveries_never_delete",
        "notice_deliveries_never_replace",
        "notice_targets_rise_only",
        "notice_targets_never_delete",
        "notice_targets_never_replace",
        "notice_sends_next_only",
        "notice_sends_never_delete",
        "notice_sends_never_replace",
    ] {
        assert!(triggers.contains(t), "trigger {t} in {triggers:?}");
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.db");
    let fifteen = Migration {
        version: 15,
        name: "next",
        sql: "CREATE TABLE next_for_test (x INTEGER) STRICT;",
    };
    drop(SqliteStore::open_with_migrations(&path, &[fifteen]).unwrap());
    // A file at version 15 does not open in a binary that knows version 14 only: no roll back.
    let err = SqliteStore::open(&path).err().unwrap();
    assert_eq!(
        err,
        OpenError::SchemaNewer {
            database: 15,
            supported: 14
        }
    );
    assert_eq!(MIGRATIONS.len(), 14);
    for (i, m) in MIGRATIONS.iter().enumerate() {
        assert_eq!(m.version as usize, i + 1);
    }
}

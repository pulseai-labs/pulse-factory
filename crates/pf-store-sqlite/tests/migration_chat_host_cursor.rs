//! Migration `0011` adds the position of a chat session's output in its host (ADR-0003's
//! amendment of 2026-11-16, ADR-0007: forward-only; `r2.s2.w5`).
//!
//! The migration case builds its version-10 store file by hand, from the text of migrations `0001`
//! to `0010` alone, as the binary of `r2.s2.w4` left it. The guard cases write to a migrated file with
//! plain SQL, so a constraint refuses what the store port would never send.

use std::collections::BTreeSet;
use std::path::Path;

use pf_store_sqlite::{MIGRATIONS, SqliteStore};
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

/// A store file at schema version 10, with one live chat session `S-1` and one item.
fn version_ten(path: &Path) {
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
         VALUES ('P-1', 'alpha', '/work/alpha', 't1', 'folder')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO worktrees (id, project_id, path, created_by, created_at) \
         VALUES ('W-1', 'P-1', '/work/alpha', 'root', 't1')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO chat_sessions (id, project_id, worktree_id, agent_id, runtime, state, model, \
         effort, permission_mode, created_at, updated_at) \
         VALUES ('S-1', 'P-1', 'W-1', 'orchestrator', 'claude-code', 'idle', 'm', 'high', 'p', 't2', 't2')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO chat_items (session_id, seq, kind, payload, created_at) \
         VALUES ('S-1', 1, 'notice', '{}', 't3')",
        [],
    )
    .unwrap();
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

fn trigger_sql(conn: &Connection, name: &str) -> String {
    conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = ?1",
        [name],
        |r| r.get(0),
    )
    .unwrap()
}

fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn the_migration_adds_one_nullable_column_and_leaves_every_existing_row_null() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.db");
    version_ten(&path);
    let before = Connection::open(&path).unwrap();
    let tables_before = names(&before, "table");
    let triggers_before = names(&before, "trigger");
    let counts_before: Vec<(String, i64)> = tables_before
        .iter()
        .map(|t| (t.clone(), count(&before, t)))
        .collect();
    drop(before);

    assert_eq!(MIGRATIONS.len(), 11);
    assert_eq!(MIGRATIONS[10].version, 11);
    assert_eq!(MIGRATIONS[10].name, "chat_host_cursor");
    let store = SqliteStore::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), 11);
    drop(store);

    let after = Connection::open(&path).unwrap();
    // One new column, nullable, text.
    let (kind, not_null): (String, i64) = after
        .query_row(
            "SELECT type, \"notnull\" FROM pragma_table_info('chat_sessions') WHERE name = 'host_cursor'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((kind.as_str(), not_null), ("TEXT", 0));
    // Every existing row reads NULL.
    let nulls: i64 = after
        .query_row(
            "SELECT count(*) FROM chat_sessions WHERE host_cursor IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(nulls, count(&after, "chat_sessions"));
    assert_eq!(nulls, 1);
    // No table and no trigger came or went, and no row changed in number.
    assert_eq!(names(&after, "table"), tables_before);
    assert_eq!(names(&after, "trigger"), triggers_before);
    for (table, n) in &counts_before {
        if table == "schema_version" {
            assert_eq!(count(&after, table), n + 1, "the new version is recorded");
        } else {
            assert_eq!(count(&after, table), *n, "{table} keeps every row");
        }
    }
}

#[test]
fn a_cursor_that_is_not_json_is_refused_and_json_or_null_is_taken() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.db");
    version_ten(&path);
    drop(SqliteStore::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();

    let refused = conn
        .execute(
            "UPDATE chat_sessions SET host_cursor = 'not json' WHERE id = 'S-1'",
            [],
        )
        .expect_err("text that is not JSON must be refused")
        .to_string();
    assert!(refused.contains("CHECK constraint failed"), "{refused}");
    let unchanged: Option<String> = conn
        .query_row(
            "SELECT host_cursor FROM chat_sessions WHERE id = 'S-1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(unchanged, None);

    conn.execute(
        "UPDATE chat_sessions SET host_cursor = '{\"unit\":\"u\"}' WHERE id = 'S-1'",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE chat_sessions SET host_cursor = NULL WHERE id = 'S-1'",
        [],
    )
    .unwrap();
}

#[test]
fn the_redaction_trigger_and_the_other_chat_triggers_are_as_they_were() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.db");
    version_ten(&path);
    let before = Connection::open(&path).unwrap();
    let names_to_keep = [
        "chat_items_change_only_by_redaction",
        "chat_items_are_never_deleted",
        "chat_items_session_exists",
        "chat_items_answer_names_an_earlier_item",
        "chat_sessions_owner_never_changes",
        "chat_sessions_end_is_final",
    ];
    let sql_before: Vec<String> = names_to_keep
        .iter()
        .map(|n| trigger_sql(&before, n))
        .collect();
    drop(before);
    drop(SqliteStore::open(&path).unwrap());
    let after = Connection::open(&path).unwrap();
    let sql_after: Vec<String> = names_to_keep
        .iter()
        .map(|n| trigger_sql(&after, n))
        .collect();
    assert_eq!(sql_after, sql_before, "no chat trigger was rewritten");

    // A plain rewrite of an item is still refused, and a cursor write is not one.
    let refused = after
        .execute(
            "UPDATE chat_items SET payload = '{\"x\":1}' WHERE session_id = 'S-1'",
            [],
        )
        .expect_err("an item changes only by redaction")
        .to_string();
    assert!(
        refused.contains("a chat item changes only by one redaction"),
        "{refused}"
    );
    after
        .execute(
            "UPDATE chat_sessions SET host_cursor = '{}' WHERE id = 'S-1'",
            [],
        )
        .expect("a cursor write is not an owner change");
}

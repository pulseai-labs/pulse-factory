//! The migration that adds the clear grant (ADR-0003's amendment of 2026-11-16, ADR-0007:
//! forward-only; `r2.s3.w1`): one table, a partial unique index and the triggers that hold each
//! rule in the database.
//!
//! Another spine may land a migration first, so this test names no number. It finds the migration
//! by its name in `MIGRATIONS`, builds a store file by hand from every migration before it, as the
//! binary before this item left it, and opens that file with this binary.

use std::collections::BTreeSet;
use std::path::Path;

use pf_engine::port::store::Store;
use pf_store_sqlite::{MIGRATIONS, SqliteStore};
use rusqlite::{Connection, params};

const NAME: &str = "clear_grants";

fn position() -> usize {
    MIGRATIONS
        .iter()
        .position(|m| m.name == NAME)
        .expect("a migration named clear_grants")
}

/// A store file as the binary before this migration left it: every earlier migration, a project and
/// a device.
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

/// A fresh store and a raw connection to it.
fn fresh() -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.db");
    drop(SqliteStore::open(&path).unwrap());
    (dir, Connection::open(&path).unwrap())
}

const GRANT: &str = "INSERT INTO clear_grants (project_id, generation, agent_id, granted_by, \
     granted_at, command_id) VALUES ('P-1', 1, 'orchestrator', 'cli', 't1', 'c1')";

fn refused(conn: &Connection, sql: &str) -> String {
    match conn.execute(sql, []) {
        Err(e) => e.to_string(),
        Ok(n) => panic!("`{sql}` changed {n} rows, and must be refused"),
    }
}

#[test]
fn the_migrations_are_numbered_with_no_gap_and_this_one_is_the_next_after_a_known_one() {
    let at = position();
    assert!(at > 0, "this migration is not the first");
    for (i, m) in MIGRATIONS.iter().enumerate() {
        assert_eq!(m.version as usize, i + 1, "{}", m.name);
    }
    assert!(
        MIGRATIONS[at].sql.contains("CREATE TABLE clear_grants"),
        "the migration holds the table"
    );
}

#[test]
fn a_store_at_the_version_before_it_moves_up_and_keeps_every_row() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.db");
    before_the_migration(&path);
    let before = Connection::open(&path).unwrap();
    let before_tables = names(&before, "table");
    let counts_before: Vec<(String, i64)> = before_tables
        .iter()
        .map(|t| (t.clone(), count(&before, t)))
        .collect();
    let version_before = MIGRATIONS[position() - 1].version;
    let top: u32 = before
        .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(top, version_before, "the file is at the version before");
    drop(before);

    let store = SqliteStore::open(&path).unwrap();
    assert_eq!(
        store.schema_version().unwrap(),
        MIGRATIONS.last().unwrap().version,
        "the plain open applies this migration and every later one"
    );
    assert_eq!(store.projects().unwrap().len(), 1);
    assert_eq!(store.devices().unwrap().len(), 1);
    assert!(store.clear_grants().unwrap().is_empty(), "no grant yet");
    drop(store);

    let after = Connection::open(&path).unwrap();
    for (table, n) in &counts_before {
        if table == "schema_version" {
            let applied = i64::try_from(MIGRATIONS.len() - position()).unwrap();
            assert_eq!(
                count(&after, table),
                n + applied,
                "this migration and every later one are recorded"
            );
        } else {
            assert_eq!(count(&after, table), *n, "{table} keeps every row");
        }
    }
    let recorded: String = after
        .query_row(
            "SELECT name FROM schema_version WHERE version = ?1",
            [MIGRATIONS[position()].version],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(recorded, NAME);
    assert!(
        names(&after, "table").contains("clear_grants"),
        "the table exists"
    );
}

#[test]
fn the_table_holds_the_columns_of_the_approach_and_no_secret() {
    let (_d, conn) = fresh();
    assert_eq!(
        columns(&conn, "clear_grants"),
        set(&[
            "project_id",
            "generation",
            "agent_id",
            "granted_by",
            "granted_at",
            "revoked_by",
            "revoked_at",
            "command_id"
        ])
    );
    let triggers = names(&conn, "trigger");
    let indexes = names(&conn, "index");
    assert!(
        triggers
            .iter()
            .filter(|t| t.starts_with("clear_grants_"))
            .count()
            >= 4,
        "the guards are triggers: {triggers:?}"
    );
    assert!(
        indexes.iter().any(|i| i.starts_with("clear_grants_")),
        "the partial unique index: {indexes:?}"
    );
}

#[test]
fn a_projects_generation_is_the_highest_plus_one_and_the_key_is_project_and_generation() {
    let (_d, conn) = fresh();
    // The first generation is 1.
    refused(
        &conn,
        "INSERT INTO clear_grants (project_id, generation, agent_id, granted_by, granted_at, command_id) \
         VALUES ('P-1', 2, 'orchestrator', 'cli', 't', 'c0')",
    );
    conn.execute(GRANT, []).unwrap();
    // The same key, a second time.
    refused(
        &conn,
        "INSERT INTO clear_grants (project_id, generation, agent_id, granted_by, granted_at, command_id) \
         VALUES ('P-1', 1, 'orchestrator', 'cli', 't', 'c2')",
    );
    // Another project has its own count.
    conn.execute(
        "INSERT INTO clear_grants (project_id, generation, agent_id, granted_by, granted_at, command_id) \
         VALUES ('P-2', 1, 'orchestrator', 'cli', 't', 'c3')",
        [],
    )
    .unwrap();
    // The command ID is unique.
    refused(
        &conn,
        "INSERT INTO clear_grants (project_id, generation, agent_id, granted_by, granted_at, command_id) \
         VALUES ('P-3', 1, 'orchestrator', 'cli', 't', 'c3')",
    );
    // A generation below 1 is no generation.
    refused(
        &conn,
        "INSERT INTO clear_grants (project_id, generation, agent_id, granted_by, granted_at, command_id) \
         VALUES ('P-4', 0, 'orchestrator', 'cli', 't', 'c4')",
    );
    assert_eq!(count(&conn, "clear_grants"), 2);
}

#[test]
fn at_most_one_row_of_a_project_is_live() {
    let (_d, conn) = fresh();
    conn.execute(GRANT, []).unwrap();
    // A second live row, with the right next generation: the partial unique index refuses it.
    refused(
        &conn,
        "INSERT INTO clear_grants (project_id, generation, agent_id, granted_by, granted_at, command_id) \
         VALUES ('P-1', 2, 'orchestrator', 'cli', 't', 'c2')",
    );
    conn.execute(
        "UPDATE clear_grants SET revoked_by = 'cli', revoked_at = 't2' WHERE project_id = 'P-1'",
        [],
    )
    .unwrap();
    // After the revoke the next generation goes in.
    conn.execute(
        "INSERT INTO clear_grants (project_id, generation, agent_id, granted_by, granted_at, command_id) \
         VALUES ('P-1', 2, 'orchestrator', 'cli', 't3', 'c2')",
        [],
    )
    .unwrap();
    let live: i64 = conn
        .query_row(
            "SELECT count(*) FROM clear_grants WHERE project_id = 'P-1' AND revoked_at IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(live, 1);
}

#[test]
fn an_update_may_only_set_the_revoke_once_and_a_delete_is_refused() {
    let (_d, conn) = fresh();
    conn.execute(GRANT, []).unwrap();
    for sql in [
        "UPDATE clear_grants SET agent_id = 'x'",
        "UPDATE clear_grants SET granted_by = 'x'",
        "UPDATE clear_grants SET granted_at = 'x'",
        "UPDATE clear_grants SET command_id = 'x'",
        "UPDATE clear_grants SET generation = 3",
        "UPDATE clear_grants SET project_id = 'P-9'",
        "UPDATE clear_grants SET revoked_by = 'cli'",
        "UPDATE clear_grants SET revoked_at = 't'",
        "UPDATE clear_grants SET revoked_by = 'cli', revoked_at = 't', agent_id = 'x'",
        "DELETE FROM clear_grants",
    ] {
        refused(&conn, sql);
    }
    conn.execute(
        "UPDATE clear_grants SET revoked_by = 'cli', revoked_at = 't9'",
        [],
    )
    .unwrap();
    // A second revoke, and an undo.
    refused(
        &conn,
        "UPDATE clear_grants SET revoked_by = 'cli', revoked_at = 't10'",
    );
    refused(
        &conn,
        "UPDATE clear_grants SET revoked_by = NULL, revoked_at = NULL",
    );
    refused(&conn, "DELETE FROM clear_grants");
    assert_eq!(count(&conn, "clear_grants"), 1);
}

#[test]
fn a_row_is_never_born_revoked_and_never_replaced() {
    let (_d, conn) = fresh();
    refused(
        &conn,
        "INSERT INTO clear_grants (project_id, generation, agent_id, granted_by, granted_at, \
         revoked_by, revoked_at, command_id) VALUES ('P-1', 1, 'orchestrator', 'cli', 't', 'cli', 't', 'c1')",
    );
    conn.execute(GRANT, []).unwrap();
    // `INSERT OR REPLACE` deletes a row without a delete trigger. It cannot rewrite a row by its
    // key or by its command ID.
    refused(
        &conn,
        "INSERT OR REPLACE INTO clear_grants (project_id, generation, agent_id, granted_by, \
         granted_at, command_id) VALUES ('P-1', 1, 'other', 'cli', 't', 'c9')",
    );
    refused(
        &conn,
        "INSERT OR REPLACE INTO clear_grants (project_id, generation, agent_id, granted_by, \
         granted_at, command_id) VALUES ('P-2', 1, 'other', 'cli', 't', 'c1')",
    );
    let agent: String = conn
        .query_row("SELECT agent_id FROM clear_grants", [], |r| r.get(0))
        .unwrap();
    assert_eq!(agent, "orchestrator");
}

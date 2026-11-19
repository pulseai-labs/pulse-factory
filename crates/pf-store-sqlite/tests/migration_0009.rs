//! Migration `0009` adds the launch records and the host registry (ADR-0003's amendment of
//! 2026-11-16, ADR-0007: forward-only; `r2.s2.w2`).
//!
//! The migration cases build their version-8 store file by hand, from the text of migrations `0001`
//! to `0008` alone, as the binary of `r2.s1` left it. The guard cases write to a migrated file with
//! plain SQL, so a trigger or a constraint refuses what the store port would never send.

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

const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// A store file at schema version 8: an ended run, a project, a setting and an admission.
fn version_eight(path: &Path) {
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
         VALUES ('run-1', 'wf', '/l.md', 'ended', 't0', 't0')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO projects (id, name, root, created_at, kind) \
         VALUES ('P-1', 'alpha', '/work/alpha', 't2', 'folder')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO admissions (run_id, command_id, kind, content, outcome, admitted_at) \
         VALUES ('@controller', 'c1', 'project_add', '{}', 'admitted', 't1')",
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

fn set(of: &[&str]) -> BTreeSet<String> {
    of.iter().map(ToString::to_string).collect()
}

fn insert_record(conn: &Connection, id: &str, revision: i64, command_id: &str) {
    conn.execute(
        "INSERT INTO launch_records (session_kind, session_id, incarnation, revision, launcher, \
         host_unit, host_socket, record, record_sha256, command_id, created_at) \
         VALUES ('chat', ?1, 1, ?2, 'detached', 'pf-host-s-S-1-1', '/run/s.sock', '{}', ?3, ?4, 't1')",
        params![id, revision, HASH, command_id],
    )
    .unwrap();
}

fn insert_host(conn: &Connection, unit: &str, command_id: &str) {
    conn.execute(
        "INSERT INTO hosts (unit, session_kind, session_id, incarnation, launched_revision, \
         launched_record_sha256, launcher, socket, invocation_id, host_binary, host_sha256, \
         protocol, started_at, command_id) \
         VALUES (?1, 'chat', 'S-1', 1, 1, ?2, 'detached', '/run/s.sock', 'inv-1', '/bin/pf-host', ?2, \
         1, 't2', ?3)",
        params![unit, HASH, command_id],
    )
    .unwrap();
}

/// The text a refused statement fails with.
fn refusal(conn: &Connection, sql: &str) -> String {
    conn.execute(sql, [])
        .expect_err(&format!("{sql} must be refused"))
        .to_string()
}

fn migrated() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.db");
    version_eight(&path);
    drop(SqliteStore::open(&path).unwrap());
    (dir, path)
}

#[test]
fn a_version_eight_store_with_data_migrates_to_the_latest_and_keeps_every_row() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.db");
    version_eight(&path);
    let before = Connection::open(&path).unwrap();
    let tables_before = names(&before, "table");
    let counts_before: Vec<(String, i64)> = tables_before
        .iter()
        .map(|t| (t.clone(), count(&before, t)))
        .collect();
    drop(before);

    assert_eq!(MIGRATIONS.len(), 10);
    assert_eq!(MIGRATIONS[8].version, 9);
    assert_eq!(MIGRATIONS[8].name, "launch_records");
    let store = SqliteStore::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), 10);
    drop(store);

    let after = Connection::open(&path).unwrap();
    for (table, n) in &counts_before {
        if table == "schema_version" {
            assert_eq!(
                count(&after, table),
                n + 2,
                "versions 9 and 10 are recorded"
            );
        } else {
            assert_eq!(count(&after, table), *n, "{table} keeps every row");
        }
    }
    assert_eq!(count(&after, "launch_records"), 0);
    assert_eq!(count(&after, "hosts"), 0);
}

#[test]
fn the_two_tables_and_their_triggers_exist() {
    let (_d, path) = migrated();
    let conn = Connection::open(&path).unwrap();
    let tables = names(&conn, "table");
    assert!(tables.contains("launch_records"));
    assert!(tables.contains("hosts"));
    let triggers = names(&conn, "trigger");
    for t in [
        "launch_records_never_update",
        "launch_records_never_delete",
        "launch_records_never_replace",
        "hosts_end_once",
        "hosts_never_delete",
        "hosts_never_replace",
    ] {
        assert!(triggers.contains(t), "trigger {t} in {triggers:?}");
    }
}

#[test]
fn a_launch_record_is_write_once() {
    let (_d, path) = migrated();
    let conn = Connection::open(&path).unwrap();
    insert_record(&conn, "S-1", 1, "cmd-1");
    for sql in [
        "UPDATE launch_records SET launcher = 'systemd'",
        "UPDATE launch_records SET record = '{\"argv\":[]}'",
        "UPDATE launch_records SET created_at = 't9'",
        "DELETE FROM launch_records",
    ] {
        let why = refusal(&conn, sql);
        assert!(why.contains("write-once"), "{sql}: {why}");
    }
    assert_eq!(count(&conn, "launch_records"), 1);
}

#[test]
fn a_launch_record_keeps_its_key_unique_and_its_command_id_unique() {
    let (_d, path) = migrated();
    let conn = Connection::open(&path).unwrap();
    insert_record(&conn, "S-1", 1, "cmd-1");
    // The same session, incarnation and revision under another command ID.
    assert!(
        conn.execute(
            "INSERT INTO launch_records (session_kind, session_id, incarnation, revision, launcher, \
             host_unit, host_socket, record, record_sha256, command_id, created_at) \
             VALUES ('chat', 'S-1', 1, 1, 'detached', 'u', 's', '{}', ?1, 'cmd-2', 't')",
            [HASH],
        )
        .is_err()
    );
    // Another revision under the same command ID.
    assert!(
        conn.execute(
            "INSERT INTO launch_records (session_kind, session_id, incarnation, revision, launcher, \
             host_unit, host_socket, record, record_sha256, command_id, created_at) \
             VALUES ('chat', 'S-1', 1, 2, 'detached', 'u', 's', '{}', ?1, 'cmd-1', 't')",
            [HASH],
        )
        .is_err()
    );
    // An incarnation or a revision below 1, a hash that is not 64 hex digits, a body that is no JSON.
    for (incarnation, revision, hash, body) in [
        (0, 1, HASH, "{}"),
        (1, 0, HASH, "{}"),
        (1, 3, "abc", "{}"),
        (1, 3, HASH, "not json"),
    ] {
        assert!(
            conn.execute(
                "INSERT INTO launch_records (session_kind, session_id, incarnation, revision, launcher, \
                 host_unit, host_socket, record, record_sha256, command_id, created_at) \
                 VALUES ('chat', 'S-1', ?1, ?2, 'detached', 'u', 's', ?4, ?3, 'cmd-9', 't')",
                params![incarnation, revision, hash, body],
            )
            .is_err(),
            "({incarnation}, {revision}, {hash}, {body}) must be refused"
        );
    }
}

#[test]
fn insert_or_replace_cannot_rewrite_a_launch_record_or_a_host_row() {
    let (_d, path) = migrated();
    let conn = Connection::open(&path).unwrap();
    insert_record(&conn, "S-1", 1, "cmd-1");
    insert_host(&conn, "pf-host-s-S-1-1", "cmd-h1");
    let other_hash = "f".repeat(64);
    let record_before: (String, String) = conn
        .query_row(
            "SELECT record, record_sha256 FROM launch_records",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();

    // A launch record: the same key, the same command ID, each with a new body.
    for (revision, command_id) in [(1, "cmd-2"), (2, "cmd-1")] {
        let why = refusal(
            &conn,
            &format!(
                "INSERT OR REPLACE INTO launch_records (session_kind, session_id, incarnation, \
                 revision, launcher, host_unit, host_socket, record, record_sha256, command_id, \
                 created_at) VALUES ('chat', 'S-1', 1, {revision}, 'detached', 'u', 's', \
                 '{{\"x\":1}}', '{other_hash}', '{command_id}', 't9')"
            ),
        );
        assert!(why.contains("write-once"), "{revision}/{command_id}: {why}");
    }
    // A host row: the same unit, the same incarnation under another unit, the same command ID.
    for (unit, incarnation, command_id) in [
        ("pf-host-s-S-1-1", 1, "cmd-h2"),
        ("pf-host-s-S-1-1b", 1, "cmd-h3"),
        ("pf-host-s-S-1-2", 2, "cmd-h1"),
    ] {
        let why = refusal(
            &conn,
            &format!(
                "INSERT OR REPLACE INTO hosts (unit, session_kind, session_id, incarnation, \
                 launched_revision, launched_record_sha256, launcher, socket, invocation_id, \
                 host_binary, host_sha256, protocol, started_at, command_id) \
                 VALUES ('{unit}', 'chat', 'S-1', {incarnation}, 1, '{other_hash}', 'detached', \
                 '/elsewhere', 'inv-9', '/bin/other', '{other_hash}', 1, 't9', '{command_id}')"
            ),
        );
        assert!(why.contains("never replaced"), "{unit}/{command_id}: {why}");
    }

    // Nothing changed.
    let record_after: (String, String) = conn
        .query_row(
            "SELECT record, record_sha256 FROM launch_records",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(record_after, record_before);
    assert_eq!(count(&conn, "launch_records"), 1);
    let socket: String = conn
        .query_row("SELECT socket FROM hosts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(socket, "/run/s.sock");
    assert_eq!(count(&conn, "hosts"), 1);

    // The control: a statement that replaces nothing still goes through.
    conn.execute(
        "INSERT OR REPLACE INTO launch_records (session_kind, session_id, incarnation, revision, \
         launcher, host_unit, host_socket, record, record_sha256, command_id, created_at) \
         VALUES ('chat', 'S-2', 1, 1, 'detached', 'u', 's', '{}', ?1, 'cmd-new', 't')",
        [HASH],
    )
    .unwrap();
    assert_eq!(count(&conn, "launch_records"), 2);
}

#[test]
fn a_host_row_ends_once_with_its_exit_and_changes_nothing_else() {
    let (_d, path) = migrated();
    let conn = Connection::open(&path).unwrap();
    insert_host(&conn, "pf-host-s-S-1-1", "cmd-h1");

    // Any other change is refused: the launched revision and hash, the socket, the unit itself.
    for sql in [
        "UPDATE hosts SET launched_revision = 2",
        "UPDATE hosts SET launched_record_sha256 = 'ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff'",
        "UPDATE hosts SET socket = '/elsewhere'",
        "UPDATE hosts SET unit = 'other'",
        "UPDATE hosts SET invocation_id = 'inv-2'",
        // The end and the exit set together: one alone is refused.
        "UPDATE hosts SET ended_at = 't3'",
        "UPDATE hosts SET exit = '{\"code\":0}'",
        // An end with a change beside it is refused.
        "UPDATE hosts SET ended_at = 't3', exit = '{\"code\":0}', socket = '/elsewhere'",
        "DELETE FROM hosts",
    ] {
        let why = refusal(&conn, sql);
        assert!(why.contains("a host row"), "{sql}: {why}");
    }
    let live: i64 = conn
        .query_row(
            "SELECT count(*) FROM hosts WHERE ended_at IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(live, 1, "no refused statement changed the row");

    // The end and the exit set once, together.
    conn.execute(
        "UPDATE hosts SET ended_at = 't3', exit = '{\"code\":0}'",
        [],
    )
    .unwrap();
    // A second set, and any change after the end, are refused.
    for sql in [
        "UPDATE hosts SET ended_at = 't4', exit = '{\"code\":1}'",
        "UPDATE hosts SET exit = '{\"code\":1}'",
        "UPDATE hosts SET launched_revision = 2",
        "DELETE FROM hosts",
    ] {
        let why = refusal(&conn, sql);
        assert!(why.contains("a host row"), "{sql}: {why}");
    }
    let (ended, exit): (String, String) = conn
        .query_row("SELECT ended_at, exit FROM hosts", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!((ended.as_str(), exit.as_str()), ("t3", "{\"code\":0}"));
}

#[test]
fn a_host_exit_is_json_and_one_incarnation_has_one_host() {
    let (_d, path) = migrated();
    let conn = Connection::open(&path).unwrap();
    insert_host(&conn, "pf-host-s-S-1-1", "cmd-h1");
    assert!(
        conn.execute("UPDATE hosts SET ended_at = 't3', exit = 'not json'", [])
            .is_err()
    );
    // A second unit for the same session and incarnation.
    assert!(
        conn.execute(
            "INSERT INTO hosts (unit, session_kind, session_id, incarnation, launched_revision, \
             launched_record_sha256, launcher, socket, invocation_id, host_binary, host_sha256, \
             protocol, started_at, command_id) \
             VALUES ('pf-host-s-S-1-1b', 'chat', 'S-1', 1, 1, ?1, 'detached', '/x', 'i', 'b', ?1, 1, 't', 'cmd-h2')",
            [HASH],
        )
        .is_err()
    );
}

#[test]
fn the_record_has_no_runtime_session_id_and_no_table_holds_an_attach_key() {
    let (_d, path) = migrated();
    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        columns(&conn, "launch_records"),
        set(&[
            "session_kind",
            "session_id",
            "incarnation",
            "revision",
            "launcher",
            "host_unit",
            "host_socket",
            "record",
            "record_sha256",
            "command_id",
            "created_at",
        ])
    );
    assert_eq!(
        columns(&conn, "hosts"),
        set(&[
            "unit",
            "session_kind",
            "session_id",
            "incarnation",
            "launched_revision",
            "launched_record_sha256",
            "launcher",
            "socket",
            "invocation_id",
            "host_binary",
            "host_sha256",
            "protocol",
            "started_at",
            "ended_at",
            "exit",
            "command_id",
        ])
    );
    for table in ["launch_records", "hosts"] {
        for column in columns(&conn, table) {
            for banned in ["key", "secret", "token", "resume", "runtime_session"] {
                assert!(
                    !column.contains(banned),
                    "{table}.{column} names {banned}: the store holds no key and no runtime session ID"
                );
            }
        }
    }
    // No `CHECK` on the kind or the launcher: the port validates them (ADR-0016's precedent).
    conn.execute(
        "INSERT INTO launch_records (session_kind, session_id, incarnation, revision, launcher, \
         host_unit, host_socket, record, record_sha256, command_id, created_at) \
         VALUES ('anything', 'X-1', 1, 1, 'launchd', 'u', 's', '{}', ?1, 'cmd-k', 't')",
        [HASH],
    )
    .unwrap();
}

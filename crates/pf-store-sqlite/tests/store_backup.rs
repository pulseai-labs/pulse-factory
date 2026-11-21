//! The backup before a migration (#85; ADR-0003, ADR-0007; `r2.s2.w2`): after the active-run check
//! and before the first pending migration, the store writes `<store>.v<from>-v<to>.<utc>.bak` beside
//! the store with `VACUUM INTO`, at mode 0600, checks it with `PRAGMA integrity_check`, and keeps the
//! newest `PF_STORE_BACKUP_KEEP` of them (1 to 20, default 3).
//!
//! Every case holds `ENV`, one lock for the whole file: `PF_STORE_BACKUP_KEEP` is a variable of the
//! process, so a case that sets it must not run beside one that opens a store.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use common::version_eight_store;
use pf_store_sqlite::{Migration, OpenError, SqliteStore, backup_keep};
use rusqlite::Connection;

static ENV: Mutex<()> = Mutex::new(());

fn locked() -> MutexGuard<'static, ()> {
    ENV.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

const THIRTEEN: Migration = Migration {
    version: 13,
    name: "test thirteen",
    sql: "CREATE TABLE extra_thirteen (x INTEGER NOT NULL) STRICT;",
};
const FOURTEEN: Migration = Migration {
    version: 14,
    name: "test fourteen",
    sql: "CREATE TABLE extra_fourteen (x INTEGER NOT NULL) STRICT;",
};

/// The backups beside a store, by name.
fn backups(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "bak"))
        .collect();
    found.sort();
    found
}

fn name(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

/// The rows of the tables that `version_eight_store` fills, as text, and the schema versions.
fn dump(path: &Path) -> Vec<String> {
    let conn = Connection::open(path).unwrap();
    let mut out = Vec::new();
    for sql in [
        "SELECT id || '|' || state FROM runs ORDER BY id",
        "SELECT run_id || '|' || id FROM nodes ORDER BY id",
        "SELECT id || '|' || root FROM projects ORDER BY id",
        "SELECT command_id || '|' || kind FROM admissions ORDER BY seq",
        "SELECT version || '|' || name FROM schema_version ORDER BY version",
    ] {
        let mut stmt = conn.prepare(sql).unwrap();
        let rows: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        out.push(rows.join(";"));
    }
    out
}

fn version(path: &Path) -> i64 {
    Connection::open(path)
        .unwrap()
        .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
        .unwrap()
}

fn has_table(path: &Path, table: &str) -> bool {
    Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
        > 0
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn opening_a_version_eight_store_with_a_binary_at_twelve_writes_one_checked_backup() {
    let _env = locked();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.db");
    version_eight_store(&path);
    let before = dump(&path);

    let store = SqliteStore::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), 12);
    drop(store);

    let found = backups(dir.path());
    assert_eq!(found.len(), 1, "{found:?}");
    let backup = &found[0];
    let file = name(backup);
    assert!(
        file.starts_with("run.db.v8-v12.") && file.ends_with("Z.bak"),
        "{file}"
    );
    assert_eq!(mode(backup), 0o600);
    // It passes the check, opens at version 8 and holds the rows the store held.
    let conn = Connection::open(backup).unwrap();
    let check: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(check, "ok");
    drop(conn);
    assert_eq!(version(backup), 8);
    assert_eq!(dump(backup), before);
    assert!(!has_table(backup, "launch_records"));
    // The store itself is migrated.
    assert_eq!(version(&path), 12);
    assert!(has_table(&path, "launch_records"));
    assert!(has_table(&path, "host_positions"));
    // The backup is the only file the migration added beside the store.
    let mut others: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .map(|e| name(&e.path()))
        .filter(|n| !n.ends_with(".bak"))
        .collect();
    others.sort();
    assert_eq!(others, ["run.db"], "no side file is left behind");
}

#[test]
fn a_backup_holds_the_frames_a_crash_left_in_the_wal() {
    let _env = locked();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.db");
    version_eight_store(&path);
    // A writer that never checkpoints, held open: its last row is only in the `-wal`.
    let writer = Connection::open(&path).unwrap();
    let mode: String = writer
        .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    writer
        .execute_batch(
            "PRAGMA wal_autocheckpoint = 0; \
             INSERT INTO admissions (run_id, command_id, kind, content, outcome, admitted_at) \
             VALUES ('@controller', 'c-wal', 'project_add', '{}', 'admitted', 't9');",
        )
        .unwrap();
    assert!(
        std::fs::metadata(dir.path().join("run.db-wal"))
            .unwrap()
            .len()
            > 0,
        "the row is in the -wal"
    );

    drop(SqliteStore::open(&path).unwrap());
    drop(writer);
    let found = backups(dir.path());
    assert_eq!(found.len(), 1);
    let conn = Connection::open(&found[0]).unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT count(*) FROM admissions WHERE command_id = 'c-wal'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1, "the backup holds the row that only the -wal held");
}

#[test]
fn an_up_to_date_store_and_a_new_empty_store_write_no_backup() {
    let _env = locked();
    let dir = tempfile::tempdir().unwrap();
    // A new, empty store (version 0): nothing to lose, so nothing to back up.
    let path = dir.path().join("new.db");
    drop(SqliteStore::open(&path).unwrap());
    assert_eq!(backups(dir.path()), Vec::<PathBuf>::new());
    // A store at the binary's own version: nothing is pending.
    drop(SqliteStore::open(&path).unwrap());
    drop(SqliteStore::open(&path).unwrap());
    assert_eq!(backups(dir.path()), Vec::<PathBuf>::new());
}

#[test]
fn the_newest_backups_stay_and_the_count_is_an_environment_setting() {
    let _env = locked();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.db");
    version_eight_store(&path);
    // SAFETY: every case of this file holds `ENV`, so no other thread of this process opens a store
    // or reads the environment while the variable is set.
    unsafe { std::env::set_var("PF_STORE_BACKUP_KEEP", "2") };
    // Three upgrades: 8 to 12, 12 to 13 and 13 to 14. Each writes its backup, then the store keeps
    // the newest two.
    let upgrades: [(&[Migration], usize); 3] =
        [(&[], 1), (&[THIRTEEN], 2), (&[THIRTEEN, FOURTEEN], 2)];
    for (extra, kept) in upgrades {
        drop(SqliteStore::open_with_migrations(&path, extra).unwrap());
        assert_eq!(
            backups(dir.path()).len(),
            kept,
            "after {} migrations",
            extra.len()
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    unsafe { std::env::remove_var("PF_STORE_BACKUP_KEEP") };

    // Oldest first: the time in the name orders them, and the version text does not.
    let mut left: Vec<String> = backups(dir.path()).iter().map(|p| name(p)).collect();
    left.sort_by_key(|n| n.rsplit('.').nth(1).unwrap_or("").to_string());
    assert_eq!(left.len(), 2, "{left:?}");
    assert!(left[0].contains(".v12-v13."), "{left:?}");
    assert!(left[1].contains(".v13-v14."), "{left:?}");
    assert!(!left.iter().any(|n| n.contains(".v8-v12.")), "{left:?}");
}

#[test]
fn the_default_keeps_three_and_a_value_outside_one_to_twenty_logs_one_line_and_keeps_three() {
    let _env = locked();
    assert_eq!(backup_keep(None), (3, None));
    assert_eq!(backup_keep(Some("1")), (1, None));
    assert_eq!(backup_keep(Some("20")), (20, None));
    assert_eq!(backup_keep(Some(" 5 ")), (5, None));
    for bad in ["0", "21", "-1", "many", "", "2.5"] {
        let (keep, line) = backup_keep(Some(bad));
        assert_eq!(keep, 3, "{bad:?}");
        let line = line.unwrap_or_else(|| panic!("{bad:?} gives no stderr line"));
        assert!(!line.contains('\n'), "one line: {line:?}");
        assert!(line.contains("PF_STORE_BACKUP_KEEP"), "{line}");
        assert!(line.contains("1 to 20"), "{line}");
    }
}

#[test]
fn a_backup_that_cannot_be_written_fails_the_open_and_leaves_the_store_at_version_eight() {
    let _env = locked();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.db");
    version_eight_store(&path);
    // A writer in WAL mode keeps the `-wal` and `-shm` files, so a second connection can open the
    // file in a directory that no longer takes new files.
    let holder = Connection::open(&path).unwrap();
    let _: String = holder
        .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
        .unwrap();
    holder
        .execute_batch(
            "PRAGMA wal_autocheckpoint = 0; \
             INSERT INTO admissions (run_id, command_id, kind, content, outcome, admitted_at) \
             VALUES ('@controller', 'c-held', 'project_add', '{}', 'admitted', 't9');",
        )
        .unwrap();
    assert!(dir.path().join("run.db-wal").exists() && dir.path().join("run.db-shm").exists());
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    // Running as a user that ignores directory modes (root) cannot make this case.
    if std::fs::File::create(dir.path().join("probe")).is_ok() {
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        eprintln!("skipped: this user writes into a read-only directory");
        return;
    }

    let err = SqliteStore::open(&path)
        .err()
        .expect("the backup must fail");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let OpenError::Backup { path: at, why } = &err else {
        panic!("{err:?}");
    };
    assert!(
        at.contains("run.db.v8-v12."),
        "the error names the path: {at}"
    );
    assert!(!why.is_empty(), "the error names the cause");
    let text = err.to_string();
    assert!(
        text.contains(at.as_str()) && text.contains(why.as_str()),
        "{text}"
    );
    drop(holder);
    assert_eq!(backups(dir.path()), Vec::<PathBuf>::new());
    assert_eq!(version(&path), 8, "no migration was applied");
    assert!(!has_table(&path, "launch_records"));
    let leftover: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .map(|e| name(&e.path()))
        .filter(|n| n.contains(".v8-v12."))
        .collect();
    assert!(leftover.is_empty(), "{leftover:?}");
}

#[test]
fn a_backup_that_fails_its_check_is_removed_and_no_migration_is_applied() {
    let _env = locked();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.db");
    version_eight_store(&path);
    let before = dump(&path);

    // The hook damages the backup after it was written and before the store checks it.
    let truncate = |backup: &Path| {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(backup)
            .unwrap();
        file.set_len(100).unwrap();
    };
    let err = SqliteStore::open_with_backup_hook(&path, &[], &truncate)
        .err()
        .expect("a damaged backup must fail the open");
    let OpenError::Backup { path: at, why } = &err else {
        panic!("{err:?}");
    };
    assert!(at.contains("run.db.v8-v12."), "{at}");
    assert!(!why.is_empty());
    assert_eq!(
        backups(dir.path()),
        Vec::<PathBuf>::new(),
        "the backup is removed"
    );
    assert_eq!(version(&path), 8);
    assert!(!has_table(&path, "launch_records"));
    assert_eq!(dump(&path), before);

    // A backup that holds no schema at all fails too: it is no backup of version 8.
    let empty = |backup: &Path| {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(backup)
            .unwrap();
        file.set_len(0).unwrap();
    };
    let err = SqliteStore::open_with_backup_hook(&path, &[], &empty)
        .err()
        .expect("an empty backup must fail the open");
    assert!(matches!(err, OpenError::Backup { .. }), "{err:?}");
    assert_eq!(backups(dir.path()), Vec::<PathBuf>::new());
    assert_eq!(version(&path), 8);

    // With no damage, the same store opens and migrates.
    drop(SqliteStore::open(&path).unwrap());
    assert_eq!(version(&path), 12);
    assert_eq!(backups(dir.path()).len(), 1);
}

#[test]
fn an_active_run_still_refuses_the_migration_and_writes_no_backup() {
    let _env = locked();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.db");
    version_eight_store(&path);
    Connection::open(&path)
        .unwrap()
        .execute(
            "INSERT INTO runs (id, workflow, ledger_file, state, created_at, updated_at) \
             VALUES ('run-2', 'wf', '/l2.md', 'active', 't0', 't0')",
            [],
        )
        .unwrap();
    let err = SqliteStore::open(&path).err().expect("the run is active");
    assert_eq!(
        err,
        OpenError::RunActive {
            current: 8,
            pending: 12
        }
    );
    assert_eq!(backups(dir.path()), Vec::<PathBuf>::new());
    assert_eq!(version(&path), 8);
}

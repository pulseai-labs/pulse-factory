//! `pf-store-sqlite`: the first implementation of the `pf-engine` store port (ADR-0003).
//!
//! rusqlite, one serialized writer thread off the async path, WAL, `synchronous=FULL`.
//! The engine never sees SQL: it sees the port.

pub mod offline;
mod port_impl;

use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::JoinHandle;

use pf_engine::port::store::StoreError;
use rusqlite::Connection;

/// A job for the writer thread. The writer runs jobs one at a time, in the order they come.
type Job = Box<dyn FnOnce(&mut Connection) + Send>;

/// One numbered, forward-only schema migration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Migration {
    /// Numbers run 1, 2, 3 with no gap.
    pub version: u32,
    pub name: &'static str,
    pub sql: &'static str,
}

/// The migrations of this binary. Migration `0001` holds what skeleton A needs. Migration `0002`
/// adds requests and claims. Migration `0003` adds projects, and a trigger that keeps `@` out of a
/// run id, so the admission scope `@controller` can never be a run. Migration `0004` adds devices.
/// Migration `0005` adds the project hierarchy: the project kind and remote key, worktrees, agents,
/// chat sessions and chat items. Migration `0006` adds the session's own runtime options. Migration
/// `0007` adds the scoped settings. Migration `0008` links a run to a project. Migration `0009` adds
/// the launch records and the host registry. Migration `0010` adds the host positions. Migration
/// `0011` adds the position of a chat's output in its host. `RunActive` covers every migration,
/// `0011` too: the store never migrates under an active run. Before it applies a pending migration,
/// the store writes
/// a checked backup of the file (see [`backup_keep`]).
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "init",
        sql: include_str!("../migrations/0001_init.sql"),
    },
    Migration {
        version: 2,
        name: "requests",
        sql: include_str!("../migrations/0002_requests.sql"),
    },
    Migration {
        version: 3,
        name: "projects",
        sql: include_str!("../migrations/0003_projects.sql"),
    },
    Migration {
        version: 4,
        name: "devices",
        sql: include_str!("../migrations/0004_devices.sql"),
    },
    Migration {
        version: 5,
        name: "hierarchy",
        sql: include_str!("../migrations/0005_hierarchy.sql"),
    },
    Migration {
        version: 6,
        name: "session_options",
        sql: include_str!("../migrations/0006_session_options.sql"),
    },
    Migration {
        version: 7,
        name: "settings",
        sql: include_str!("../migrations/0007_settings.sql"),
    },
    Migration {
        version: 8,
        name: "run_project",
        sql: include_str!("../migrations/0008_run_project.sql"),
    },
    Migration {
        version: 9,
        name: "launch_records",
        sql: include_str!("../migrations/0009_launch_records.sql"),
    },
    Migration {
        version: 10,
        name: "host_positions",
        sql: include_str!("../migrations/0010_host_positions.sql"),
    },
    Migration {
        version: 11,
        name: "chat_host_cursor",
        sql: include_str!("../migrations/0011_chat_host_cursor.sql"),
    },
];

/// Why the store cannot open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenError {
    /// SQLite refused the file, a setting or a migration.
    Sqlite(String),
    /// The writer thread did not start.
    Thread(String),
    /// A migration is pending and a run is active. The store never migrates under an active run
    /// (ADR-0003).
    RunActive { current: u32, pending: u32 },
    /// The list of migrations is not numbered 1, 2, 3 with no gap.
    BadMigrations(String),
    /// The file was migrated by a newer binary. A binary rolls back only to one with the same
    /// schema version (ADR-0007).
    SchemaNewer { database: u32, supported: u32 },
    /// The backup before a migration could not be written, or it failed its check. No migration was
    /// applied, and a backup that failed its check was removed (#85).
    Backup { path: String, why: String },
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::Sqlite(why) => write!(f, "cannot open the store: {why}"),
            OpenError::Thread(why) => write!(f, "cannot start the store writer: {why}"),
            OpenError::RunActive { current, pending } => write!(
                f,
                "cannot migrate the store from version {current} to {pending} while a run is active"
            ),
            OpenError::BadMigrations(why) => write!(f, "the migration list is not valid: {why}"),
            OpenError::SchemaNewer {
                database,
                supported,
            } => write!(
                f,
                "the store is at schema version {database}, and this binary supports version {supported}"
            ),
            OpenError::Backup { path, why } => {
                write!(
                    f,
                    "cannot write the backup {path}: {why}; no migration was applied"
                )
            }
        }
    }
}

impl std::error::Error for OpenError {}

/// The settings of the open store, as read back through `PRAGMA`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreSettings {
    pub journal_mode: String,
    /// `OFF`, `NORMAL`, `FULL` or `EXTRA`.
    pub synchronous: String,
}

/// The SQLite store. Every call runs on one writer thread, so calls serialize.
pub struct SqliteStore {
    tx: Option<mpsc::Sender<Job>>,
    handle: Option<JoinHandle<()>>,
}

fn sql_error(e: rusqlite::Error) -> StoreError {
    StoreError(e.to_string())
}

fn check_migration_list(all: &[Migration]) -> Result<(), OpenError> {
    for (i, m) in all.iter().enumerate() {
        let expected = u32::try_from(i + 1).unwrap_or(u32::MAX);
        if m.version != expected {
            return Err(OpenError::BadMigrations(format!(
                "position {} holds version {}, and version {expected} is next",
                i + 1,
                m.version
            )));
        }
    }
    Ok(())
}

/// The schema version recorded in the file. A file with no `schema_version` table is at 0.
pub(crate) fn current_version(conn: &Connection) -> rusqlite::Result<u32> {
    let has_table: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'schema_version'",
        [],
        |r| r.get(0),
    )?;
    if has_table == 0 {
        return Ok(0);
    }
    let version: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_version",
        [],
        |r| r.get(0),
    )?;
    Ok(u32::try_from(version).unwrap_or(u32::MAX))
}

/// The name of the environment variable that sets how many backups the store keeps. The store
/// opens before any setting can be read, so the count is an environment value, as the chat
/// environment settings are (ADR-0016).
pub const BACKUP_KEEP_VAR: &str = "PF_STORE_BACKUP_KEEP";

const BACKUP_KEEP_DEFAULT: usize = 3;

/// How many backups the store keeps, from the value of [`BACKUP_KEEP_VAR`]: 1 to 20, and 3 when the
/// value is not set. A value outside the range, or one that is no number, gives 3 and the one stderr
/// line that says so.
pub fn backup_keep(value: Option<&str>) -> (usize, Option<String>) {
    let Some(raw) = value else {
        return (BACKUP_KEEP_DEFAULT, None);
    };
    match raw.trim().parse::<usize>() {
        Ok(n) if (1..=20).contains(&n) => (n, None),
        _ => (
            BACKUP_KEEP_DEFAULT,
            Some(format!(
                "pf-store-sqlite: {BACKUP_KEEP_VAR}={raw:?} is not a number from 1 to 20, so the store keeps {BACKUP_KEEP_DEFAULT} backups"
            )),
        ),
    }
}

/// Check a store or backup file without writing to it: `PRAGMA integrity_check` must answer `ok`
/// and nothing else. It answers the schema version recorded in the file. The file opens read-only.
pub fn verify_file(path: &Path) -> Result<u32, String> {
    let conn = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let answers: Vec<String> = {
        let mut stmt = conn
            .prepare("PRAGMA integrity_check")
            .map_err(|e| format!("the integrity check of {} failed: {e}", path.display()))?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| format!("the integrity check of {} failed: {e}", path.display()))?;
        rows.collect::<Result<_, _>>()
            .map_err(|e| format!("the integrity check of {} failed: {e}", path.display()))?
    };
    if answers != ["ok"] {
        let first = answers.first().map_or("no answer", String::as_str);
        return Err(format!(
            "the integrity check of {} answered {first:?}",
            path.display()
        ));
    }
    current_version(&conn)
        .map_err(|e| format!("cannot read the version of {}: {e}", path.display()))
}

/// The UTC time of a file name: `20261118T180319123Z`.
fn backup_stamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    pf_engine::engine::format_utc(now.as_secs(), now.subsec_millis())
        .chars()
        .filter(|c| !matches!(c, '-' | ':' | '.'))
        .collect()
}

/// One backup file beside a store: `<store file name>.v<from>-v<to>.<utc>.bak`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackupFile {
    pub path: PathBuf,
    /// The schema version the store was at when the backup was made.
    pub from_version: u32,
    /// The schema version the migration that followed it was going to.
    pub to_version: u32,
    /// The UTC time in the name, `20261118T180319123Z`.
    pub stamp: String,
    pub size: u64,
}

impl BackupFile {
    /// The time of the name as the ledger writes it: `2026-11-18T18:03:19.123Z`.
    pub fn time(&self) -> String {
        let s = &self.stamp;
        if s.len() == 19 && s.is_ascii() {
            format!(
                "{}-{}-{}T{}:{}:{}.{}Z",
                &s[0..4],
                &s[4..6],
                &s[6..8],
                &s[9..11],
                &s[11..13],
                &s[13..15],
                &s[15..18]
            )
        } else {
            self.stamp.clone()
        }
    }
}

/// Read `v<from>-v<to>.<utc>` from the part of a backup's name after the store's own name.
fn parse_backup_tail(tail: &str) -> Option<(u32, u32, String)> {
    let rest = tail.strip_prefix(".v")?;
    let (from, rest) = rest.split_once("-v")?;
    let (to, stamp) = rest.split_once('.')?;
    let stamp_ok = stamp.len() == 19
        && stamp.ends_with('Z')
        && stamp.as_bytes().get(8) == Some(&b'T')
        && stamp[..stamp.len() - 1].bytes().enumerate().all(|(i, b)| {
            if i == 8 {
                b == b'T'
            } else {
                b.is_ascii_digit()
            }
        });
    if !stamp_ok {
        return None;
    }
    Some((from.parse().ok()?, to.parse().ok()?, stamp.to_string()))
}

/// The backups beside a store, newest first. The time in the name orders them, whatever versions
/// they span. A file that does not follow the naming is no backup of this store and is skipped.
pub fn list_backups(store: &Path) -> Vec<BackupFile> {
    let dir = match store.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let Some(store_name) = store.file_name().map(|n| n.to_string_lossy().into_owned()) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut found: Vec<BackupFile> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let stem = name.strip_suffix(".bak")?;
            let tail = stem.strip_prefix(&store_name)?;
            let (from_version, to_version, stamp) = parse_backup_tail(tail)?;
            let meta = e.metadata().ok().filter(std::fs::Metadata::is_file)?;
            Some(BackupFile {
                path: e.path(),
                from_version,
                to_version,
                stamp,
                size: meta.len(),
            })
        })
        .collect();
    found.sort_by(|a, b| b.stamp.cmp(&a.stamp).then_with(|| b.path.cmp(&a.path)));
    found
}

/// Remove the oldest backups of a store, so that the newest `keep` stay.
fn prune_backups(store: &Path, keep: usize) {
    for old in list_backups(store).into_iter().skip(keep) {
        let _ = std::fs::remove_file(old.path);
    }
}

/// Write the checked backup that a migration from `from` to `to` needs. `VACUUM INTO` reads through
/// the connection, so it holds what the `-wal` holds. The file is made at mode 0600 before any byte is
/// written, synced, and then opened and checked. A check that does not answer `ok`, or a backup at
/// another version than the store, removes the backup and fails.
fn write_backup(
    conn: &Connection,
    store: &Path,
    from: u32,
    to: u32,
    after_write: &dyn Fn(&Path),
) -> Result<(), OpenError> {
    let dir = match store.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let store_name = store
        .file_name()
        .map_or_else(|| "store".to_string(), |n| n.to_string_lossy().into_owned());
    let target = dir.join(format!("{store_name}.v{from}-v{to}.{}.bak", backup_stamp()));
    let fail = |why: String| OpenError::Backup {
        path: target.display().to_string(),
        why,
    };
    let (keep, line) = backup_keep(std::env::var(BACKUP_KEEP_VAR).ok().as_deref());
    if let Some(line) = line {
        eprintln!("{line}");
    }
    // `VACUUM INTO` takes a file that does not exist or is empty: make it empty, at mode 0600.
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&target)
        .map_err(|e| fail(e.to_string()))?;
    let written = (|| -> Result<(), String> {
        conn.execute("VACUUM INTO ?1", [target.to_string_lossy().as_ref()])
            .map_err(|e| e.to_string())?;
        std::fs::set_permissions(&target, std::os::unix::fs::PermissionsExt::from_mode(0o600))
            .map_err(|e| e.to_string())?;
        std::fs::File::open(&target)
            .and_then(|f| f.sync_all())
            .map_err(|e| format!("cannot sync the backup: {e}"))?;
        // The directory entry too, so the file survives a power loss.
        let _ = std::fs::File::open(&dir).and_then(|d| d.sync_all());
        after_write(&target);
        let version = verify_file(&target)?;
        if version != from {
            return Err(format!(
                "the backup is at schema version {version}, and the store is at {from}"
            ));
        }
        Ok(())
    })();
    if let Err(why) = written {
        let _ = std::fs::remove_file(&target);
        return Err(fail(why));
    }
    prune_backups(store, keep);
    Ok(())
}

/// Apply the pending migrations, each in its own transaction. Forward only. After the active-run
/// check and before the first pending migration, it writes a checked backup of a store that has data
/// (version 1 or more): a failed backup applies no migration.
fn migrate(
    conn: &mut Connection,
    all: &[Migration],
    store: &Path,
    after_backup: &dyn Fn(&Path),
) -> Result<(), OpenError> {
    let sql = |e: rusqlite::Error| OpenError::Sqlite(e.to_string());
    let current = current_version(conn).map_err(sql)?;
    let supported = u32::try_from(all.len()).unwrap_or(u32::MAX);
    if current > supported {
        return Err(OpenError::SchemaNewer {
            database: current,
            supported,
        });
    }
    if current == supported {
        return Ok(());
    }
    if current >= 1 {
        let active: i64 = conn
            .query_row(
                "SELECT count(*) FROM runs WHERE state = 'active'",
                [],
                |r| r.get(0),
            )
            .map_err(sql)?;
        if active > 0 {
            return Err(OpenError::RunActive {
                current,
                pending: supported,
            });
        }
        write_backup(conn, store, current, supported, after_backup)?;
    }
    for m in all.iter().filter(|m| m.version > current) {
        let tx = conn.transaction().map_err(sql)?;
        tx.execute_batch(m.sql).map_err(sql)?;
        tx.execute(
            "INSERT INTO schema_version (version, name, applied_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![m.version, m.name, now_text()],
        )
        .map_err(sql)?;
        tx.commit().map_err(sql)?;
    }
    Ok(())
}

fn now_text() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    pf_engine::engine::format_utc(now.as_secs(), now.subsec_millis())
}

impl SqliteStore {
    /// The schema version recorded in the file.
    pub fn schema_version(&self) -> Result<u32, StoreError> {
        self.call(|conn| current_version(conn).map_err(sql_error))?
    }

    /// Open the store at `path`, creating the file when it does not exist, and apply the
    /// migrations of this binary.
    pub fn open(path: impl AsRef<Path>) -> Result<SqliteStore, OpenError> {
        SqliteStore::open_with_migrations(path, &[])
    }

    /// Like [`SqliteStore::open`], with more migrations after the built-in ones. The built-in
    /// list and `extra` together must be numbered 1, 2, 3 with no gap.
    pub fn open_with_migrations(
        path: impl AsRef<Path>,
        extra: &[Migration],
    ) -> Result<SqliteStore, OpenError> {
        SqliteStore::open_with_backup_hook(path, extra, &|_| {})
    }

    /// Like [`SqliteStore::open_with_migrations`], with a hook that runs on the backup file after it
    /// is written and before the store checks it. A test damages the backup there, to see the open
    /// fail and the backup go. The hook runs only when a migration needs a backup.
    pub fn open_with_backup_hook(
        path: impl AsRef<Path>,
        extra: &[Migration],
        after_backup: &dyn Fn(&Path),
    ) -> Result<SqliteStore, OpenError> {
        let all: Vec<Migration> = MIGRATIONS.iter().chain(extra).copied().collect();
        check_migration_list(&all)?;
        let mut conn =
            Connection::open(path.as_ref()).map_err(|e| OpenError::Sqlite(e.to_string()))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| OpenError::Sqlite(e.to_string()))?;
        let mode: String = conn
            .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
            .map_err(|e| OpenError::Sqlite(e.to_string()))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(OpenError::Sqlite(format!(
                "the journal mode is {mode}, not wal"
            )));
        }
        conn.execute_batch("PRAGMA synchronous = FULL;")
            .map_err(|e| OpenError::Sqlite(e.to_string()))?;
        migrate(&mut conn, &all, path.as_ref(), after_backup)?;
        SqliteStore::spawn(conn)
    }

    fn spawn(mut conn: Connection) -> Result<SqliteStore, OpenError> {
        let (tx, rx) = mpsc::channel::<Job>();
        let handle = std::thread::Builder::new()
            .name("pf-store-writer".to_string())
            .spawn(move || {
                for job in rx {
                    job(&mut conn);
                }
            })
            .map_err(|e| OpenError::Thread(e.to_string()))?;
        Ok(SqliteStore {
            tx: Some(tx),
            handle: Some(handle),
        })
    }

    /// Run `f` on the writer thread and wait for its result.
    fn call<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Connection) -> R + Send + 'static,
    ) -> Result<R, StoreError> {
        let stopped = || StoreError("the store writer thread has stopped".to_string());
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let job: Job = Box::new(move |conn| {
            let _ = reply_tx.send(f(conn));
        });
        self.tx
            .as_ref()
            .ok_or_else(stopped)?
            .send(job)
            .map_err(|_| stopped())?;
        reply_rx.recv().map_err(|_| stopped())
    }

    /// Read one `PRAGMA` value as text.
    pub fn pragma(&self, name: &str) -> Result<String, StoreError> {
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(StoreError(format!("{name:?} is not a pragma name")));
        }
        let sql = format!("PRAGMA {name}");
        self.call(move |conn| {
            conn.query_row(&sql, [], |row| {
                let value = row.get_ref(0)?;
                Ok(match value {
                    rusqlite::types::ValueRef::Integer(i) => i.to_string(),
                    rusqlite::types::ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
                    other => format!("{other:?}"),
                })
            })
            .map_err(sql_error)
        })?
    }

    /// The journal mode and the synchronous setting, read back through `PRAGMA`.
    pub fn settings(&self) -> Result<StoreSettings, StoreError> {
        let synchronous = match self.pragma("synchronous")?.as_str() {
            "0" => "OFF",
            "1" => "NORMAL",
            "2" => "FULL",
            "3" => "EXTRA",
            other => return Err(StoreError(format!("unknown synchronous value {other}"))),
        };
        Ok(StoreSettings {
            journal_mode: self.pragma("journal_mode")?,
            synchronous: synchronous.to_string(),
        })
    }
}

impl Drop for SqliteStore {
    fn drop(&mut self) {
        // Close the channel, then wait for the writer to finish its queue and close the file.
        self.tx.take();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

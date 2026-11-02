//! `pf-store-sqlite`: the first implementation of the `pf-engine` store port (ADR-0003).
//!
//! rusqlite, one serialized writer thread off the async path, WAL, `synchronous=FULL`.
//! The engine never sees SQL: it sees the port.

mod port_impl;

use std::path::Path;
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

/// The migrations of this binary. Migration `0001` holds only what skeleton A needs.
pub const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "init",
    sql: include_str!("../migrations/0001_init.sql"),
}];

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
fn current_version(conn: &Connection) -> rusqlite::Result<u32> {
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

/// Apply the pending migrations, each in its own transaction. Forward only.
fn migrate(conn: &mut Connection, all: &[Migration]) -> Result<(), OpenError> {
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
        migrate(&mut conn, &all)?;
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

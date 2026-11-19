//! The read path of the `store` verbs that run without the controller (#85, #67; `r2.s2.w2`).
//!
//! After a crash every host row can read live, and a store can hold frames that no checkpoint wrote
//! to the file, so the verbs read the live file the way a running controller does: a normal
//! connection that shares the WAL index, so it reads the frames in the `-wal`. It never opens the
//! store with `immutable=1`, which would skip them. The connection cannot write: `query_only` is on.
//! It cannot checkpoint: `wal_autocheckpoint` is 0 and `SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE` is set, so
//! closing the last connection leaves the `-wal` as it was. It never creates the file, and it never
//! migrates it.

use std::path::Path;

use rusqlite::config::DbConfig;
use rusqlite::{Connection, OpenFlags};

/// A host row that reads live, with what its launch records name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveHostRow {
    pub unit: String,
    pub session_kind: String,
    pub session_id: String,
    pub incarnation: u32,
    pub launcher: String,
    /// The host protocol version the host speaks.
    pub protocol: u32,
    pub host_binary: String,
    /// The managed CLI copies that the records of this incarnation name (`runtime.binary_path`), in
    /// every revision, so a resume from any of them finds its copy. Sorted, without repeats.
    pub cli_copies: Vec<String>,
}

/// A query-only connection to a store file.
pub struct Offline {
    conn: Connection,
}

impl Offline {
    /// Open the store at `path`. A file that does not exist is an error: the read path never makes one.
    pub fn open(path: &Path) -> Result<Offline, String> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| format!("cannot open the store {}: {e}", path.display()))?;
        let guard =
            |e: rusqlite::Error| format!("cannot set up the read path of {}: {e}", path.display());
        conn.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
            .map_err(guard)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(guard)?;
        conn.execute_batch("PRAGMA query_only = ON; PRAGMA wal_autocheckpoint = 0;")
            .map_err(guard)?;
        Ok(Offline { conn })
    }

    fn read<T>(&self, what: &str, r: rusqlite::Result<T>) -> Result<T, String> {
        r.map_err(|e| format!("cannot read {what} from the store: {e}"))
    }

    fn has_table(&self, name: &str) -> Result<bool, String> {
        let n: i64 = self.read(
            name,
            self.conn.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [name],
                |r| r.get(0),
            ),
        )?;
        Ok(n > 0)
    }

    /// The schema version recorded in the file. A file with no version table is at 0.
    pub fn schema_version(&self) -> Result<u32, String> {
        self.read("the schema version", crate::current_version(&self.conn))
    }

    /// The IDs of the runs that are active, in order. A store before the first migration has none.
    pub fn active_runs(&self) -> Result<Vec<String>, String> {
        if !self.has_table("runs")? {
            return Ok(Vec::new());
        }
        let mut stmt = self.read(
            "the runs",
            self.conn
                .prepare("SELECT id FROM runs WHERE state = 'active' ORDER BY id"),
        )?;
        let rows = self.read("the runs", stmt.query_map([], |r| r.get::<_, String>(0)))?;
        self.read("the runs", rows.collect())
    }

    /// The host rows with no end, in the order they started. A store with no `hosts` table (one
    /// that no binary with migration 0009 has opened) has none.
    pub fn live_host_rows(&self) -> Result<Vec<LiveHostRow>, String> {
        if !self.has_table("hosts")? {
            return Ok(Vec::new());
        }
        let mut stmt = self.read(
            "the hosts",
            self.conn.prepare(
                "SELECT unit, session_kind, session_id, incarnation, launcher, protocol, host_binary \
                 FROM hosts WHERE ended_at IS NULL ORDER BY rowid",
            ),
        )?;
        let rows = self.read(
            "the hosts",
            stmt.query_map([], |r| {
                Ok(LiveHostRow {
                    unit: r.get(0)?,
                    session_kind: r.get(1)?,
                    session_id: r.get(2)?,
                    incarnation: u32::try_from(r.get::<_, i64>(3)?).unwrap_or(u32::MAX),
                    launcher: r.get(4)?,
                    protocol: u32::try_from(r.get::<_, i64>(5)?).unwrap_or(u32::MAX),
                    host_binary: r.get(6)?,
                    cli_copies: Vec::new(),
                })
            }),
        )?;
        let mut hosts: Vec<LiveHostRow> = self.read("the hosts", rows.collect())?;
        let mut copies = self.read(
            "the launch records",
            self.conn.prepare(
                "SELECT DISTINCT json_extract(record, '$.runtime.binary_path') FROM launch_records \
                 WHERE session_kind = ?1 AND session_id = ?2 AND incarnation = ?3 \
                   AND json_type(record, '$.runtime.binary_path') = 'text' ORDER BY 1",
            ),
        )?;
        for host in &mut hosts {
            let found = self.read(
                "the launch records",
                copies.query_map(
                    rusqlite::params![
                        host.session_kind,
                        host.session_id,
                        i64::from(host.incarnation)
                    ],
                    |r| r.get::<_, String>(0),
                ),
            )?;
            host.cli_copies = self.read("the launch records", found.collect())?;
        }
        Ok(hosts)
    }
}

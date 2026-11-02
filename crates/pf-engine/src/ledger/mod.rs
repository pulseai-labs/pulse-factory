//! The ledger: one append-only file at the path the run gives the engine (ADR-0003).
//!
//! Only the engine writes it. Each line is whole, carries a sequence number that the store
//! assigns per ledger file (rising across runs), and is never rewritten.
//!
//! **Crash safety.** The store commits the transition, its command and receipt, and the ledger
//! line in one transaction, as an outbox row. Only then does the engine append the line to the
//! file, flush it with `sync_all`, and mark the row written. A failed append stops admission, as
//! a store write failure does.
//!
//! **Recovery at start-up.** The engine truncates a torn last line (one with no newline), reads
//! the sequence numbers left in the file, appends each unwritten outbox line that is missing, in
//! order, and marks it written. A stop before the append loses no line. A stop after the append
//! and before the mark adds no second line. A line can never precede its commit.

use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::admission::EngineError;
use crate::engine::Engine;

/// Format one ledger line, with its newline.
///
/// The body is cut to one line. A line reads `- #<seq> <body>`.
pub fn format_line(seq: u64, body: &str) -> String {
    let one_line: String = body
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    format!("- #{seq} {one_line}\n")
}

/// Read the sequence number from one ledger line. A line that the engine did not write has none.
pub fn parse_seq(line: &str) -> Option<u64> {
    let rest = line.strip_prefix("- #")?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || !rest[digits.len()..].starts_with(' ') {
        return None;
    }
    digits.parse().ok()
}

/// The file side of the ledger. The engine owns the one writer.
pub trait LedgerFile: Send {
    /// Cut a last line that has no newline. Returns true when something was cut.
    fn truncate_torn_tail(&mut self) -> std::io::Result<bool>;
    /// The sequence numbers of the whole lines in the file.
    fn seqs(&mut self) -> std::io::Result<BTreeSet<u64>>;
    /// Append one whole line (it ends with a newline) and flush it to disk with `sync_all`.
    fn append(&mut self, line: &str) -> std::io::Result<()>;
}

/// The ledger on the file system.
#[derive(Debug)]
pub struct FsLedger {
    path: PathBuf,
}

impl FsLedger {
    pub fn new(path: impl Into<PathBuf>) -> FsLedger {
        FsLedger { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read_bytes(&self) -> std::io::Result<Vec<u8>> {
        match std::fs::File::open(&self.path) {
            Ok(mut f) => {
                let mut bytes = Vec::new();
                f.read_to_end(&mut bytes)?;
                Ok(bytes)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }
}

impl LedgerFile for FsLedger {
    fn truncate_torn_tail(&mut self) -> std::io::Result<bool> {
        let bytes = self.read_bytes()?;
        if bytes.is_empty() || bytes.last() == Some(&b'\n') {
            return Ok(false);
        }
        let keep = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        let file = OpenOptions::new().write(true).open(&self.path)?;
        file.set_len(keep as u64)?;
        file.sync_all()?;
        Ok(true)
    }

    fn seqs(&mut self) -> std::io::Result<BTreeSet<u64>> {
        let bytes = self.read_bytes()?;
        let text = String::from_utf8_lossy(&bytes);
        Ok(text.lines().filter_map(parse_seq).collect())
    }

    fn append(&mut self, line: &str) -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(line.as_bytes())?;
        file.sync_all()
    }
}

/// What `recover_ledger` did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// A last line with no newline was cut.
    pub truncated_torn_tail: bool,
    /// Sequence numbers of the outbox lines that were missing from the file and are appended now.
    pub appended: Vec<u64>,
    /// Sequence numbers of the unwritten outbox lines that the file held already. They are marked
    /// written, and no second line is added.
    pub already_present: Vec<u64>,
}

impl Engine {
    /// Recover the ledger file after a stop. Run it before any other work at start-up.
    ///
    /// It cuts a torn last line, reads the sequence numbers left in the file, appends each
    /// unwritten outbox line that is missing, in order, and marks it written. A failure here
    /// stops admission, as a failed append does.
    pub fn recover_ledger(&self) -> Result<RecoveryReport, EngineError> {
        let mut inner = self.lock();
        if let Some(why) = &inner.degraded {
            return Err(EngineError::Degraded(why.clone()));
        }
        let mut report = RecoveryReport::default();

        let cut = inner.ledger.truncate_torn_tail();
        report.truncated_torn_tail =
            cut.map_err(|e| self.degrade(&mut inner, format!("ledger recovery failed: {e}")))?;
        let present = inner.ledger.seqs();
        let mut present = present
            .map_err(|e| self.degrade(&mut inner, format!("ledger recovery failed: {e}")))?;

        let pending = self.store.unwritten_ledger_lines(&self.ledger_file());
        let pending = pending.map_err(|e| {
            self.degrade(
                &mut inner,
                format!("ledger recovery failed: store read failed: {e}"),
            )
        })?;
        for line in pending {
            if present.contains(&line.seq) {
                self.store
                    .mark_ledger_written(&line.ledger_file, line.seq)
                    .map_err(|e| {
                        self.degrade(
                            &mut inner,
                            format!("marking ledger line {} written failed: {e}", line.seq),
                        )
                    })?;
                report.already_present.push(line.seq);
            } else {
                self.append_and_mark(&mut inner, &line).map_err(|why| {
                    self.degrade(&mut inner, format!("ledger recovery failed: {why}"))
                })?;
                present.insert(line.seq);
                report.appended.push(line.seq);
            }
        }
        Ok(report)
    }
}

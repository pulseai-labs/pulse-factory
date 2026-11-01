//! The queue unit source (Approach 4, ADR-0002, ADR-0003).
//!
//! The queue is a directory, `<state dir>/runs/<run id>/queue/`, that the controller owns. A seat
//! publishes one record per unit by atomic rename. A record is untrusted input (ADR-0005): it is
//! parsed strictly and never executed.
//!
//! Records never change. The same ID with the same content is a no-op. The same ID with changed
//! content, a malformed file or a file over 64 KiB stops the run's launches and merges: the
//! controller calls [`pf_engine::Engine::halt_for`] with a reason that names the file.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use pf_engine::admission::EngineError;
use pf_engine::port::unit_source::{UnitRecord, UnitSource, UnitSourceError};
use pf_engine::{Engine, IngestReport};

/// The largest record file the queue reads.
pub const MAX_RECORD_BYTES: u64 = 64 * 1024;

/// The source name that halts use.
pub const HALT_SOURCE: &str = "queue";

/// A file the queue saw and did not read, with the reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skipped {
    pub file: String,
    pub why: String,
}

/// The queue directory of one run, read as a unit source.
#[derive(Debug)]
pub struct QueueSource {
    dir: PathBuf,
    track: String,
    /// The bytes of each file at its first read.
    files: BTreeMap<String, Vec<u8>>,
    /// The canonical JSON of each unit ID read so far, and the file that set it.
    ids: BTreeMap<String, (String, String)>,
    skipped: Vec<Skipped>,
}

impl QueueSource {
    /// A source for the queue directory `dir`. Every unit it reads goes on `track`.
    pub fn new(dir: impl Into<PathBuf>, track: impl Into<String>) -> QueueSource {
        QueueSource {
            dir: dir.into(),
            track: track.into(),
            files: BTreeMap::new(),
            ids: BTreeMap::new(),
            skipped: Vec::new(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file that set a unit ID, when this source read it.
    pub fn file_of(&self, id: &str) -> Option<&str> {
        self.ids.get(id).map(|(_, file)| file.as_str())
    }

    /// The files the last poll did not read: symlinks and other non-regular files.
    pub fn skipped(&self) -> &[Skipped] {
        &self.skipped
    }
}

fn describe(dir: &Path, name: &str) -> String {
    format!("queue record {name} in {}", dir.display())
}

/// Read a file, at most `MAX_RECORD_BYTES` plus one byte.
fn read_bounded(path: &Path) -> std::io::Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(MAX_RECORD_BYTES + 1).read_to_end(&mut bytes)?;
    Ok(bytes)
}

impl UnitSource for QueueSource {
    fn poll(&mut self) -> Result<Vec<UnitRecord>, UnitSourceError> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(e) => e,
            // The controller creates the directory. Until it exists there is nothing to read.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(UnitSourceError(format!(
                    "cannot read the queue directory {}: {e}",
                    self.dir.display()
                )));
            }
        };
        let mut names: Vec<String> = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| {
                UnitSourceError(format!(
                    "cannot read the queue directory {}: {e}",
                    self.dir.display()
                ))
            })?;
            // A name that is not UTF-8 cannot be a record: a seat writes plain names.
            if let Some(name) = entry.file_name().to_str()
                && name.len() > ".json".len()
                && name.ends_with(".json")
            {
                names.push(name.to_string());
            }
        }
        names.sort();

        // Collect the new state first. A bad file leaves this source as it was.
        let mut skipped = Vec::new();
        let mut new_files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        let mut new_ids: BTreeMap<String, (String, String)> = BTreeMap::new();
        let mut out = Vec::new();
        for name in names {
            let path = self.dir.join(&name);
            let fail =
                |why: String| UnitSourceError(format!("{}: {why}", describe(&self.dir, &name)));
            let meta = match std::fs::symlink_metadata(&path) {
                Ok(m) => m,
                // The file went away since the listing.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(fail(format!("cannot be inspected: {e}"))),
            };
            if !meta.is_file() {
                let kind = if meta.file_type().is_symlink() {
                    "is a symlink"
                } else {
                    "is not a regular file"
                };
                skipped.push(Skipped {
                    file: name.clone(),
                    why: format!("{kind}, so the queue does not read it"),
                });
                continue;
            }
            if meta.len() > MAX_RECORD_BYTES {
                return Err(fail(format!(
                    "is {} bytes; the limit is {MAX_RECORD_BYTES}",
                    meta.len()
                )));
            }
            let bytes = read_bounded(&path).map_err(|e| fail(format!("cannot be read: {e}")))?;
            if bytes.len() as u64 > MAX_RECORD_BYTES {
                return Err(fail(format!(
                    "is over the limit of {MAX_RECORD_BYTES} bytes"
                )));
            }
            if let Some(first) = self.files.get(&name) {
                if *first != bytes {
                    return Err(fail(
                        "changed after it was read; records never change".to_string(),
                    ));
                }
                continue;
            }
            let text =
                std::str::from_utf8(&bytes).map_err(|_| fail("is not valid UTF-8".to_string()))?;
            let record =
                UnitRecord::from_json(text).map_err(|e| fail(format!("is malformed: {e}")))?;
            let canonical = record.to_json();
            let known = new_ids.get(&record.id).or_else(|| self.ids.get(&record.id));
            if let Some((first_json, first_file)) = known {
                if *first_json != canonical {
                    return Err(fail(format!(
                        "changes unit {}, which {first_file} set; records never change",
                        record.id
                    )));
                }
                new_files.insert(name, bytes);
                continue;
            }
            new_ids.insert(record.id.clone(), (canonical, name.clone()));
            new_files.insert(name, bytes);
            out.push(record);
        }
        self.files.extend(new_files);
        self.ids.extend(new_ids);
        self.skipped = skipped;
        Ok(out)
    }

    fn track(&self) -> Option<&str> {
        Some(&self.track)
    }
}

/// What one pass over the queue did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueuePoll {
    /// The records were admitted (or replayed).
    Ingested(IngestReport),
    /// The queue holds a bad record. The run is halted with this reason.
    Halted(String),
}

/// Read the queue and admit its records. A bad record halts the run through `halt_for`.
///
/// The run's launches and merges stop. A running unit's signals are still recorded.
pub fn ingest_queue(engine: &Engine, source: &mut QueueSource) -> Result<QueuePoll, EngineError> {
    let records = match source.poll() {
        Ok(records) => records,
        Err(UnitSourceError(reason)) => return halt(engine, &reason),
    };
    let mut report = IngestReport::default();
    for record in records {
        let id = record.id.clone();
        let file = source.file_of(&id).unwrap_or("?").to_string();
        let mut one = Fixed(Some(record), source.track().unwrap_or_default().to_string());
        match engine.ingest(&mut one) {
            Ok(r) => {
                report.admitted.extend(r.admitted);
                report.replayed.extend(r.replayed);
            }
            // The unit was admitted before, with other content: a record changed across a restart.
            Err(EngineError::Conflict { .. }) => {
                let reason = format!(
                    "{}: unit {id} was admitted with other content; records never change",
                    describe(source.dir(), &file)
                );
                return halt(engine, &reason);
            }
            Err(EngineError::Rejected(why)) => {
                let reason = format!("{}: {why}", describe(source.dir(), &file));
                return halt(engine, &reason);
            }
            Err(other) => return Err(other),
        }
    }
    Ok(QueuePoll::Ingested(report))
}

fn halt(engine: &Engine, reason: &str) -> Result<QueuePoll, EngineError> {
    match engine.halt_for(HALT_SOURCE, reason) {
        // The first halt from this source stays. A later bad record is reported, not recorded again.
        Ok(_) | Err(EngineError::Conflict { .. }) => Ok(QueuePoll::Halted(reason.to_string())),
        Err(other) => Err(other),
    }
}

/// A source that yields one record.
struct Fixed(Option<UnitRecord>, String);

impl UnitSource for Fixed {
    fn poll(&mut self) -> Result<Vec<UnitRecord>, UnitSourceError> {
        Ok(self.0.take().into_iter().collect())
    }

    fn track(&self) -> Option<&str> {
        Some(&self.1)
    }
}

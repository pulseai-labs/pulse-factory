//! The budget of real OMP sessions (operator decision, 2026-11-01).
//!
//! A real session spends money. At most [`LIMIT`] sessions are allowed in total, and only from
//! the marked probe tests (`tests/real_probe_*.rs`). A counter file holds the sessions used. A
//! launch that would pass the limit fails before it starts. The count is never reset.
//! A probe runs only with `PF_REAL_OMP=1`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use pf_engine::port::agent::{AgentPort, LaunchPlan, SessionRef};

use crate::OmpAdapter;

/// The most real sessions, in total.
pub const LIMIT: u32 = 20;

/// The model every probe uses.
pub const PROBE_MODEL: &str = "zai/glm-5.3-flash";

/// The counter file: `~/.local/state/pulse-factory/probe-sessions`. `HOME` decides where.
pub fn counter_path() -> PathBuf {
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from);
    home.join(".local/state/pulse-factory/probe-sessions")
}

/// Is the real-probe switch on?
pub fn enabled() -> bool {
    std::env::var("PF_REAL_OMP").is_ok_and(|v| v == "1")
}

/// The sessions used so far. A missing file is 0. A file that holds no number is an error: the
/// budget must not reset by accident.
pub fn used() -> Result<u32, String> {
    match std::fs::read_to_string(counter_path()) {
        Ok(t) => t
            .trim()
            .parse::<u32>()
            .map_err(|_| format!("{} holds no number", counter_path().display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(format!("cannot read {}: {e}", counter_path().display())),
    }
}

struct Lock(PathBuf);

impl Lock {
    fn take() -> Result<Lock, String> {
        let path = counter_path().with_extension("lock");
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("cannot make {dir:?}: {e}"))?;
        }
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(Lock(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if Instant::now() >= end {
                        return Err(format!("the counter lock {path:?} stays taken"));
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => return Err(format!("cannot take the counter lock: {e}")),
            }
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Reserve `n` sessions. It fails, and changes nothing, when the total would pass [`LIMIT`].
/// A probe that could start subagents reserves their count first.
pub fn reserve(n: u32) -> Result<u32, String> {
    let _lock = Lock::take()?;
    let before = used()?;
    let after = before.saturating_add(n);
    if after > LIMIT {
        return Err(format!(
            "probe budget: {before} of {LIMIT} sessions used, and {n} more would pass the limit"
        ));
    }
    std::fs::write(counter_path(), format!("{after}\n"))
        .map_err(|e| format!("cannot write {}: {e}", counter_path().display()))?;
    Ok(after)
}

/// What a probe launch did.
pub enum Probe {
    /// `PF_REAL_OMP` is not `1`. Nothing started and nothing was counted.
    Skipped,
    /// The session started. One session was counted, and `extra` more for subagents.
    Started(SessionRef),
}

/// Launch one probe session through the adapter. It checks the switch, reserves the budget and
/// only then starts the process.
pub fn launch(adapter: &OmpAdapter, plan: &LaunchPlan, extra: u32) -> Result<Probe, String> {
    if !enabled() {
        return Ok(Probe::Skipped);
    }
    reserve(1 + extra)?;
    adapter
        .launch(plan)
        .map(Probe::Started)
        .map_err(|e| e.to_string())
}

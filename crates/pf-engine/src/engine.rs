//! The engine: one control authority that admits commands one at a time.
//!
//! `submit` is the core. It serializes admissions with one lock, finds a stored receipt for the
//! command ID, decides, commits through the store port and appends the ledger line. The other
//! methods build commands with stable IDs and call `submit`.
//!
//! **Store write failure** (ADR-0006): the engine stops admitting work, writes the failure to
//! `health.json` in the state directory (outside the store) and to stderr, which the systemd
//! journal collects. A failed ledger append does the same.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::admission::{
    Admission, Command, CommandBody, EngineError, RunView, canonical_content, decide,
};
use crate::capacity::Capacity;
use crate::ledger::{FsLedger, LedgerFile, format_line};
use crate::port::agent::{AgentPort, Usage};
use crate::port::forge::{ForgePort, PrRef};
use crate::port::store::{
    Commit, Committed, LedgerEntry, LedgerLine, NewAdmission, Store, StoreError,
};
use crate::port::unit_source::UnitSource;
use crate::status::EndStatus;
use crate::unit::Signal;
use crate::workflow::Workflow;

/// A source of UTC timestamps, such as `2026-10-31T14:07:09.803Z`.
pub type Clock = Arc<dyn Fn() -> String + Send + Sync>;

/// The clock of the host.
pub fn system_clock() -> Clock {
    Arc::new(|| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        format_utc(now.as_secs(), now.subsec_millis())
    })
}

/// Format seconds since the Unix epoch as UTC text.
pub fn format_utc(secs: u64, millis: u32) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or(i64::MAX / 2);
    let rem = secs % 86_400;
    // Civil date from a day count (proleptic Gregorian calendar).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

/// What a run gives the engine.
#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub run_id: String,
    pub workflow: Workflow,
    /// The one append-only ledger file the run gives the engine.
    pub ledger_path: PathBuf,
    /// Where `health.json` goes. It is outside the store.
    pub state_dir: PathBuf,
}

pub(crate) struct Inner {
    pub(crate) degraded: Option<String>,
    pub(crate) ledger: Box<dyn LedgerFile>,
}

pub struct Engine {
    pub(crate) cfg: EngineConfig,
    pub(crate) store: Arc<dyn Store>,
    pub(crate) agent: Arc<dyn AgentPort>,
    pub(crate) forge: Arc<dyn ForgePort>,
    pub(crate) clock: Clock,
    pub(crate) capacity: Capacity,
    pub(crate) inner: Mutex<Inner>,
}

/// What `ingest` did with the records of a source.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IngestReport {
    pub admitted: Vec<String>,
    pub replayed: Vec<String>,
}

impl Engine {
    /// Build an engine. Nothing is read or written yet: call [`Engine::start_up`].
    pub fn new(
        cfg: EngineConfig,
        store: Arc<dyn Store>,
        agent: Arc<dyn AgentPort>,
        forge: Arc<dyn ForgePort>,
        clock: Clock,
        ledger: Box<dyn LedgerFile>,
    ) -> Result<Engine, EngineError> {
        let capacity = Capacity::new(&cfg.workflow.settings)
            .map_err(|e| EngineError::Rejected(e.to_string()))?;
        Ok(Engine {
            cfg,
            store,
            agent,
            forge,
            clock,
            capacity,
            inner: Mutex::new(Inner {
                degraded: None,
                ledger,
            }),
        })
    }

    /// Build an engine on the host clock and the ledger file of the config, then start it up.
    pub fn start(
        cfg: EngineConfig,
        store: Arc<dyn Store>,
        agent: Arc<dyn AgentPort>,
        forge: Arc<dyn ForgePort>,
    ) -> Result<Engine, EngineError> {
        let ledger = Box::new(FsLedger::new(cfg.ledger_path.clone()));
        let engine = Engine::new(cfg, store, agent, forge, system_clock(), ledger)?;
        engine.start_up()?;
        Ok(engine)
    }

    /// Start-up: recover the ledger, open the run, then reconcile.
    pub fn start_up(&self) -> Result<(), EngineError> {
        self.recover_ledger()?;
        self.open_run()?;
        self.reconcile()?;
        Ok(())
    }

    pub fn run_id(&self) -> &str {
        &self.cfg.run_id
    }

    pub fn workflow(&self) -> &Workflow {
        &self.cfg.workflow
    }

    pub fn capacity(&self) -> &Capacity {
        &self.capacity
    }

    pub fn now(&self) -> String {
        (self.clock)()
    }

    pub(crate) fn ledger_file(&self) -> String {
        self.cfg.ledger_path.display().to_string()
    }

    /// The path of `health.json`.
    pub fn health_path(&self) -> PathBuf {
        self.cfg.state_dir.join("health.json")
    }

    /// The reason the engine stopped admitting work, when it did.
    pub fn degraded(&self) -> Option<String> {
        self.lock().degraded.clone()
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Open the run. The same call at a later start-up returns the stored receipt.
    pub fn open_run(&self) -> Result<Admission, EngineError> {
        self.submit(Command {
            id: format!("open-run:{}", self.cfg.run_id),
            body: CommandBody::OpenRun {
                workflow: self.cfg.workflow.name.clone(),
                ledger_file: self.ledger_file(),
            },
        })
    }

    /// Read the stored state of the run.
    pub fn view(&self) -> Result<RunView, EngineError> {
        let run = &self.cfg.run_id;
        let read = || -> Result<RunView, StoreError> {
            Ok(RunView {
                run: self.store.run(run)?,
                nodes: self.store.nodes(run)?,
                sessions: self.store.sessions(run)?,
                admissions: self.store.admissions(run)?,
            })
        };
        read().map_err(|e| EngineError::Store(e.to_string()))
    }

    /// Admit one command, or return its stored receipt.
    pub fn submit(&self, command: Command) -> Result<Admission, EngineError> {
        let mut inner = self.lock();
        if let Some(why) = &inner.degraded {
            return Err(EngineError::Degraded(why.clone()));
        }
        self.submit_locked(&mut inner, command)
    }

    fn submit_locked(&self, inner: &mut Inner, command: Command) -> Result<Admission, EngineError> {
        let run_id = &self.cfg.run_id;
        let content = canonical_content(run_id, &command);

        match self.store.admission(run_id, &command.id) {
            Err(e) => return Err(self.degrade(inner, format!("store read failed: {e}"))),
            Ok(Some(row)) => {
                return if row.content == content {
                    Ok(Admission {
                        row,
                        replayed: true,
                    })
                } else {
                    Err(EngineError::Conflict {
                        command_id: command.id,
                    })
                };
            }
            Ok(None) => {}
        }

        let view = match self.view() {
            Ok(v) => v,
            Err(e) => return Err(self.degrade(inner, format!("store read failed: {e}"))),
        };
        let now = self.now();
        let decision = decide(&view, run_id, &self.cfg.workflow, &command, &now)
            .map_err(EngineError::Rejected)?;

        let ledger = decision.ledger.as_ref().map(|text| {
            let unit = decision
                .unit_id
                .as_ref()
                .map(|u| format!(" {u}"))
                .unwrap_or_default();
            LedgerEntry {
                ledger_file: self.ledger_file(),
                body: format!("{now} {run_id} {}{unit} | {text}", command.body.kind()),
            }
        });
        let commit = Commit {
            run_id: run_id.clone(),
            command_id: command.id.clone(),
            content,
            admission: NewAdmission {
                kind: command.body.kind().to_string(),
                outcome: decision.outcome,
                reason: decision.reason,
                unit_id: decision.unit_id,
                role: decision.role,
                model: decision.model,
                attempt: decision.attempt,
                admitted_at: now,
            },
            effects: decision.effects,
            ledger,
        };

        match self.store.commit(commit) {
            Err(e) => Err(self.degrade(inner, format!("store write failed: {e}"))),
            Ok(Committed::Replayed(row)) => Ok(Admission {
                row,
                replayed: true,
            }),
            Ok(Committed::Conflict(_)) => Err(EngineError::Conflict {
                command_id: command.id,
            }),
            Ok(Committed::Applied { admission, ledger }) => {
                if let Some(line) = ledger
                    && let Err(e) = self.append_and_mark(inner, &line)
                {
                    return Err(
                        self.degrade(inner, format!("{e} (command {} was committed)", command.id))
                    );
                }
                Ok(Admission {
                    row: admission,
                    replayed: false,
                })
            }
        }
    }

    /// Append one outbox line to the file, flush it, and mark the outbox row written.
    pub(crate) fn append_and_mark(
        &self,
        inner: &mut Inner,
        line: &LedgerLine,
    ) -> Result<(), String> {
        inner
            .ledger
            .append(&format_line(line.seq, &line.body))
            .map_err(|e| format!("ledger append failed: {e}"))?;
        self.store
            .mark_ledger_written(&line.ledger_file, line.seq)
            .map_err(|e| format!("marking ledger line {} written failed: {e}", line.seq))
    }

    /// Stop admitting work, write `health.json` and stderr, and return the error.
    pub(crate) fn degrade(&self, inner: &mut Inner, why: String) -> EngineError {
        inner.degraded = Some(why.clone());
        eprintln!("pf-engine: run {} degraded: {why}", self.cfg.run_id);
        let body = serde_json::json!({
            "status": "degraded",
            "run": self.cfg.run_id,
            "reason": why,
            "at": self.now(),
        });
        let path = self.health_path();
        let tmp = path.with_extension("json.tmp");
        let written = std::fs::create_dir_all(&self.cfg.state_dir)
            .and_then(|()| std::fs::write(&tmp, body.to_string()))
            .and_then(|()| std::fs::rename(&tmp, &path));
        if let Err(e) = written {
            eprintln!("pf-engine: cannot write {}: {e}", path.display());
        }
        EngineError::Degraded(why)
    }

    /// Admit the units a source has ready. A unit ID that was admitted before is a replay.
    pub fn ingest(&self, source: &mut dyn UnitSource) -> Result<IngestReport, EngineError> {
        let records = source
            .poll()
            .map_err(|e| EngineError::Port(e.to_string()))?;
        let mut report = IngestReport::default();
        for record in records {
            let id = record.id.clone();
            let admission = self.submit(Command {
                id: format!("unit:{id}"),
                body: CommandBody::AdmitUnit { unit: record },
            })?;
            if admission.replayed {
                report.replayed.push(id);
            } else {
                report.admitted.push(id);
            }
        }
        Ok(report)
    }

    /// A signal ends the unit's running stage `stage`.
    pub fn signal(
        &self,
        unit_id: &str,
        stage: &str,
        signal: Signal,
        pr: Option<PrRef>,
    ) -> Result<Admission, EngineError> {
        let view = self.view()?;
        let node = view
            .stages_of(unit_id)
            .into_iter()
            .rfind(|n| n.detail == stage)
            .ok_or_else(|| {
                EngineError::Rejected(format!("unit {unit_id} has no stage {stage} yet"))
            })?;
        self.submit(Command {
            id: format!("signal:{}", node.id),
            body: CommandBody::Signal {
                unit_id: unit_id.to_string(),
                stage: stage.to_string(),
                signal,
                pr,
            },
        })
    }

    /// Add one reading of token usage and cost to a session. `reading_id` makes a retry safe.
    pub fn record_usage(
        &self,
        session_id: &str,
        usage: Usage,
        reading_id: &str,
    ) -> Result<Admission, EngineError> {
        self.submit(Command {
            id: format!("usage:{session_id}:{reading_id}"),
            body: CommandBody::Usage {
                session_id: session_id.to_string(),
                usage,
            },
        })
    }

    /// Park a unit that is `running`. Every halt condition comes through here.
    pub fn park(&self, unit_id: &str, reason: &str) -> Result<Admission, EngineError> {
        self.submit(Command {
            id: format!("park:{unit_id}"),
            body: CommandBody::Park {
                unit_id: unit_id.to_string(),
                reason: reason.to_string(),
            },
        })
    }

    /// End a node with `done`, `failed` or `stopped`.
    pub fn end(
        &self,
        node_id: &str,
        status: EndStatus,
        reason: &str,
    ) -> Result<Admission, EngineError> {
        self.submit(Command {
            id: format!("end:{node_id}"),
            body: CommandBody::End {
                node_id: node_id.to_string(),
                status,
                reason: reason.to_string(),
            },
        })
    }

    /// The `halt` command: the run admits no further launch and no further merge.
    pub fn halt(&self, reason: &str) -> Result<Admission, EngineError> {
        self.submit(Command {
            id: format!("halt:{}", self.cfg.run_id),
            body: CommandBody::Halt {
                reason: reason.to_string(),
            },
        })
    }

    /// End the run.
    pub fn end_run(&self, reason: &str) -> Result<Admission, EngineError> {
        self.submit(Command {
            id: format!("end-run:{}", self.cfg.run_id),
            body: CommandBody::EndRun {
                reason: reason.to_string(),
            },
        })
    }
}

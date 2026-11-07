//! The controller's own commands: the one path that project and device commands take
//! (ADR-0002, ADR-0003, ADR-0004, ADR-0009, ADR-0012).
//!
//! Each command carries a command ID and commits under the admission scope `@controller`, so the
//! store admits it once. The same ID with the same content is a replay and returns the first
//! record. The same ID with other content is a conflict. A command the controller refuses (a bad
//! root or name, a duplicate, an unknown ID) is an admission with the outcome `refused`, its
//! reason and no effect. Every admitted or refused new command is one line of
//! `<state dir>/controller.ledger`, written by one writer with the ledger the run engines use.
//!
//! The commands launch nothing and change no run, so `halt` does not apply to them.

use std::path::PathBuf;
use std::sync::MutexGuard;

use pf_engine::ledger::{FsLedger, append_and_mark_line, recover_ledger_file};
use pf_engine::port::store::{
    AdmissionOutcome, AdmissionRow, CONTROLLER_SCOPE, Commit, Committed, Effect, LedgerEntry,
    LedgerLine, NewAdmission, Store,
};
use pf_engine::status::{one_line_reason, redact};

use super::{Controller, lock};

/// The controller ledger's file name in the state directory.
pub const LEDGER_FILE: &str = "controller.ledger";

/// The one command kind that still commits while the ledger is stopped.
pub const DEVICE_REVOKE: &str = "device_revoke";

/// A command ID is 1 to 128 characters.
pub const COMMAND_ID_MAX_CHARS: usize = 128;

/// A command ID is 1 to 128 characters of letters, digits and `.`, `_`, `-`, `:`.
pub fn check_command_id(id: &str) -> Result<(), String> {
    let n = id.chars().count();
    let plain = id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':'));
    if n == 0 || n > COMMAND_ID_MAX_CHARS || !plain {
        return Err(format!(
            "a command ID is 1 to {COMMAND_ID_MAX_CHARS} characters of letters, digits and . _ - :"
        ));
    }
    Ok(())
}

/// The controller ledger and its one writer. The lock around it spans a commit and the append of
/// its line, so the lines reach the file in the order their sequence numbers have.
pub struct ControllerLedger {
    path: PathBuf,
    file: FsLedger,
    /// Why the commands stopped: a ledger that could not be recovered or appended to.
    stopped: Option<String>,
}

impl ControllerLedger {
    /// Recover the ledger file, as a run's engine does at start-up: cut a torn last line and
    /// append each outbox line that is missing. A failure stops the controller's commands. It does
    /// not stop the controller, and the reads still work.
    pub fn start(store: &dyn Store, path: PathBuf) -> ControllerLedger {
        let mut file = FsLedger::new(path.clone());
        let name = path.display().to_string();
        let stopped = match recover_ledger_file(store, &mut file, &name) {
            Ok(report) => {
                for note in report.notes() {
                    eprintln!("pulse-factory: controller ledger: {note}");
                }
                None
            }
            Err(why) => {
                eprintln!("pulse-factory: controller ledger: {why}");
                Some(why)
            }
        };
        ControllerLedger {
            path,
            file,
            stopped,
        }
    }

    fn name(&self) -> String {
        self.path.display().to_string()
    }
}

/// A ledger value: as it is when it is plain, a JSON string when it holds a space, a quote, a
/// backslash or a control character. A secret in it is redacted first.
pub fn field(value: &str) -> String {
    let value = redact(value);
    let plain = !value.is_empty()
        && !value
            .chars()
            .any(|c| c.is_whitespace() || c == '"' || c == '\\' || c.is_control());
    if plain {
        value
    } else {
        serde_json::to_string(&value).unwrap_or_default()
    }
}

pub fn now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    pf_engine::engine::format_utc(now.as_secs(), now.subsec_millis())
}

/// What a command asks of the store, once the controller has judged it.
pub enum Plan {
    /// The controller refuses it, with this one-line reason.
    Refuse(String),
    /// The store applies these effects. `subject` is the entity the command names, or
    /// [`pf_engine::port::store::NEW_ID`] for the ID the store assigns. `line` is the admitted
    /// ledger line.
    Admit {
        effects: Vec<Effect>,
        subject: String,
        line: String,
    },
}

impl Controller {
    /// Why the controller's commands are stopped, or `None` while they run.
    pub(super) fn commands_stopped(&self) -> Option<String> {
        lock(&self.controller_ledger).stopped.clone()
    }

    /// Commit one controller command, append its ledger line, and return the stored admission and
    /// whether it was a replay. A refused command comes back as `Err` with its reason, which is
    /// redacted and cut to one line. So does every failure: a stopped ledger, a conflict, a store
    /// error.
    pub(super) fn run_command(
        &self,
        kind: &'static str,
        command_id: &str,
        content: String,
        plan: Plan,
    ) -> Result<(AdmissionRow, bool), String> {
        let mut ledger = lock(&self.controller_ledger);
        // A stopped ledger refuses every command but a revoke. A revoke cuts a device off, and the
        // API reads the store on each call, so the operator must still be able to make one. It
        // commits to the store and its line waits in the outbox for the next start to append.
        let stopped = ledger.stopped.clone();
        if let Some(why) = &stopped
            && kind != DEVICE_REVOKE
        {
            return Err(format!("controller commands are stopped: {why}"));
        }
        let committed = match plan {
            Plan::Refuse(why) => self.commit_refusal(&ledger, kind, command_id, &content, &why),
            Plan::Admit {
                effects,
                subject,
                line,
            } => {
                let commit = Commit {
                    run_id: CONTROLLER_SCOPE.to_string(),
                    command_id: command_id.to_string(),
                    content: content.clone(),
                    admission: NewAdmission {
                        kind: kind.to_string(),
                        outcome: AdmissionOutcome::Admitted,
                        reason: None,
                        unit_id: Some(subject),
                        role: None,
                        model: None,
                        attempt: None,
                        admitted_at: now(),
                    },
                    effects,
                    ledger: Some(LedgerEntry {
                        ledger_file: ledger.name(),
                        body: line,
                    }),
                };
                match self.store.commit(commit) {
                    // The store refused an effect and wrote nothing. The refusal is a command
                    // of its own: an admission with the reason, and one ledger line.
                    Ok(Committed::Rejected(why)) => {
                        self.commit_refusal(&ledger, kind, command_id, &content, &why)
                    }
                    other => other.map_err(|e| e.to_string()),
                }
            }
        }?;
        match committed {
            Committed::Applied {
                admission,
                ledger: line,
            } => {
                if let Some(line) = line
                    && stopped.is_none()
                    && let Err(why) = self.append_line(&mut ledger, &line)
                {
                    return Err(format!(
                        "{kind} command {command_id} was committed, but its ledger line was not \
                         written ({why}); controller commands are stopped until the controller \
                         restarts"
                    ));
                }
                refusal_to_err(admission, false)
            }
            Committed::Replayed(admission) => refusal_to_err(admission, true),
            Committed::Conflict(_) => Err(format!(
                "command {command_id} was used before with other arguments"
            )),
            Committed::Rejected(why) => Err(why),
        }
    }

    /// Commit a refused admission: the reason, no effect and one ledger line.
    fn commit_refusal(
        &self,
        ledger: &MutexGuard<'_, ControllerLedger>,
        kind: &str,
        command_id: &str,
        content: &str,
        why: &str,
    ) -> Result<Committed, String> {
        // The reason is redacted and cut to one line once, here. The admission, the ledger line
        // and the answer all carry this text.
        let reason = one_line_reason(why);
        let body = format!(
            "{kind} refused reason={} command={command_id}",
            serde_json::to_string(&reason).unwrap_or_default()
        );
        self.store
            .commit(Commit {
                run_id: CONTROLLER_SCOPE.to_string(),
                command_id: command_id.to_string(),
                content: content.to_string(),
                admission: NewAdmission {
                    kind: kind.to_string(),
                    outcome: AdmissionOutcome::Refused,
                    reason: Some(reason),
                    unit_id: None,
                    role: None,
                    model: None,
                    attempt: None,
                    admitted_at: now(),
                },
                effects: Vec::new(),
                ledger: Some(LedgerEntry {
                    ledger_file: ledger.name(),
                    body,
                }),
            })
            .map_err(|e| e.to_string())
    }

    /// Append one committed line to the file and mark it written. A failure stops the controller's
    /// commands: the line stays in the outbox, and the next start appends it.
    fn append_line(
        &self,
        ledger: &mut MutexGuard<'_, ControllerLedger>,
        line: &LedgerLine,
    ) -> Result<(), String> {
        let ControllerLedger { file, stopped, .. } = &mut **ledger;
        append_and_mark_line(self.store.as_ref(), file, line).inspect_err(|why| {
            eprintln!("pulse-factory: controller ledger: {why}");
            *stopped = Some(why.clone());
        })
    }
}

/// A stored admission as a result: the admission, or the reason it was refused.
fn refusal_to_err(admission: AdmissionRow, replayed: bool) -> Result<(AdmissionRow, bool), String> {
    if admission.outcome == AdmissionOutcome::Refused {
        return Err(admission.reason.unwrap_or_default());
    }
    Ok((admission, replayed))
}

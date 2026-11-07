//! Projects: the verbs `project_add`, `project_list` and `project_remove`, and the controller
//! ledger they write (ADR-0002, ADR-0003, ADR-0004, ADR-0009).
//!
//! A project is a controller entity with an ID, a name and one root. Each change is a controller
//! command: it carries a command ID and commits under the admission scope `@controller`, so the
//! store admits it once. The same ID with the same content is a replay and returns the first
//! record. The same ID with other content is a conflict. A command the controller refuses (a bad
//! root or name, a duplicate, an unknown ID) is an admission with the outcome `refused`, its
//! reason and no effect. Every admitted or refused new command is one line of
//! `<state dir>/controller.ledger`, written by one writer with the ledger the run engines use.
//!
//! The commands launch nothing and change no run, so `halt` does not apply to them.
//!
//! The argument and answer shapes are the serde types of this module. Other front ends (the
//! network API) take their types from these structs.

use std::path::{Path, PathBuf};
use std::sync::MutexGuard;

use pf_engine::ledger::{FsLedger, append_and_mark_line, recover_ledger_file};
use pf_engine::port::store::{
    AdmissionOutcome, AdmissionRow, CONTROLLER_SCOPE, Commit, Committed, Effect, LedgerEntry,
    NEW_ID, NewAdmission, NewProject, ProjectRow, Store,
};
use pf_engine::status::{one_line_reason, redact};
use pf_ipc::Response;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Controller, lock};

/// The controller ledger's file name in the state directory.
pub const LEDGER_FILE: &str = "controller.ledger";

/// The `schema_version` of the answers (ADR-0004).
pub const SCHEMA_VERSION: u32 = 1;

/// A name is 1 to 64 characters.
pub const NAME_MAX_CHARS: usize = 64;

/// A command ID is 1 to 128 characters.
pub const COMMAND_ID_MAX_CHARS: usize = 128;

/// A project ID names a stored row, so it is short. A longer text is no ID.
const PROJECT_ID_MAX_CHARS: usize = 64;

// ------------------------------------------------------------------ the shapes

/// The arguments of `project_add`. `root` is an absolute path. The controller makes it canonical.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectAddArgs {
    pub root: String,
    /// The default is the last component of the canonical root.
    #[serde(default)]
    pub name: Option<String>,
    pub command_id: String,
}

/// The arguments of `project_remove`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRemoveArgs {
    pub id: String,
    pub command_id: String,
}

/// One project, as every verb shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRecord {
    pub id: String,
    pub name: String,
    pub root: String,
    pub created_at: String,
    pub removed_at: Option<String>,
}

impl From<ProjectRow> for ProjectRecord {
    fn from(p: ProjectRow) -> ProjectRecord {
        ProjectRecord {
            id: p.id,
            name: p.name,
            root: p.root,
            created_at: p.created_at,
            removed_at: p.removed_at,
        }
    }
}

/// The answer of `project_add` and `project_remove`. `replayed` is true when the command ID was
/// admitted before with the same content: the project is the first record's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectChanged {
    pub schema_version: u32,
    pub replayed: bool,
    pub project: ProjectRecord,
}

/// The answer of `project_list`: the live projects, in creation order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectList {
    pub schema_version: u32,
    pub projects: Vec<ProjectRecord>,
}

/// The content of a `project_add` admission: its arguments after the root is made canonical and
/// the name has its default. The store compares these bytes, so the field order is fixed.
#[derive(Serialize)]
struct AddContent<'a> {
    name: &'a str,
    root: &'a str,
}

/// The content of a `project_remove` admission.
#[derive(Serialize)]
struct RemoveContent<'a> {
    id: &'a str,
}

// ------------------------------------------------------------------ the rules

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

/// A name is 1 to 64 characters with no control character and no line break.
pub fn check_name(name: &str) -> Result<(), String> {
    let n = name.chars().count();
    if n == 0 {
        return Err("the name is empty".to_string());
    }
    if n > NAME_MAX_CHARS {
        return Err(format!(
            "the name has {n} characters, and the limit is {NAME_MAX_CHARS}"
        ));
    }
    if name
        .chars()
        .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
    {
        return Err("the name holds a control character or a line break".to_string());
    }
    Ok(())
}

/// The root as the command records it, and whether the root is acceptable.
///
/// The root is made canonical (`std::fs::canonicalize`), so two spellings of one directory (a
/// symlink, a `..`) are one root. A root that cannot be made canonical stays as it was given. The
/// controller does not look inside the root: a git repo and a plain folder are the same to it.
fn resolve_root(given: &str) -> (String, Result<(), String>) {
    let path = Path::new(given);
    if !path.is_absolute() {
        return (
            given.to_string(),
            Err(format!("the root is not an absolute path: {given}")),
        );
    }
    let canonical = match std::fs::canonicalize(path) {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (
                given.to_string(),
                Err(format!("the root does not exist: {given}")),
            );
        }
        Err(e) => {
            return (
                given.to_string(),
                Err(format!("the root cannot be read: {given}: {e}")),
            );
        }
    };
    let Some(text) = canonical.to_str().map(str::to_string) else {
        return (
            given.to_string(),
            Err(format!("the root is not valid UTF-8: {given}")),
        );
    };
    if canonical == Path::new("/") {
        return (text, Err("the root is the file system root".to_string()));
    }
    if !canonical.is_dir() {
        return (
            text.clone(),
            Err(format!("the root is not a directory: {text}")),
        );
    }
    (text, Ok(()))
}

/// The last path component of a root, which is the default name.
fn default_name(root: &str) -> String {
    Path::new(root)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

// ------------------------------------------------------------------ the ledger

/// The controller ledger and its one writer. The lock around it spans a commit and the append of
/// its line, so the lines reach the file in the order their sequence numbers have.
pub struct ProjectLedger {
    path: PathBuf,
    file: FsLedger,
    /// Why project commands stopped: a ledger that could not be recovered or appended to.
    stopped: Option<String>,
}

impl ProjectLedger {
    /// Recover the ledger file, as a run's engine does at start-up: cut a torn last line and
    /// append each outbox line that is missing. A failure stops the project commands. It does not
    /// stop the controller, and the reads still work.
    pub fn start(store: &dyn Store, path: PathBuf) -> ProjectLedger {
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
        ProjectLedger {
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
fn field(value: &str) -> String {
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

fn now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    pf_engine::engine::format_utc(now.as_secs(), now.subsec_millis())
}

// ------------------------------------------------------------------ the commands

/// What a command asks of the store, once the controller has judged it.
enum Plan {
    /// The controller refuses it, with this one-line reason.
    Refuse(String),
    /// The store applies these effects. `subject` is the project the command names, or
    /// [`NEW_ID`] for the ID the store assigns. `line` is the admitted ledger line.
    Admit {
        effects: Vec<Effect>,
        subject: String,
        line: String,
    },
}

impl Controller {
    pub(super) fn verb_project_add(&self, args: &Value) -> Response {
        let args: ProjectAddArgs = match serde_json::from_value(args.clone()) {
            Ok(a) => a,
            Err(e) => return Response::err(format!("the arguments are not readable: {e}")),
        };
        if let Err(why) = check_command_id(&args.command_id) {
            return Response::err(why);
        }
        let (root, root_ok) = resolve_root(&args.root);
        let name = args.name.clone().unwrap_or_else(|| default_name(&root));
        let content = serde_json::to_string(&AddContent {
            name: &name,
            root: &root,
        })
        .unwrap_or_default();
        let plan = match root_ok.and_then(|()| check_name(&name)) {
            Err(why) => Plan::Refuse(why),
            Ok(()) => Plan::Admit {
                effects: vec![Effect::AddProject(NewProject {
                    name: name.clone(),
                    root: root.clone(),
                    created_at: now(),
                })],
                subject: NEW_ID.to_string(),
                line: format!(
                    "project_add {NEW_ID} admitted name={} root={} command={}",
                    field(&name),
                    field(&root),
                    args.command_id
                ),
            },
        };
        self.project_command("project_add", &args.command_id, content, plan)
    }

    pub(super) fn verb_project_remove(&self, args: &Value) -> Response {
        let args: ProjectRemoveArgs = match serde_json::from_value(args.clone()) {
            Ok(a) => a,
            Err(e) => return Response::err(format!("the arguments are not readable: {e}")),
        };
        if let Err(why) = check_command_id(&args.command_id) {
            return Response::err(why);
        }
        let n = args.id.chars().count();
        if n == 0 || n > PROJECT_ID_MAX_CHARS {
            return Response::err(format!(
                "a project ID is 1 to {PROJECT_ID_MAX_CHARS} characters"
            ));
        }
        let content = serde_json::to_string(&RemoveContent { id: &args.id }).unwrap_or_default();
        // The store knows whether the ID names a live project, and its answer decides.
        let plan = Plan::Admit {
            effects: vec![Effect::RemoveProject {
                id: args.id.clone(),
                removed_at: now(),
            }],
            subject: args.id.clone(),
            line: format!(
                "project_remove {} admitted command={}",
                field(&args.id),
                args.command_id
            ),
        };
        self.project_command("project_remove", &args.command_id, content, plan)
    }

    pub(super) fn verb_project_list(&self) -> Response {
        match self.store.projects() {
            Ok(rows) => Response::ok(
                serde_json::to_value(ProjectList {
                    schema_version: SCHEMA_VERSION,
                    projects: rows.into_iter().map(ProjectRecord::from).collect(),
                })
                .unwrap_or(Value::Null),
            ),
            Err(e) => Response::err(e.to_string()),
        }
    }

    /// Commit one project command, append its ledger line, and answer.
    fn project_command(
        &self,
        kind: &'static str,
        command_id: &str,
        content: String,
        plan: Plan,
    ) -> Response {
        let mut ledger = lock(&self.project_ledger);
        if let Some(why) = &ledger.stopped {
            return Response::err(format!("project commands are stopped: {why}"));
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
        };
        let committed = match committed {
            Ok(c) => c,
            Err(why) => return Response::err(why),
        };
        match committed {
            Committed::Applied {
                admission,
                ledger: line,
            } => {
                if let Some(line) = line
                    && let Err(why) = self.append_line(&mut ledger, &line)
                {
                    return Response::err(format!(
                        "{kind} command {command_id} was committed, but its ledger line was not \
                         written ({why}); project commands are stopped until the controller restarts"
                    ));
                }
                self.answer(&admission, false)
            }
            Committed::Replayed(admission) => self.answer(&admission, true),
            Committed::Conflict(_) => Response::err(format!(
                "command {command_id} was used before with other arguments"
            )),
            Committed::Rejected(why) => Response::err(why),
        }
    }

    /// Commit a refused admission: the reason, no effect and one ledger line.
    fn commit_refusal(
        &self,
        ledger: &MutexGuard<'_, ProjectLedger>,
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

    /// Append one committed line to the file and mark it written. A failure stops the project
    /// commands: the line stays in the outbox, and the next start appends it.
    fn append_line(
        &self,
        ledger: &mut MutexGuard<'_, ProjectLedger>,
        line: &pf_engine::port::store::LedgerLine,
    ) -> Result<(), String> {
        let ProjectLedger { file, stopped, .. } = &mut **ledger;
        append_and_mark_line(self.store.as_ref(), file, line).inspect_err(|why| {
            eprintln!("pulse-factory: controller ledger: {why}");
            *stopped = Some(why.clone());
        })
    }

    /// The answer for a stored admission: the project it names, or its refusal.
    fn answer(&self, admission: &AdmissionRow, replayed: bool) -> Response {
        if admission.outcome == AdmissionOutcome::Refused {
            return Response::err(admission.reason.clone().unwrap_or_default());
        }
        let Some(id) = &admission.unit_id else {
            return Response::err("the admission names no project");
        };
        match self.store.project(id) {
            Ok(Some(row)) => Response::ok(
                serde_json::to_value(ProjectChanged {
                    schema_version: SCHEMA_VERSION,
                    replayed,
                    project: ProjectRecord::from(row),
                })
                .unwrap_or(Value::Null),
            ),
            Ok(None) => Response::err(format!("project {id} is not in the store")),
            Err(e) => Response::err(e.to_string()),
        }
    }
}

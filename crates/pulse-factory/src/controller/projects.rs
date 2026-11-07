//! Projects: the verbs `project_add`, `project_list` and `project_remove` (ADR-0002, ADR-0003,
//! ADR-0004, ADR-0009).
//!
//! A project is a controller entity with an ID, a name and one root. Each change is a controller
//! command that takes the path of `commands.rs`: a command ID, the admission scope `@controller`,
//! one ledger line for each new command, and a refusal that is an admission of its own.
//!
//! The argument and answer shapes are the serde types of `pf_api::types`. The network API takes
//! the same types, so a device sees the JSON that the CLI's `--json` prints.

use std::path::Path;

use pf_api::types::{
    ProjectAddArgs, ProjectChanged, ProjectList, ProjectRecord, ProjectRemoveArgs, SCHEMA_VERSION,
};
use pf_engine::port::store::{AdmissionRow, Effect, NEW_ID, NewProject, ProjectRow, Store};
use pf_ipc::Response;
use serde::Serialize;
use serde_json::Value;

use super::Controller;
use super::commands::{Plan, check_command_id, field, now};

/// A name is 1 to 64 characters.
pub const NAME_MAX_CHARS: usize = 64;

/// A project ID names a stored row, so it is short. A longer text is no ID.
const PROJECT_ID_MAX_CHARS: usize = 64;

// ------------------------------------------------------------------ the shapes

/// One stored project as every verb shows it.
fn record(p: ProjectRow) -> ProjectRecord {
    ProjectRecord {
        id: p.id,
        name: p.name,
        root: p.root,
        created_at: p.created_at,
        removed_at: p.removed_at,
    }
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

// ------------------------------------------------------------------ the commands

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
                    projects: rows.into_iter().map(record).collect(),
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
        match self.run_command(kind, command_id, content, plan) {
            Ok((admission, replayed)) => self.answer(&admission, replayed),
            Err(why) => Response::err(why),
        }
    }

    /// The answer for a stored admission: the project it names.
    fn answer(&self, admission: &AdmissionRow, replayed: bool) -> Response {
        let Some(id) = &admission.unit_id else {
            return Response::err("the admission names no project");
        };
        match self.store.project(id) {
            Ok(Some(row)) => Response::ok(
                serde_json::to_value(ProjectChanged {
                    schema_version: SCHEMA_VERSION,
                    replayed,
                    project: record(row),
                })
                .unwrap_or(Value::Null),
            ),
            Ok(None) => Response::err(format!("project {id} is not in the store")),
            Err(e) => Response::err(e.to_string()),
        }
    }
}

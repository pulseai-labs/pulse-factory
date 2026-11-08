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
    ProjectAddArgs, ProjectChanged, ProjectKind, ProjectList, ProjectRecord, ProjectRemoveArgs,
    SCHEMA_VERSION,
};
use pf_engine::port::store::{
    AdmissionRow, Effect, NEW_ID, NewProject, ProjectKind as StoredKind, ProjectRow, Store,
};
use pf_engine::status::redact;
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

/// The wire form of a stored kind.
fn wire_kind(kind: StoredKind) -> ProjectKind {
    match kind {
        StoredKind::Git => ProjectKind::Git,
        StoredKind::Folder => ProjectKind::Folder,
    }
}

/// One stored project as every verb shows it. `kind_error` is the cause that a fresh detection gave
/// for a project with no kind, and `None` for every other project.
fn record(p: ProjectRow, kind_error: Option<String>) -> ProjectRecord {
    ProjectRecord {
        id: p.id,
        name: p.name,
        root: p.root,
        created_at: p.created_at,
        removed_at: p.removed_at,
        kind: p.kind.map(wire_kind),
        remote_key: p.remote_key,
        kind_error,
    }
}

/// The content of a `project_add` admission: its arguments after the root is made canonical and
/// the name has its default. The store compares these bytes, so the field order is fixed.
#[derive(Serialize)]
struct AddContent<'a> {
    name: &'a str,
    root: &'a str,
}

/// The content of a `project_remove` admission, and of a `project_kind` admission (the backfill).
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
/// symlink, a `..`) are one root. A root that cannot be made canonical stays as it was given. This
/// looks at the path only. What lies inside the root (a git repository or a plain folder) is
/// [`Controller::detect`]'s, and `project add` asks it after this check.
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
        // The kind is detected once, here. A root that is neither a main worktree of a repository
        // nor a plain folder is refused with the cause, and a failed detection is never a folder.
        let plan = match root_ok
            .and_then(|()| check_name(&name))
            .and_then(|()| self.detect(&root))
        {
            Err(why) => Plan::Refuse(why),
            Ok(found) => Plan::Admit {
                effects: vec![Effect::AddProject(NewProject {
                    name: name.clone(),
                    root: root.clone(),
                    created_at: now(),
                    kind: found.kind,
                    remote_key: found.remote_key,
                })],
                subject: NEW_ID.to_string(),
                line: format!(
                    "project_add {NEW_ID} admitted name={} root={} kind={} command={}",
                    field(&name),
                    field(&root),
                    found.kind.as_str(),
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

    /// The live projects. A list is a read: it costs no admission, no ledger line and no store
    /// write. It reads the remote key of each `git` project again, and answers the key it read. When
    /// the read fails, or the root is not a directory, it answers the stored key. A project with no
    /// kind shows the cause that a fresh detection gives.
    pub(super) fn verb_project_list(&self) -> Response {
        match self.store.projects() {
            Ok(rows) => Response::ok(
                serde_json::to_value(ProjectList {
                    schema_version: SCHEMA_VERSION,
                    projects: rows.into_iter().map(|p| self.listed(p)).collect(),
                })
                .unwrap_or(Value::Null),
            ),
            Err(e) => Response::err(e.to_string()),
        }
    }

    fn listed(&self, mut p: ProjectRow) -> ProjectRecord {
        let mut kind_error = None;
        match p.kind {
            None => kind_error = Some(self.kind_cause(&p.root)),
            Some(StoredKind::Git) if Path::new(&p.root).is_dir() => {
                if let Ok(live) = self.read_remote_key(&p.root) {
                    p.remote_key = live;
                }
            }
            Some(_) => {}
        }
        record(p, kind_error)
    }

    /// At controller start (Approach 5): each live project with no kind gets the detection of
    /// `project add`. A project it detects gets one admitted `project_kind` command. A project it
    /// cannot detect keeps no kind and is reported in four places: a refused `project_kind`
    /// admission (which a later start replays, so its line appears once), a line on stderr at each
    /// start, `project list`, and the refusal of a chat session start. It never guesses, and each
    /// start tries again, so a cause that the operator fixes gives the kind at the next start.
    pub(super) fn backfill_project_kinds(&self) {
        if let Some(why) = self.commands_stopped() {
            eprintln!(
                "pulse-factory: the project kind backfill is skipped, because controller commands are stopped: {why}"
            );
            return;
        }
        let projects = match self.store.projects() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("pulse-factory: the projects are not read for the kind backfill: {e}");
                return;
            }
        };
        for p in projects.into_iter().filter(|p| p.kind.is_none()) {
            let content = serde_json::to_string(&RemoveContent { id: &p.id }).unwrap_or_default();
            let command_id = format!("backfill-kind:{}", p.id);
            let result = match self.detect(&p.root) {
                Ok(found) => {
                    let key = found
                        .remote_key
                        .as_deref()
                        .map_or_else(|| "none".to_string(), field);
                    let line = format!(
                        "project_kind {} admitted kind={} remote_key={key} command={command_id}",
                        p.id,
                        found.kind.as_str()
                    );
                    self.run_command(
                        "project_kind",
                        &command_id,
                        content,
                        Plan::Admit {
                            effects: vec![Effect::SetProjectKind {
                                id: p.id.clone(),
                                kind: found.kind,
                                remote_key: found.remote_key,
                            }],
                            subject: p.id.clone(),
                            line,
                        },
                    )
                }
                Err(cause) => {
                    eprintln!(
                        "pulse-factory: project {} has no kind: {}",
                        p.id,
                        redact(&cause)
                    );
                    self.run_command(
                        "project_kind",
                        &format!("{command_id}:refused"),
                        content,
                        Plan::Refuse(format!("project {}: {cause}", p.id)),
                    )
                }
            };
            // A refusal is the expected result of the second branch, and a replay of it. Any other
            // failure is the store's or the ledger's.
            if let Err(why) = result
                && !why.starts_with(&format!("project {}:", p.id))
            {
                eprintln!(
                    "pulse-factory: the kind of project {} is not set: {why}",
                    p.id
                );
            }
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
                    project: record(row, None),
                })
                .unwrap_or(Value::Null),
            ),
            Ok(None) => Response::err(format!("project {id} is not in the store")),
            Err(e) => Response::err(e.to_string()),
        }
    }
}

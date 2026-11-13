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
    AdmissionRow, CONTROLLER_SCOPE, Effect, NEW_ID, NewProject, ProjectKind as StoredKind,
    ProjectRow, RunState, Store,
};
use pf_engine::status::redact;
use pf_ipc::Response;
use serde::Serialize;
use serde_json::{Value, json};

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
    /// Only when the add asked for `git init` (`r1.s5.w5`), so the content of every add without it is
    /// byte for byte what it was.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    git_init: bool,
}

/// The content of a `project_remove` admission, and of a `project_kind` admission (the backfill).
#[derive(Serialize)]
struct RemoveContent<'a> {
    id: &'a str,
}

/// The command ID of the end of one session, for a `project_remove` with `end_sessions`: the
/// remove's own ID, then `:end:` and the session ID.
fn derived_end_id(command_id: &str, session: &str) -> String {
    format!("{command_id}:end:{session}")
}

/// The numeric part of a session ID, for session order.
fn session_number(id: &str) -> u64 {
    id.rsplit('-')
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(u64::MAX)
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

/// Git's answer for a directory that is in no repository: exit 128 and "not a git repository".
fn not_a_repository(out: &super::worktrees::GitOut) -> bool {
    out.code == Some(128)
        && out
            .stderr
            .trim_start()
            .starts_with("fatal: not a git repository")
}

/// The first line of git's stderr, redacted: the cause of a failure.
fn git_cause(out: &super::worktrees::GitOut) -> String {
    let line = out
        .stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("git failed with no message");
    redact(line)
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
        let git_init = args.git_init.unwrap_or(false);
        let (root, root_ok) = resolve_root(&args.root);
        let name = args.name.clone().unwrap_or_else(|| default_name(&root));
        let content = serde_json::to_string(&AddContent {
            name: &name,
            root: &root,
            git_init,
        })
        .unwrap_or_default();
        // `git init` runs in this call only when the command ID is new: a replay runs nothing, and
        // the store answers it with the first record.
        let mut inited = false;
        // The kind is detected once, here. A root that is neither a main worktree of a repository
        // nor a plain folder is refused with the cause, and a failed detection is never a folder.
        let checked = root_ok.and_then(|()| check_name(&name)).and_then(|()| {
            if git_init && !self.project_command_known(&args.command_id)? {
                self.check_git_init(&root, &name)?;
                self.run_git_init(&root)?;
                inited = true;
            }
            self.detect(&root)
        });
        let plan = match checked {
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
                    "project_add {NEW_ID} admitted name={} root={} kind={}{} command={}",
                    field(&name),
                    field(&root),
                    found.kind.as_str(),
                    if git_init { " git_init=true" } else { "" },
                    args.command_id
                ),
            },
        };
        match self.run_command("project_add", &args.command_id, content, plan) {
            Ok((admission, replayed)) => self.answer(&admission, replayed, None),
            // Known limit: the store can refuse the add after `git init` ran (a race with a second
            // add of the same root). The folder stays a repository, and the answer says so.
            Err(why) if inited => Response::err(format!(
                "{why}; git init already ran in the root, and the folder stays a git repository"
            )),
            Err(why) => Response::err(why),
        }
    }

    /// Does the controller know this command ID already? Then the verb is a replay or a conflict, and
    /// the store decides which.
    fn project_command_known(&self, command_id: &str) -> Result<bool, String> {
        Ok(self
            .store
            .admission(CONTROLLER_SCOPE, command_id)
            .map_err(|e| e.to_string())?
            .is_some())
    }

    /// The checks of an add with `git_init`, all before any change on disk: the root lies under the
    /// home folder, no live project has the root or the name, and the root is not a repository and
    /// is not inside one.
    fn check_git_init(&self, root: &str, name: &str) -> Result<(), String> {
        let home = self.home_folder()?;
        let path = Path::new(root);
        if path == home || !path.starts_with(&home) {
            return Err(format!("the root is not under the home folder: {root}"));
        }
        // The store checks these two at the commit. They are checked here as well, so a refused add
        // does not leave a repository behind.
        for p in self.store.projects().map_err(|e| e.to_string())? {
            if p.root == root {
                return Err(format!("a live project already has the root {root}"));
            }
            if p.name == name {
                return Err(format!("a live project already has the name {name}"));
            }
        }
        if std::fs::symlink_metadata(path.join(".git")).is_ok() {
            return Err(format!("the root is a git repository already: {root}"));
        }
        let inside = self.git(path, &["rev-parse", "--is-inside-work-tree"])?;
        if inside.code == Some(0) {
            return Err(if inside.stdout.starts_with(b"true") {
                format!("the root is inside a git repository: {root}")
            } else {
                format!("the root is inside a git directory: {root}")
            });
        }
        if !not_a_repository(&inside) {
            return Err(git_cause(&inside));
        }
        Ok(())
    }

    fn run_git_init(&self, root: &str) -> Result<(), String> {
        let out = self.git(Path::new(root), &["init", "--quiet"])?;
        if out.code == Some(0) {
            Ok(())
        } else {
            Err(git_cause(&out))
        }
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
        let end = args.end_sessions.unwrap_or(false);
        let content = serde_json::to_string(&RemoveContent { id: &args.id }).unwrap_or_default();
        // The store knows whether the ID names a live project, and its answer decides.
        let remove = Plan::Admit {
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
        // A command ID the controller knows is a replay (or a conflict): it runs no check and ends
        // no session, and the store answers it with the first record.
        match self.project_command_known(&args.command_id) {
            Ok(true) => {
                let ended = end.then(|| self.ended_by(&args.id, &args.command_id));
                return self.project_command(
                    "project_remove",
                    &args.command_id,
                    content,
                    remove,
                    ended,
                );
            }
            Ok(false) => {}
            Err(why) => return Response::err(why),
        }
        // The run check, for every caller (Planner ruling 3): a project with an active run is not
        // removed, and nothing is ended.
        let active = self.active_runs_of(&args.id);
        if !active.is_empty() {
            return self.project_command(
                "project_remove",
                &args.command_id,
                content,
                Plan::Refuse(format!(
                    "project {} has an active run {}; remove it after the run ends",
                    args.id,
                    active.join(", ")
                )),
                None,
            );
        }
        let mut ended = None;
        if end {
            let sessions = match self.live_sessions_of(&args.id) {
                Ok(s) => s,
                Err(why) => return Response::err(why),
            };
            // Every derived command ID is checked before the first end: one that breaks the rule
            // refuses the whole remove.
            for session in &sessions {
                if let Err(why) = check_command_id(&derived_end_id(&args.command_id, session)) {
                    return self.project_command(
                        "project_remove",
                        &args.command_id,
                        content,
                        Plan::Refuse(format!(
                            "the command ID is too long to end session {session}: {why}"
                        )),
                        None,
                    );
                }
            }
            let mut done = Vec::new();
            for session in sessions {
                let end_id = derived_end_id(&args.command_id, &session);
                let answer = self.verb_session_end(&json!({
                    "session_id": session,
                    "command_id": end_id,
                }));
                if !answer.ok {
                    let why = answer.error.unwrap_or_default();
                    return Response::err(format!(
                        "session {session} was not ended, and the project is not removed: {why}"
                    ));
                }
                done.push(session);
            }
            ended = Some(done);
        }
        // The store's rule stays as the last guard: a session that started in between refuses.
        self.project_command("project_remove", &args.command_id, content, remove, ended)
    }

    /// The runs of this project that are active now: each run the controller holds, as the store
    /// says it (`run_list`'s reading). A run the store cannot read is skipped.
    fn active_runs_of(&self, project_id: &str) -> Vec<String> {
        self.handles()
            .iter()
            .filter_map(|handle| {
                let row = self.store.run(&handle.info.run_id).ok().flatten()?;
                (row.state == RunState::Active && row.project_id.as_deref() == Some(project_id))
                    .then_some(row.id)
            })
            .collect()
    }

    /// The live chat sessions of a project, in session order.
    fn live_sessions_of(&self, project_id: &str) -> Result<Vec<String>, String> {
        let mut rows = self
            .store
            .chat_sessions(project_id)
            .map_err(|e| e.to_string())?;
        rows.retain(|r| r.ended_at.is_none());
        rows.sort_by_key(|r| session_number(&r.id));
        Ok(rows.into_iter().map(|r| r.id).collect())
    }

    /// The sessions a remove command ended: those of the project with an admitted end under the
    /// command's derived ID. A replay names the same ones as the first call.
    fn ended_by(&self, project_id: &str, command_id: &str) -> Vec<String> {
        let mut rows = self.store.chat_sessions(project_id).unwrap_or_default();
        rows.sort_by_key(|r| session_number(&r.id));
        rows.into_iter()
            .filter(|r| {
                self.store
                    .admission(CONTROLLER_SCOPE, &derived_end_id(command_id, &r.id))
                    .ok()
                    .flatten()
                    .is_some()
            })
            .map(|r| r.id)
            .collect()
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

    /// Commit one project command, append its ledger line, and answer. `ended` is the sessions a
    /// `project_remove` with `end_sessions` ended.
    fn project_command(
        &self,
        kind: &'static str,
        command_id: &str,
        content: String,
        plan: Plan,
        ended: Option<Vec<String>>,
    ) -> Response {
        match self.run_command(kind, command_id, content, plan) {
            Ok((admission, replayed)) => self.answer(&admission, replayed, ended),
            Err(why) => Response::err(why),
        }
    }

    /// The answer for a stored admission: the project it names.
    fn answer(
        &self,
        admission: &AdmissionRow,
        replayed: bool,
        ended: Option<Vec<String>>,
    ) -> Response {
        let Some(id) = &admission.unit_id else {
            return Response::err("the admission names no project");
        };
        match self.store.project(id) {
            Ok(Some(row)) => Response::ok(
                serde_json::to_value(ProjectChanged {
                    schema_version: SCHEMA_VERSION,
                    replayed,
                    project: record(row, None),
                    ended,
                })
                .unwrap_or(Value::Null),
            ),
            Ok(None) => Response::err(format!("project {id} is not in the store")),
            Err(e) => Response::err(e.to_string()),
        }
    }
}

//! `folder_list` (`r1.s5.w5`, O4): the folders inside the server's home folder, for the app's Add a
//! project picker.
//!
//! It is a read, like `project_list`: it costs no admission, writes no `controller.ledger` line and
//! touches no row. A device's call gets its one `api.ledger` line from the router. The verb is
//! confined to the home folder (`Config::home`): the path is made canonical first, so a `..` and a
//! symlink are resolved before the check, and a canonical path that is not the home folder or under
//! it is refused with no listing.
//!
//! **No file is opened.** The verb reads directory entries and metadata only (`read_dir`,
//! `symlink_metadata`, `canonicalize`), and git's own reads of `.git` for a folder that has one. A
//! file, a FIFO, a socket and a broken link are skipped, so none can stop or slow a listing.

use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use pf_api::types::{FolderEntry, FolderList, FolderListArgs, SCHEMA_VERSION};
use pf_engine::port::store::Store;
use pf_ipc::Response;
use serde_json::Value;

use super::Controller;

/// A listing holds at most this many entries. `truncated` is true when there were more.
pub const ENTRIES_MAX: usize = 500;

const NO_HOME: &str = "the server has no home folder";
const OUTSIDE: &str = "the path is outside the home folder";

/// The path with its `.` and `..` components resolved by text, with no file system call. It only
/// decides which refusal a path that cannot be made canonical gets.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The text of a path, or the refusal for one that is not valid UTF-8.
fn text(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| "a path in the listing is not valid UTF-8".to_string())
}

/// The canonical directory that `given` names, when it is the home folder or lies under it.
///
/// A path that is not inside the home folder gets one answer, whether it exists or not, so the verb
/// does not tell a caller which paths outside the home folder exist. One limit: a path that is
/// lexically inside the home folder but does not resolve (a dangling symlink, say) answers that it
/// does not exist, so a symlink that already sits inside the home folder shows whether its target
/// resolves. The verb cannot make such a link; it only reads.
fn inside_home(home: &Path, given: &str) -> Result<PathBuf, String> {
    let path = Path::new(given);
    if !path.is_absolute() {
        return Err(format!("the path is not an absolute path: {given}"));
    }
    let canonical = match fs::canonicalize(path) {
        Ok(c) => c,
        Err(e) => {
            if !lexical(path).starts_with(home) {
                return Err(OUTSIDE.to_string());
            }
            return Err(match e.kind() {
                std::io::ErrorKind::NotFound => format!("the path does not exist: {given}"),
                _ => format!("the path cannot be read: {given}: {e}"),
            });
        }
    };
    if !canonical.starts_with(home) {
        return Err(OUTSIDE.to_string());
    }
    if !canonical.is_dir() {
        return Err(format!("the path is not a directory: {given}"));
    }
    Ok(canonical)
}

/// A branch name that is safe to show: one line, no control character.
fn plain_branch(out: &[u8]) -> Option<String> {
    let name = String::from_utf8_lossy(out).trim().to_string();
    if name.is_empty() || name.chars().any(char::is_control) {
        return None;
    }
    Some(name)
}

impl Controller {
    /// The home folder, made canonical now, or the refusal.
    pub(super) fn home_folder(&self) -> Result<PathBuf, String> {
        let Some(home) = &self.cfg.home else {
            return Err(NO_HOME.to_string());
        };
        match fs::canonicalize(home) {
            Ok(canonical) if canonical.is_dir() => Ok(canonical),
            _ => Err(NO_HOME.to_string()),
        }
    }

    pub(super) fn verb_folder_list(&self, args: &Value) -> Response {
        let args: FolderListArgs = match serde_json::from_value(args.clone()) {
            Ok(a) => a,
            Err(e) => return Response::err(format!("the arguments are not readable: {e}")),
        };
        let home = match self.home_folder() {
            Ok(h) => h,
            Err(why) => return Response::err(why),
        };
        let listed = match args.path.as_deref() {
            None => home.clone(),
            Some(given) => match inside_home(&home, given) {
                Ok(p) => p,
                Err(why) => return Response::err(why),
            },
        };
        match self.list_folders(&home, &listed) {
            Ok(list) => Response::ok(serde_json::to_value(list).unwrap_or(Value::Null)),
            Err(why) => Response::err(why),
        }
    }

    fn list_folders(&self, home: &Path, listed: &Path) -> Result<FolderList, String> {
        let read = fs::read_dir(listed)
            .map_err(|e| format!("the folder cannot be read: {}: {e}", listed.display()))?;
        // The folders to show, with the canonical path of each: a directory is itself, and a
        // symlink is its target, when the target is a directory under the home folder.
        let mut found: Vec<(String, PathBuf)> = Vec::new();
        for entry in read.flatten() {
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            let target = if meta.is_dir() {
                path
            } else if meta.file_type().is_symlink() {
                match fs::canonicalize(&path) {
                    Ok(t) if t.is_dir() && t.starts_with(home) => t,
                    _ => continue,
                }
            } else {
                continue;
            };
            found.push((name.to_string(), target));
        }
        found.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        let truncated = found.len() > ENTRIES_MAX;
        found.truncate(ENTRIES_MAX);

        // The live projects, by the canonical root each was added under.
        let projects: HashMap<String, String> = self
            .store
            .projects()
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|p| (p.root, p.id))
            .collect();

        let mut entries = Vec::with_capacity(found.len());
        for (name, target) in found {
            let Ok(path) = text(&target) else {
                continue;
            };
            let has_git = fs::symlink_metadata(target.join(".git")).is_ok();
            let branch = if has_git {
                self.git(&target, &["symbolic-ref", "--short", "-q", "HEAD"])
                    .ok()
                    .filter(|out| out.code == Some(0))
                    .and_then(|out| plain_branch(&out.stdout))
            } else {
                None
            };
            entries.push(FolderEntry {
                project_id: projects.get(&path).cloned(),
                name,
                path,
                git: has_git,
                branch,
            });
        }

        // The repository that holds the listed folder, once for the call.
        let repo = self
            .git(listed, &["rev-parse", "--show-toplevel"])
            .ok()
            .filter(|out| out.code == Some(0))
            .and_then(|out| plain_branch(&out.stdout));
        let parent = if listed == home {
            None
        } else {
            listed.parent().and_then(|p| text(p).ok())
        };
        Ok(FolderList {
            schema_version: SCHEMA_VERSION,
            home: text(home)?,
            path: text(listed)?,
            parent,
            repo,
            entries,
            truncated,
        })
    }
}

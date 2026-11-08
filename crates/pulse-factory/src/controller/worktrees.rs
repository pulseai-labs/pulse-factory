//! What git and the file system say about a project and its worktrees (ADR-0003, ADR-0015).
//!
//! - **The git runner.** Every git call is a subprocess with a 10-second timeout. It runs with each
//!   `GIT_*` variable of the controller's environment removed, then `LC_ALL=C`,
//!   `GIT_TERMINAL_PROMPT=0` and `GIT_OPTIONAL_LOCKS=0` set. A git that cannot start, or a timeout, is
//!   an error that names the cause. A failed detection never becomes `folder`.
//! - **The kind** of a root (`project add` and the backfill at start): a main worktree of a git
//!   repository is `git`; a plain folder, and a subdirectory of a repository, are `folder`; every
//!   other answer is a refusal that names the cause.
//! - **The remote key** of a `git` project: `host/path` of its remote, with no user, password or port.
//!   The URL itself is never stored, never printed and never in a ledger line, because it can hold
//!   a token.
//! - **The live check** of a worktree (the admission of a chat session start): the file system and
//!   git at that moment, never a stored mark.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use pf_engine::port::store::{ProjectKind, ProjectRow, Store, WorktreeRow};
use pf_engine::status::redact;

use super::Controller;
use super::commands::now;

/// How long one git call may run: 10 seconds.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(10);

/// What one git call gave.
#[derive(Debug)]
pub struct GitOut {
    /// `None` when a signal ended the process.
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

impl GitOut {
    fn ok(&self) -> bool {
        self.code == Some(0)
    }

    fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    /// The first line of stderr, redacted: the cause of a failure.
    fn cause(&self) -> String {
        let line = self
            .stderr
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("git failed with no message");
        redact(line)
    }

    fn not_a_repository(&self) -> bool {
        self.code == Some(128)
            && self
                .stderr
                .trim_start()
                .starts_with("fatal: not a git repository")
    }

    fn dubious_ownership(&self) -> bool {
        self.stderr.contains("detected dubious ownership")
    }
}

/// What detection found for a root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detected {
    pub kind: ProjectKind,
    /// The canonical remote key of a `git` project that has one.
    pub remote_key: Option<String>,
}

/// One entry of `git worktree list --porcelain -z`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedWorktree {
    pub path: String,
    /// The branch name, without `refs/heads/`.
    pub branch: Option<String>,
    pub detached: bool,
    pub bare: bool,
    /// Git lists the entry, and its directory is gone.
    pub prunable: bool,
}

/// What the live check of a worktree found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorktreeLive {
    /// The worktree is there. `branch` is `None` for a folder and for a detached HEAD.
    Live {
        branch: Option<String>,
        detached: bool,
    },
    /// The worktree is gone, and this is why.
    Gone(String),
}

/// Parse the output of `git worktree list --porcelain -z`: NUL-terminated fields, and one more NUL
/// after the last field of each entry.
pub fn parse_worktree_list(out: &[u8]) -> Vec<ListedWorktree> {
    let mut entries: Vec<ListedWorktree> = Vec::new();
    for field in out.split(|b| *b == 0) {
        let field = String::from_utf8_lossy(field);
        if let Some(path) = field.strip_prefix("worktree ") {
            entries.push(ListedWorktree {
                path: path.to_string(),
                branch: None,
                detached: false,
                bare: false,
                prunable: false,
            });
            continue;
        }
        let Some(current) = entries.last_mut() else {
            continue;
        };
        if let Some(branch) = field.strip_prefix("branch ") {
            current.branch = Some(
                branch
                    .strip_prefix("refs/heads/")
                    .unwrap_or(branch)
                    .to_string(),
            );
        } else if field == "detached" {
            current.detached = true;
        } else if field == "bare" {
            current.bare = true;
        } else if field == "prunable" || field.starts_with("prunable ") {
            current.prunable = true;
        }
    }
    entries
}

/// The canonical form of a path, or the path as it was when it cannot be made canonical.
fn canonical(path: &str) -> String {
    std::fs::canonicalize(path)
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| path.to_string())
}

/// The canonical key `host/path` of a remote URL, or `None`.
///
/// It takes the host and path of `https://`, `http://`, `ssh://` and `git://` URLs and of the scp
/// form `user@host:path`. It drops the user, the password and the port, lowercases the host, and
/// drops one trailing `/` and one trailing `.git` of the path. A local path, a `file://` URL, a URL
/// with no path and any other scheme give no key.
pub fn remote_key_of(url: &str) -> Option<String> {
    let url = url.trim();
    let (host, path) = match url.split_once("://") {
        Some((scheme, rest)) => {
            if !matches!(scheme, "https" | "http" | "ssh" | "git") {
                return None;
            }
            let (authority, path) = rest.split_once('/')?;
            // The user and the password come before the last `@`.
            let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
            (strip_port(host_port), path)
        }
        None => {
            // The scp form: `[user@]host:path`, and the part before the colon holds no `/`.
            let (before, path) = url.split_once(':')?;
            if before.is_empty() || before.contains('/') {
                return None;
            }
            let host = before.rsplit_once('@').map_or(before, |(_, h)| h);
            (host, path)
        }
    };
    let host = host.to_ascii_lowercase();
    let path = path.trim_start_matches('/');
    let path = path.strip_suffix('/').unwrap_or(path);
    let path = path.strip_suffix(".git").unwrap_or(path);
    if host.is_empty() || path.is_empty() {
        return None;
    }
    let key = format!("{host}/{path}");
    // A key goes into ledger lines and answers: no space and no control character.
    if key.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    Some(key)
}

/// A host without its port. A bracketed IPv6 address keeps its brackets.
fn strip_port(host_port: &str) -> &str {
    if host_port.starts_with('[') {
        return match host_port.find(']') {
            Some(end) => &host_port[..=end],
            None => host_port,
        };
    }
    match host_port.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => host,
        _ => host_port,
    }
}

impl Controller {
    /// Run git in `dir` with the controller's rules for the environment and the time.
    pub fn git(&self, dir: &Path, args: &[&str]) -> Result<GitOut, String> {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(dir)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // The controller's own GIT_* variables (a GIT_DIR, say) must not steer the calls.
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("GIT_") {
                cmd.env_remove(&name);
            }
        }
        cmd.env("LC_ALL", "C")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0");
        // The test seam adds variables after the strip.
        for (name, value) in &self.cfg.git_env {
            cmd.env(name, value);
        }
        let mut child = cmd.spawn().map_err(|e| format!("git cannot start: {e}"))?;
        let mut stdout = child.stdout.take();
        let mut stderr = child.stderr.take();
        let read_out = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(s) = stdout.as_mut() {
                let _ = s.read_to_end(&mut bytes);
            }
            bytes
        });
        let read_err = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(s) = stderr.as_mut() {
                let _ = s.read_to_end(&mut bytes);
            }
            bytes
        });
        let deadline = Instant::now() + GIT_TIMEOUT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "git did not answer within {} seconds",
                        GIT_TIMEOUT.as_secs()
                    ));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                Err(e) => return Err(format!("git cannot be waited for: {e}")),
            }
        };
        Ok(GitOut {
            code: status.code(),
            stdout: read_out.join().unwrap_or_default(),
            stderr: String::from_utf8_lossy(&read_err.join().unwrap_or_default()).into_owned(),
        })
    }

    /// The worktrees git lists for the repository that holds `dir`.
    fn list_worktrees(&self, dir: &Path) -> Result<GitListing, String> {
        let out = self.git(dir, &["worktree", "list", "--porcelain", "-z"])?;
        if out.ok() {
            return Ok(GitListing::Listed(parse_worktree_list(&out.stdout)));
        }
        if out.not_a_repository() {
            return Ok(GitListing::NotARepository);
        }
        if out.dubious_ownership() {
            return Err(format!(
                "the root is in a repository that git refuses for dubious ownership: {}",
                dir.display()
            ));
        }
        Err(out.cause())
    }

    /// Decide the kind of a root, and read the remote key of a `git` root. The root has been made
    /// canonical and exists. The answer is `Err` with the cause for a root that is refused.
    pub fn detect(&self, root: &str) -> Result<Detected, String> {
        let root_path = Path::new(root);
        let first = self.git(
            root_path,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--is-bare-repository",
                "--is-inside-work-tree",
                "--git-common-dir",
            ],
        )?;
        if first.not_a_repository() {
            return Ok(Detected {
                kind: ProjectKind::Folder,
                remote_key: None,
            });
        }
        if first.dubious_ownership() {
            return Err(format!(
                "the root is in a repository that git refuses for dubious ownership: {root}"
            ));
        }
        if !first.ok() {
            return Err(first.cause());
        }
        let text = first.stdout_text();
        let mut lines = text.lines();
        let bare = lines.next().unwrap_or_default().trim();
        let inside = lines.next().unwrap_or_default().trim();
        let common_dir = lines.next().unwrap_or_default().trim().to_string();
        if bare == "true" {
            return Err("the root is a bare repository".to_string());
        }
        if inside == "false" {
            return Err("the root is inside a git directory".to_string());
        }
        let top = self.git(
            root_path,
            &["rev-parse", "--path-format=absolute", "--show-toplevel"],
        )?;
        if !top.ok() {
            return Err(top.cause());
        }
        let top = canonical(top.stdout_text().trim());
        let root_canonical = canonical(root);
        if top != root_canonical && Path::new(&root_canonical).starts_with(&top) {
            // A subdirectory of a repository: its git operations would reach outside the root.
            return Ok(Detected {
                kind: ProjectKind::Folder,
                remote_key: None,
            });
        }
        let dot_git = canonical(&format!("{}/.git", root_canonical.trim_end_matches('/')));
        if top == root_canonical && canonical(&common_dir) == dot_git {
            return Ok(Detected {
                kind: ProjectKind::Git,
                remote_key: self.read_remote_key(root).unwrap_or(None),
            });
        }
        // Any other answer is refused. The first entry of the worktree list is the main worktree.
        let common = canonical(&common_dir);
        let main = match self.list_worktrees(root_path)? {
            // The entry of a git directory that holds no work tree (a bare entry, or the common
            // directory itself, as `--separate-git-dir` lists it) is no main worktree to add.
            GitListing::Listed(entries) => entries
                .into_iter()
                .next()
                .map(|e| canonical(&e.path))
                .filter(|path| *path != common),
            GitListing::NotARepository => None,
        };
        match main {
            Some(main) if main != root_canonical => Err(format!(
                "the root is a linked worktree; add its main worktree instead: {main}"
            )),
            _ => Err(format!(
                "the root is not a main worktree: its git directory is {common_dir}"
            )),
        }
    }

    /// The remote key of a `git` root, read now. `Ok(None)` when the repository has no remote that
    /// gives a key. `Err` when git failed: the caller keeps the key it had.
    pub fn read_remote_key(&self, root: &str) -> Result<Option<String>, String> {
        let dir = Path::new(root);
        let names = self.git(dir, &["remote"])?;
        if !names.ok() {
            return Err(names.cause());
        }
        let names = names.stdout_text();
        let names: Vec<&str> = names
            .lines()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .collect();
        let name = if names.contains(&"origin") {
            "origin"
        } else if names.len() == 1 {
            names[0]
        } else {
            return Ok(None);
        };
        let url = self.git(dir, &["remote", "get-url", name])?;
        if !url.ok() {
            return Err(url.cause());
        }
        Ok(remote_key_of(url.stdout_text().trim()))
    }

    /// Why a project has no kind, from a fresh detection: the cause of a refusal, or a note that the
    /// kind is set at the next start when the detection now succeeds.
    pub fn kind_cause(&self, root: &str) -> String {
        if !Path::new(root).is_dir() {
            return format!("the root does not exist: {root}");
        }
        match self.detect(root) {
            Err(cause) => cause,
            Ok(d) => format!(
                "git now reports a {} project; the controller sets the kind at its next start",
                d.kind.as_str()
            ),
        }
    }

    /// The live check of a worktree: the file system and, for a `git` project, git at this moment.
    /// The first read that finds a recorded row gone marks it gone, for display. The admission never
    /// decides on the mark. A git failure other than "not a repository" is an `Err`, and marks
    /// nothing.
    pub fn check_worktree(
        &self,
        project: &ProjectRow,
        path: &str,
        row: Option<&WorktreeRow>,
    ) -> Result<WorktreeLive, String> {
        let found = self.look_at_worktree(project, path)?;
        if let (WorktreeLive::Gone(_), Some(row)) = (&found, row)
            && row.gone_at.is_none()
            && row.removed_at.is_none()
        {
            let _ = self.store.mark_worktree_gone(&row.id, &now());
        }
        Ok(found)
    }

    fn look_at_worktree(&self, project: &ProjectRow, path: &str) -> Result<WorktreeLive, String> {
        let Some(kind) = project.kind else {
            return Err(format!("project {} has no kind", project.id));
        };
        if !Path::new(path).is_dir() {
            return Ok(WorktreeLive::Gone(
                "the path is not a directory".to_string(),
            ));
        }
        if kind == ProjectKind::Folder {
            return Ok(WorktreeLive::Live {
                branch: None,
                detached: false,
            });
        }
        let root = PathBuf::from(&project.root);
        if !root.is_dir() {
            return Ok(WorktreeLive::Gone(
                "the project root is not a directory".to_string(),
            ));
        }
        let wanted = canonical(path);
        match self.list_worktrees(&root)? {
            GitListing::NotARepository => Ok(WorktreeLive::Gone(
                "git reports that the project root is not a repository".to_string(),
            )),
            GitListing::Listed(entries) => {
                match entries
                    .into_iter()
                    .find(|e| !e.prunable && canonical(&e.path) == wanted)
                {
                    Some(e) => Ok(WorktreeLive::Live {
                        branch: e.branch,
                        detached: e.detached,
                    }),
                    None => Ok(WorktreeLive::Gone(
                        "git does not list it as a worktree".to_string(),
                    )),
                }
            }
        }
    }
}

/// What `git worktree list` answered.
enum GitListing {
    Listed(Vec<ListedWorktree>),
    NotARepository,
}

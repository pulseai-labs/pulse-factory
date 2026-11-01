//! `pf-forge-bare`: the forge port over a bare git repo in a local directory.
//!
//! It is the paper env for `pf-forge-github`, and not a fake: real git runs, and the forge
//! answers the same port with the same outcomes. A directory holds the bare repo and, beside it,
//! the PRs:
//!
//! ```text
//! <dir>/repo.git          the bare repo (any name)
//! <dir>/prs/<n>.json     {"head_branch": "...", "base": "...", "checks": "green|red|pending"}
//! <dir>/scratch/         one clone per merge, removed when the merge ends
//! ```
//!
//! `merge` makes a `--no-ff` merge of the pinned head in a scratch clone and pushes it to the
//! bare base. Nothing here deletes a branch, and nothing reaches the network. Every git call is
//! an argv list with no shell, away from the host's git config.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use pf_engine::port::forge::{
    Checks, ForgeEffect, ForgeError, ForgeOutcome, ForgePort, Mergeable, PrRef, PrStatus,
};
use pf_engine::status::{one_line_reason, redact};
use serde::{Deserialize, Serialize};

/// The checks a PR file reports. A test sets them to drive green, red and pending.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrChecks {
    Green,
    Red,
    Pending,
}

/// The content of `prs/<n>.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PrFile {
    head_branch: String,
    base: String,
    checks: PrChecks,
}

/// The most bytes of a PR file the forge reads.
const MAX_PR_FILE: u64 = 64 * 1024;

static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

/// A forge over one bare repo. Its repo name is the path of the bare repo.
#[derive(Clone, Debug)]
pub struct BareForge {
    repo: String,
    bare: PathBuf,
    prs: PathBuf,
    scratch: PathBuf,
}

fn branch_ok(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(['-', '/', '.'])
        && !name.ends_with(['/', '.'])
        && !name.contains("..")
        && !name.contains("//")
        && !name.contains("@{")
        && !name.ends_with(".lock")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
}

fn sha_ok(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

fn err(why: impl std::fmt::Display) -> ForgeError {
    ForgeError(one_line_reason(&why.to_string()))
}

/// Run git in `dir`, away from the host's git config and with no prompt.
fn git(dir: &Path, args: &[&str]) -> Result<Output, ForgeError> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=pf-forge-bare",
            "-c",
            "user.email=pf-forge-bare@localhost",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| err(format!("cannot start git: {e}")))
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_string()
}

/// Run git and require exit 0. The error holds stderr, redacted.
fn git_ok(dir: &Path, args: &[&str]) -> Result<String, ForgeError> {
    let out = git(dir, args)?;
    if out.status.success() {
        Ok(text(&out.stdout))
    } else {
        Err(err(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or(""),
            redact(&text(&out.stderr))
        )))
    }
}

/// A scratch clone. The directory goes away when this value does.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl BareForge {
    /// A forge over the bare repo at `bare`. PRs live in `prs/` beside it.
    pub fn open(bare: impl AsRef<Path>) -> Result<BareForge, ForgeError> {
        let bare = std::fs::canonicalize(bare.as_ref())
            .map_err(|e| err(format!("bare repo {}: {e}", bare.as_ref().display())))?;
        let is_bare = git_ok(&bare, &["rev-parse", "--is-bare-repository"])?;
        if is_bare != "true" {
            return Err(err(format!("{} is not a bare git repo", bare.display())));
        }
        let parent = bare
            .parent()
            .ok_or_else(|| err("the bare repo has no parent directory"))?
            .to_path_buf();
        Ok(BareForge {
            repo: bare.display().to_string(),
            prs: parent.join("prs"),
            scratch: parent.join("scratch"),
            bare,
        })
    }

    /// The repo name a PR must carry: the path of the bare repo.
    pub fn repo(&self) -> &str {
        &self.repo
    }

    /// Write or replace PR `number`, for a test or for the paper env.
    pub fn write_pr(
        &self,
        number: u64,
        head_branch: &str,
        base: &str,
        checks: PrChecks,
    ) -> Result<(), ForgeError> {
        let file = PrFile {
            head_branch: head_branch.to_string(),
            base: base.to_string(),
            checks,
        };
        std::fs::create_dir_all(&self.prs).map_err(err)?;
        let body = serde_json::to_string(&file).map_err(err)?;
        std::fs::write(self.pr_path(number), body).map_err(err)
    }

    fn pr_path(&self, number: u64) -> PathBuf {
        self.prs.join(format!("{number}.json"))
    }

    fn check_repo(&self, pr: &PrRef) -> Result<(), ForgeError> {
        if pr.repo == self.repo {
            Ok(())
        } else {
            Err(err(format!(
                "PR repo {} is not the repo of this forge, {}",
                pr.repo, self.repo
            )))
        }
    }

    fn read_pr(&self, number: u64) -> Result<PrFile, ForgeError> {
        use std::io::Read;
        let path = self.pr_path(number);
        let mut body = String::new();
        std::fs::File::open(&path)
            .and_then(|f| f.take(MAX_PR_FILE).read_to_string(&mut body))
            .map_err(|e| err(format!("PR {number}: cannot read {}: {e}", path.display())))?;
        let file: PrFile = serde_json::from_str(&body)
            .map_err(|e| err(format!("PR {number}: unreadable PR file: {e}")))?;
        for name in [&file.head_branch, &file.base] {
            if !branch_ok(name) {
                return Err(err(format!(
                    "PR {number}: {name:?} is not a plain branch name"
                )));
            }
        }
        Ok(file)
    }

    /// The tip of a branch of the bare repo.
    fn tip(&self, branch: &str) -> Result<String, ForgeError> {
        let sha = git_ok(
            &self.bare,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}^{{commit}}"),
            ],
        )
        .map_err(|_| err(format!("the bare repo has no branch {branch}")))?;
        if sha_ok(&sha) {
            Ok(sha)
        } else {
            Err(err(format!("branch {branch} gave no object name")))
        }
    }

    fn effect(&self, pr: &PrRef, branch: &str, sha: &str, outcome: ForgeOutcome) -> ForgeEffect {
        ForgeEffect {
            repo: pr.repo.clone(),
            branch: branch.to_string(),
            sha: sha.to_string(),
            outcome,
        }
    }
}

impl ForgePort for BareForge {
    fn pr_status(&self, pr: &PrRef) -> Result<PrStatus, ForgeError> {
        self.check_repo(pr)?;
        let file = self.read_pr(pr.number)?;
        let sha = self.tip(&file.head_branch)?;
        self.tip(&file.base)?;
        // `merge-tree` merges in memory: exit 0 is clean, exit 1 is a conflict.
        let merge = git(
            &self.bare,
            &[
                "merge-tree",
                "--write-tree",
                "--quiet",
                &format!("refs/heads/{}", file.base),
                &format!("refs/heads/{}", file.head_branch),
            ],
        )?;
        let mergeable = match merge.status.code() {
            Some(0) => Mergeable::Clean,
            Some(1) => Mergeable::Conflict,
            _ => {
                return Err(err(format!(
                    "git merge-tree failed: {}",
                    redact(&text(&merge.stderr))
                )));
            }
        };
        Ok(PrStatus {
            repo: pr.repo.clone(),
            branch: file.head_branch,
            sha,
            base: file.base,
            checks: match file.checks {
                PrChecks::Green => Checks::Green,
                PrChecks::Red => Checks::Red,
                PrChecks::Pending => Checks::Pending,
            },
            mergeable,
        })
    }

    fn merge(&self, pr: &PrRef, expected_sha: &str) -> Result<ForgeEffect, ForgeError> {
        if self.check_repo(pr).is_err() {
            return Ok(self.effect(
                pr,
                "",
                expected_sha,
                ForgeOutcome::Refused(one_line_reason(&format!(
                    "PR repo {} is not the repo of this forge, {}",
                    pr.repo, self.repo
                ))),
            ));
        }
        if !sha_ok(expected_sha) {
            return Err(err(format!(
                "the expected head {expected_sha:?} is not a full object name"
            )));
        }
        let file = self.read_pr(pr.number)?;
        let head = self.tip(&file.head_branch)?;
        self.tip(&file.base)?;
        if !head.eq_ignore_ascii_case(expected_sha) {
            return Ok(self.effect(
                pr,
                &file.head_branch,
                &head,
                ForgeOutcome::Refused(format!(
                    "the head moved: expected {expected_sha}, found {head}"
                )),
            ));
        }

        let scratch = Scratch(self.scratch.join(format!(
            "merge-{}-{}-{}",
            pr.number,
            std::process::id(),
            SCRATCH_SEQ.fetch_add(1, Ordering::SeqCst)
        )));
        std::fs::create_dir_all(&self.scratch).map_err(err)?;
        let dir = scratch.0.as_path();
        let source = self.bare.display().to_string();
        let target = dir.display().to_string();
        git_ok(
            &self.scratch,
            &[
                "clone",
                "--quiet",
                "--no-tags",
                "--branch",
                &file.base,
                "--",
                &source,
                &target,
            ],
        )?;

        // Merge the pinned SHA, not the branch name, so a push after the check cannot slip in.
        let message = format!(
            "Merge pull request #{} from {}",
            pr.number, file.head_branch
        );
        let merged = git(
            dir,
            &[
                "merge",
                "--no-ff",
                "--no-edit",
                "-m",
                &message,
                &head.to_ascii_lowercase(),
            ],
        )?;
        if !merged.status.success() {
            // A merge that stopped on a conflict leaves MERGE_HEAD behind.
            let in_progress = git(dir, &["rev-parse", "--verify", "--quiet", "MERGE_HEAD"])?;
            if in_progress.status.success() {
                return Ok(self.effect(pr, &file.head_branch, &head, ForgeOutcome::Conflict));
            }
            return Err(err(format!(
                "git merge failed: {}",
                redact(&text(&merged.stderr))
            )));
        }
        // No force: a base that moved since the clone makes the push fail, and the call errors.
        git_ok(
            dir,
            &[
                "push",
                "--quiet",
                "origin",
                &format!("HEAD:refs/heads/{}", file.base),
            ],
        )?;
        Ok(self.effect(pr, &file.head_branch, &head, ForgeOutcome::Merged))
    }

    fn merge_landed(&self, pr: &PrRef) -> Result<ForgeEffect, ForgeError> {
        self.check_repo(pr)?;
        let file = self.read_pr(pr.number)?;
        let head = self.tip(&file.head_branch)?;
        self.tip(&file.base)?;
        // Landed: the PR head is an ancestor of the head of the base.
        let probe = git(
            &self.bare,
            &[
                "merge-base",
                "--is-ancestor",
                &head,
                &format!("refs/heads/{}", file.base),
            ],
        )?;
        let outcome = match probe.status.code() {
            Some(0) => ForgeOutcome::Landed,
            Some(1) => ForgeOutcome::NotLanded,
            _ => {
                return Err(err(format!(
                    "git merge-base failed: {}",
                    redact(&text(&probe.stderr))
                )));
            }
        };
        Ok(self.effect(pr, &file.head_branch, &head, outcome))
    }
}

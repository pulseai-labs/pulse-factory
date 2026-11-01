//! `pf-forge-github`: the forge port over the `gh` CLI.
//!
//! `gh` runs as an argv list with no shell, and each call has a time limit. Everything `gh`
//! prints is untrusted: it is parsed as strict JSON, and a malformed answer is a [`ForgeError`].
//! The merge pins the head SHA the engine read, and it never passes `--admin`, `--auto`,
//! `--squash`, `--rebase` or `--delete-branch`.

mod parse;
mod run;

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use pf_engine::port::forge::{ForgeEffect, ForgeError, ForgeOutcome, ForgePort, PrRef, PrStatus};
use pf_engine::status::{one_line_reason, redact};

use run::{Output, RunError};

/// The limit on each `gh` call.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// A forge for one GitHub repo, `owner/name`.
#[derive(Clone, Debug)]
pub struct GithubForge {
    repo: String,
    program: OsString,
    path_prefix: Option<PathBuf>,
    timeout: Duration,
}

impl GithubForge {
    /// A forge for `repo`, in the form `owner/name`. Any other form is an error: the name goes
    /// into `gh` arguments and API paths.
    pub fn new(repo: &str) -> Result<GithubForge, ForgeError> {
        if !parse::is_repo_name(repo) {
            return Err(ForgeError(format!(
                "{repo:?} is not a repo name in the form owner/name"
            )));
        }
        Ok(GithubForge {
            repo: repo.to_string(),
            program: OsString::from("gh"),
            path_prefix: None,
            timeout: DEFAULT_TIMEOUT,
        })
    }

    /// Put `dir` first on the `PATH` of each `gh` child process. Tests use it for a stub `gh`.
    /// The `PATH` of this process stays as it is.
    pub fn with_path_prefix(mut self, dir: impl Into<PathBuf>) -> GithubForge {
        self.path_prefix = Some(dir.into());
        self
    }

    /// Run this program instead of `gh`. A name with no `/` is searched on the `PATH`.
    pub fn with_program(mut self, program: impl Into<OsString>) -> GithubForge {
        self.program = program.into();
        self
    }

    /// Change the limit on each `gh` call. The default is [`DEFAULT_TIMEOUT`].
    pub fn with_timeout(mut self, timeout: Duration) -> GithubForge {
        self.timeout = timeout;
        self
    }

    pub fn repo(&self) -> &str {
        &self.repo
    }

    fn check_repo(&self, pr: &PrRef) -> Result<(), ForgeError> {
        if pr.repo == self.repo {
            Ok(())
        } else {
            Err(ForgeError(one_line_reason(&format!(
                "PR repo {} is not the repo of this forge, {}",
                pr.repo, self.repo
            ))))
        }
    }

    /// Run `gh` once. A timeout and a start failure are forge errors.
    fn gh(&self, args: &[String]) -> Result<Output, ForgeError> {
        run::run(
            &self.program,
            args,
            self.path_prefix.as_deref(),
            self.timeout,
        )
        .map_err(|e| match e {
            RunError::Spawn(why) => ForgeError(redact(&why)),
            RunError::Timeout => ForgeError(format!(
                "gh {} timed out after {} s",
                args.first().map_or("", String::as_str),
                self.timeout.as_secs()
            )),
        })
    }

    /// Run `gh` and require exit 0. Any other exit is a forge error with stderr, redacted.
    fn gh_ok(&self, args: &[String]) -> Result<Output, ForgeError> {
        let out = self.gh(args)?;
        if out.code == Some(0) {
            Ok(out)
        } else {
            Err(failure(&out))
        }
    }
}

/// The forge error for a `gh` call that exited non-zero.
fn failure(out: &Output) -> ForgeError {
    let code = out
        .code
        .map_or("a signal".to_string(), |c| format!("exit {c}"));
    ForgeError(redact(&format!(
        "gh failed ({code}): {}",
        String::from_utf8_lossy(&out.stderr).trim()
    )))
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_string()).collect()
}

impl ForgePort for GithubForge {
    fn pr_status(&self, pr: &PrRef) -> Result<PrStatus, ForgeError> {
        self.check_repo(pr)?;
        let out = self.gh_ok(&args(&[
            "pr",
            "view",
            &pr.number.to_string(),
            "-R",
            &self.repo,
            "--json",
            parse::STATUS_FIELDS,
        ]))?;
        parse::pr_status(&self.repo, &out.stdout)
    }

    fn merge(&self, pr: &PrRef, expected_sha: &str) -> Result<ForgeEffect, ForgeError> {
        let effect = |outcome| ForgeEffect {
            repo: pr.repo.clone(),
            // The merge call does not read the PR, so it does not know the branch.
            branch: String::new(),
            sha: expected_sha.to_string(),
            outcome,
        };
        if self.check_repo(pr).is_err() {
            return Ok(effect(ForgeOutcome::Refused(one_line_reason(&format!(
                "PR repo {} is not the repo of this forge, {}",
                pr.repo, self.repo
            )))));
        }
        if !parse::is_sha(expected_sha) {
            return Err(ForgeError(one_line_reason(&format!(
                "the expected head {expected_sha:?} is not a full object name"
            ))));
        }
        // `--match-head-commit` makes GitHub merge this head or nothing. Nothing retries.
        let out = self.gh(&args(&[
            "pr",
            "merge",
            &pr.number.to_string(),
            "-R",
            &self.repo,
            "--merge",
            "--match-head-commit",
            expected_sha,
        ]))?;
        if out.code == Some(0) {
            return Ok(effect(ForgeOutcome::Merged));
        }
        // Only these words of stderr show that the head moved. Any other wording is an error.
        let stderr = String::from_utf8_lossy(&out.stderr).to_lowercase();
        if stderr.contains("head branch was modified") || stderr.contains("expected head") {
            return Ok(effect(ForgeOutcome::Refused(one_line_reason(&format!(
                "the head is no longer {expected_sha}: {}",
                String::from_utf8_lossy(&out.stderr)
            )))));
        }
        Err(failure(&out))
    }

    fn merge_landed(&self, pr: &PrRef) -> Result<ForgeEffect, ForgeError> {
        self.check_repo(pr)?;
        let out = self.gh_ok(&args(&[
            "pr",
            "view",
            &pr.number.to_string(),
            "-R",
            &self.repo,
            "--json",
            parse::LANDED_FIELDS,
        ]))?;
        let view = parse::landed_view(&out.stdout)?;
        let effect = |outcome| ForgeEffect {
            repo: self.repo.clone(),
            branch: view.branch.clone(),
            sha: view.sha.clone(),
            outcome,
        };
        // A PR that is not merged has not landed. A merged PR has landed when its merge commit
        // is an ancestor of the head of the base branch: the compare from the commit to the base
        // is then `ahead` (the base has more commits) or `identical`.
        let Some(merge_commit) = &view.merge_commit else {
            return Ok(effect(ForgeOutcome::NotLanded));
        };
        let compare = format!("repos/{}/compare/{merge_commit}...{}", self.repo, view.base);
        let out = self.gh_ok(&args(&["api", "--method", "GET", &compare]))?;
        let landed = parse::is_descendant_status(&out.stdout)?;
        Ok(effect(if landed {
            ForgeOutcome::Landed
        } else {
            ForgeOutcome::NotLanded
        }))
    }
}

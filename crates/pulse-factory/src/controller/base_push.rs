//! The base push of a stage (`docs/contracts/workflow-file.md`, `### base_push`).
//!
//! A stage may name a branch with `base_push: {dir, branch}`. Before the stage launches, the
//! controller asks `origin` of `dir` for the branch. When `origin` lacks it, the controller pushes
//! the local branch there and writes the push to the ledger. It never forces, never overwrites a
//! branch `origin` holds, never pushes the forge's default branch and never pushes to a repository
//! other than the run's forge. Every refusal and every failure is a park reason for the caller.
//!
//! Every name here is generic: a workflow file says which branch, and nothing in this module knows
//! why (ADR-0002).

use pf_engine::Engine;
use pf_engine::admission::PushOutcome;
use pf_engine::render::{RenderContext, render};
use pf_engine::status::one_line_reason;
use pf_workflow::commands::{CommandFailure, DEFAULT_TIMEOUT_SECONDS, RunOutput, run_argv_env};
use pf_workflow::file::{BasePush, StageSpec};

use crate::forge::parse_origin;

/// What the controller knows of the run's forge.
pub struct Forge<'a> {
    /// `owner/name` on GitHub, and the canonical path of `forge.git` in the paper env.
    pub repo: &'a str,
    pub default_branch: &'a str,
    /// The run is a paper run: the forge is a local bare repository.
    pub paper: bool,
}

/// What a git call may not do: ask a person for a password.
const GIT_ENV: [(&str, &str); 1] = [("GIT_TERMINAL_PROMPT", "0")];

/// Run `git -C <dir> <args>` as an argv, with no shell and with the command timeout.
fn git(dir: &str, args: &[&str]) -> Result<RunOutput, CommandFailure> {
    let mut argv: Vec<String> = ["git", "-C", dir]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    argv.extend(args.iter().map(|s| (*s).to_string()));
    run_argv_env(&argv, &GIT_ENV, None, DEFAULT_TIMEOUT_SECONDS)
}

/// The first non-empty line of stderr, cleaned to one line and redacted. Empty when there is none.
fn stderr_line(out: &RunOutput) -> String {
    let text = String::from_utf8_lossy(&out.stderr);
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    if line.is_empty() {
        return String::new();
    }
    one_line_reason(&line.trim().chars().take(160).collect::<String>())
}

/// `head`, then `: cause` when there is a cause.
fn with_cause(head: String, cause: &str) -> String {
    if cause.is_empty() {
        head
    } else {
        format!("{head}: {cause}")
    }
}

/// The canonical path of a local remote URL: a plain path or a `file://` URL.
fn canonical(url: &str) -> Option<std::path::PathBuf> {
    let path = url.trim().strip_prefix("file://").unwrap_or(url.trim());
    std::fs::canonicalize(path).ok()
}

/// The URLs `git remote get-url --all [--push] origin` prints, or the cause when it cannot print
/// one. A remote with several push URLs pushes to every one, so each counts.
fn origin_urls(dir: &str, push: bool) -> Result<Vec<String>, String> {
    let mut args = vec!["remote", "get-url", "--all"];
    if push {
        args.push("--push");
    }
    args.push("origin");
    let out = git(dir, &args).map_err(|e| e.to_string())?;
    if out.exit_code != Some(0) {
        let how = out.exit_code.map_or("ended by a signal".to_string(), |n| {
            format!("git exited with code {n}")
        });
        return Err(with_cause(how, &stderr_line(&out)));
    }
    let urls: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if urls.is_empty() {
        return Err("git printed no URL".to_string());
    }
    Ok(urls)
}

/// Does `origin` (a remote URL) name the run's forge?
fn is_run_forge(forge: &Forge, origin: &str) -> bool {
    if forge.paper {
        match (canonical(origin), canonical(forge.repo)) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    } else {
        // GitHub names are not case sensitive, and `arm` stores the name as GitHub spells it.
        parse_origin(origin).is_some_and(|name| name.eq_ignore_ascii_case(forge.repo))
    }
}

/// The reason for a branch the controller must not push, or `None` when the name is allowed.
fn refused_branch(branch: &str, forge: &Forge) -> Option<String> {
    let refuse = |rule: &str| Some(format!("base push of branch `{branch}` refused: {rule}"));
    if branch.trim().is_empty() {
        return refuse("a branch name must not be empty");
    }
    if branch.starts_with('-') {
        return refuse("a branch name must not start with `-`");
    }
    // `check-ref-format --branch` also expands `@{-1}`, so the printed name must be the one given.
    let argv: Vec<String> = ["git", "check-ref-format", "--branch", branch]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    let valid = run_argv_env(&argv, &GIT_ENV, None, DEFAULT_TIMEOUT_SECONDS).is_ok_and(|o| {
        o.exit_code == Some(0) && String::from_utf8_lossy(&o.stdout).trim() == branch
    });
    if !valid {
        return refuse("git check-ref-format --branch does not accept the name");
    }
    if branch == forge.default_branch {
        return refuse(&format!(
            "it is the default branch of the forge `{}`, and the controller never pushes it",
            forge.default_branch
        ));
    }
    None
}

/// Make sure `origin` holds the branch of the stage's `base_push`, before the stage launches.
///
/// `Ok` means the stage may go on: `origin` had the branch, or the controller pushed it and wrote
/// the ledger line. `Err` is the reason to park the unit with, and the controller pushed nothing
/// the reason does not name.
pub fn ensure(
    engine: &Engine,
    forge: &Forge,
    stage: &StageSpec,
    base: &BasePush,
    unit: &str,
    park_generation: u32,
    ctx: &RenderContext,
) -> Result<(), String> {
    let text = |what: &str, template: &str| {
        render(template, ctx).map(|r| r.text).map_err(|e| {
            format!(
                "cannot render the {what} of base_push of stage `{}` for unit {unit}: {e}",
                stage.name
            )
        })
    };
    let dir = text("dir", &base.dir)?;
    let branch = text("branch", &base.branch)?;
    if let Some(reason) = refused_branch(&branch, forge) {
        return Err(one_line_reason(&reason));
    }
    if dir.trim().is_empty() {
        return Err(format!(
            "base push of branch `{branch}` refused: the dir is empty"
        ));
    }

    // Only the forge the operator armed against receives a base push. `ls-remote` reads the fetch
    // URL of `origin`, and `push` writes to its push URLs (`remote.origin.pushurl`,
    // `pushInsteadOf`). Both must be the run's forge, so that the branch check and the push go to
    // one place. When `origin` cannot be read, the controller refuses: with no remote named
    // `origin`, git reads a directory of that name in `dir` as the remote.
    for push in [false, true] {
        let urls = origin_urls(&dir, push).map_err(|cause| {
            one_line_reason(&format!(
                "base push of branch `{branch}` refused: {}",
                with_cause(
                    format!(
                        "cannot read the {} URL of origin of {dir}",
                        if push { "push" } else { "fetch" }
                    ),
                    &cause
                )
            ))
        })?;
        if let Some(url) = urls.iter().find(|u| !is_run_forge(forge, u)) {
            let what = if push { "pushes to" } else { "is" };
            return Err(one_line_reason(&format!(
                "base push of branch `{branch}` refused: origin of {dir} {what} {url}, which is not the run's forge {}",
                forge.repo
            )));
        }
    }

    let unreadable = |how: String| format!("cannot read {branch} on origin of {dir} ({how})");
    let pattern = format!("refs/heads/{branch}");
    match git(&dir, &["ls-remote", "--exit-code", "origin", &pattern]) {
        Ok(out) => match out.exit_code {
            Some(0) => return Ok(()),
            // No matching ref: `origin` lacks the branch.
            Some(2) => {}
            Some(n) => {
                return Err(one_line_reason(&with_cause(
                    unreadable(format!("ls-remote rc {n}")),
                    &stderr_line(&out),
                )));
            }
            None => {
                return Err(one_line_reason(&with_cause(
                    unreadable("ls-remote ended by a signal".to_string()),
                    &stderr_line(&out),
                )));
            }
        },
        Err(CommandFailure::Timeout { .. }) => {
            return Err(one_line_reason(&unreadable(
                "ls-remote timed out".to_string(),
            )));
        }
        Err(e) => {
            return Err(one_line_reason(&unreadable(format!("ls-remote {e}"))));
        }
    }

    // `origin` lacks the branch. Each push attempt has its own command ID, and that ID is its record:
    // the attempt number counts the base pushes this unit already recorded in this park generation.
    let id_prefix = format!("push:base-push:{unit}:{park_generation}:");
    let attempt = match engine.view() {
        Ok(view) => view
            .admissions
            .iter()
            .filter(|a| a.kind == "push" && a.unit_id.as_deref() == Some(unit))
            .filter(|a| {
                a.command_id
                    .strip_prefix(&id_prefix)
                    .is_some_and(|n| n.parse::<u32>().is_ok())
            })
            .count(),
        Err(e) => {
            return Err(one_line_reason(&format!(
                "base push of {branch} to origin not tried: cannot read the ledger: {e}"
            )));
        }
    };
    let event_id = format!("base-push:{unit}:{park_generation}:{attempt}");
    let record = |sha: &str, outcome: PushOutcome| {
        engine
            .record_push(unit, &event_id, forge.repo, &branch, sha, outcome)
            .map(|_| ())
            .map_err(|e| e.to_string())
    };
    let failed = |sha: &str, cause: String| -> String {
        let note = match record(sha, PushOutcome::Failed(cause.clone())) {
            Ok(()) => String::new(),
            Err(e) => format!(" (the ledger line was not written: {e})"),
        };
        one_line_reason(&format!(
            "base push of {branch} to origin failed: {cause}{note}"
        ))
    };

    let local = git(&dir, &["rev-parse", "--verify", &pattern]);
    let sha = match local {
        Ok(out) if out.exit_code == Some(0) => {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
        _ => String::new(),
    };
    if sha.is_empty() {
        return Err(failed("", format!("no local branch {branch}")));
    }

    let refspec = format!("{pattern}:{pattern}");
    let cause = match git(&dir, &["push", "origin", &refspec]) {
        Ok(out) if out.exit_code == Some(0) => None,
        Ok(out) => {
            let line = stderr_line(&out);
            Some(if line.is_empty() {
                out.exit_code.map_or("ended by a signal".to_string(), |n| {
                    format!("exited with code {n}")
                })
            } else {
                line
            })
        }
        Err(e) => Some(e.to_string()),
    };
    match cause {
        Some(cause) => Err(failed(&sha, cause)),
        None => record(&sha, PushOutcome::Pushed).map_err(|e| {
            one_line_reason(&format!(
                "base push of {branch} to origin was done, but its ledger line was not written: {e}"
            ))
        }),
    }
}

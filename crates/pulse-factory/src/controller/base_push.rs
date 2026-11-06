//! The base push of a stage (`docs/contracts/workflow-file.md`, `### base_push`).
//!
//! A stage may name a branch with `base_push: {dir, branch}`. Before the stage launches, the
//! controller asks `origin` of `dir` for the branch. When `origin` lacks it, the controller pushes
//! the local branch there and writes the push to the ledger. It never forces, never overwrites a
//! branch `origin` holds, never pushes the forge's default branch and never pushes to a repository
//! other than the run's forge. Every refusal and every failure is a park reason for the caller.
//!
//! `dir` is a seat's repository, and a seat can write its git config. So the controller reads only
//! names from `dir` (`seat_read`: no transport runs there). It checks, asks and pushes from a bare
//! repository of its own (`own`), which borrows the objects of `dir`. Each seat-written setting it
//! ignores gets one `note:` line in the ledger.
//!
//! Every name here is generic: a workflow file says which branch, and nothing in this module knows
//! why (ADR-0002).

use std::path::{Path, PathBuf};

use pf_engine::Engine;
use pf_engine::admission::PushOutcome;
use pf_engine::render::{RenderContext, render};
use pf_engine::status::{one_line_reason, redact};
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
    /// Where the controller keeps its own repositories: `<run dir>/controller/base-push`.
    pub work_dir: &'a Path,
}

/// What a git call may not do: ask a person for a password. `LC_ALL=C` fixes the language of git's
/// messages.
const GIT_ENV: [(&str, &str); 2] = [("GIT_TERMINAL_PROMPT", "0"), ("LC_ALL", "C")];

/// Put before the subcommand of every git call: no hook runs and no file-system monitor runs. A
/// command-line `-c` wins over every config file.
const SAFE_CONFIG: [&str; 4] = [
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "core.fsmonitor=false",
];

/// Run `git <where_> <SAFE_CONFIG> <args>` as an argv, with no shell and with the command timeout.
fn run_git(where_: Vec<String>, args: &[&str]) -> Result<RunOutput, CommandFailure> {
    let mut argv: Vec<String> = vec!["git".to_string()];
    argv.extend(where_);
    argv.extend(SAFE_CONFIG.iter().map(|s| (*s).to_string()));
    argv.extend(args.iter().map(|s| (*s).to_string()));
    run_argv_env(&argv, &GIT_ENV, None, DEFAULT_TIMEOUT_SECONDS)
}

/// Run `git -C <dir> <args>` in the seat's repository. Only for calls that read names and run no
/// transport: the seat can write the config of `dir`.
fn seat_read(dir: &str, args: &[&str]) -> Result<RunOutput, CommandFailure> {
    run_git(vec!["-C".to_string(), dir.to_string()], args)
}

/// Run `git --git-dir=<git_dir> <args>` in the controller's own repository. Every call that reaches
/// the forge uses it.
fn own(git_dir: &Path, args: &[&str]) -> Result<RunOutput, CommandFailure> {
    run_git(vec![format!("--git-dir={}", git_dir.display())], args)
}

/// The first non-empty line of stderr, redacted, cut to 160 characters and cleaned to one line.
/// Empty when there is none. The redaction sees the whole line, so a token that crosses the cut is
/// never shown in part.
fn stderr_line(out: &RunOutput) -> String {
    let text = String::from_utf8_lossy(&out.stderr);
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    if line.is_empty() {
        return String::new();
    }
    let redacted = redact(line.trim());
    one_line_reason(&redacted.chars().take(160).collect::<String>())
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

/// The URLs `git remote get-url --all [--push] <remote>` prints, or the cause when it cannot print
/// one. A remote with several push URLs pushes to every one, so each counts. `run` runs git.
fn remote_urls(
    run: &dyn Fn(&[&str]) -> Result<RunOutput, CommandFailure>,
    remote: &str,
    push: bool,
) -> Result<Vec<String>, String> {
    let mut args = vec!["remote", "get-url", "--all"];
    if push {
        args.push("--push");
    }
    args.push(remote);
    let out = run(&args).map_err(|e| e.to_string())?;
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
    // A URL that starts with `-` could stand in for an option of the `git config` call that stores it.
    if origin.trim().starts_with('-') {
        return false;
    }
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

/// Does the output of `git ls-remote <remote> <refname>` hold the ref `refname`, by its full name?
/// `ls-remote` matches a pattern by the tail of a ref, so `refs/heads/b` also prints
/// `refs/tags/refs/heads/b` and `refs/heads/x/refs/heads/b`. Only a line whose second field is
/// exactly `refname` counts.
fn holds_exact(stdout: &str, refname: &str) -> bool {
    stdout
        .lines()
        .any(|line| line.split_whitespace().nth(1) == Some(refname))
}

/// The keys of git config the ledger names when a seat sets them: the settings that run a command,
/// read a file, or change a URL or a credential. The controller ignores them whether they are
/// listed or not, because no transport runs in the seat's repository. The list only chooses what
/// the ledger names. It is the same list as [`CONFIG_KEYS`], written as a function so that a key is
/// checked again after git printed it.
fn is_listed_key(key: &str) -> bool {
    let core = [
        "hookspath",
        "fsmonitor",
        "sshcommand",
        "gitproxy",
        "askpass",
        "alternaterefscommand",
    ];
    let origin = ["receivepack", "uploadpack", "vcs", "proxy"];
    if let Some(name) = key.strip_prefix("core.") {
        return core.contains(&name);
    }
    if let Some(name) = key.strip_prefix("remote.origin.") {
        return origin.contains(&name);
    }
    if let Some(rest) = key.strip_prefix("url.") {
        return rest.ends_with(".insteadof") || rest.ends_with(".pushinsteadof");
    }
    key.starts_with("credential.")
        || key.starts_with("http.")
        || key.starts_with("protocol.")
        || key.starts_with("includeif.")
        || key == "include.path"
}

/// The extended regular expression that `git config --get-regexp` takes: the keys of
/// [`is_listed_key`], in the lower case git prints them in.
const CONFIG_KEYS: &str = r"^(core\.(hookspath|fsmonitor|sshcommand|gitproxy|askpass|alternaterefscommand)|credential\..*|url\..*\.(insteadof|pushinsteadof)|remote\.origin\.(receivepack|uploadpack|vcs|proxy)|http\..*|protocol\..*|include\.path|includeif\..*)$";

/// The settings of the seat's own config, from the output of
/// `git config -z --show-scope --get-regexp`: `(key, scope)`, in output order and once per key.
/// Only the scopes `local` and `worktree` are the seat's. The global and system scopes are the
/// operator's, and the controller's own repository reads them.
///
/// With `-z`, each entry is `scope NUL key NL value NUL`, so a newline in a value cannot forge an
/// entry. A key that is not listed is dropped even so.
fn seat_settings(stdout: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut parts = stdout.split('\0');
    while let (Some(scope), Some(entry)) = (parts.next(), parts.next()) {
        if scope != "local" && scope != "worktree" {
            continue;
        }
        let key = entry.split('\n').next().unwrap_or("");
        if !is_listed_key(key) || out.iter().any(|(k, _)| k == key) {
            continue;
        }
        out.push((key.to_string(), scope.to_string()));
    }
    out
}

/// The controller's own repository for one call of [`ensure`]. It is removed when the value drops,
/// so every path out of `ensure` removes it. A failure to remove it is not a park reason.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Stdout of a git call that must exit 0, trimmed, or the cause.
fn stdout_of(result: Result<RunOutput, CommandFailure>) -> Result<String, String> {
    let out = result.map_err(|e| e.to_string())?;
    if out.exit_code != Some(0) {
        let how = out.exit_code.map_or("ended by a signal".to_string(), |n| {
            format!("git exited with code {n}")
        });
        return Err(with_cause(how, &stderr_line(&out)));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Make the controller's bare repository `<work_dir>/<unit>.git`. It holds no objects and no hooks:
/// `objects/info/alternates` names the object directory of the seat's repository.
fn make_scratch(forge: &Forge, unit: &str, common: &Path) -> Result<Scratch, String> {
    let path = forge.work_dir.join(format!("{unit}.git"));
    if path.symlink_metadata().is_ok() {
        std::fs::remove_dir_all(&path)
            .map_err(|e| format!("cannot remove the old {path:?}: {e}"))?;
    }
    std::fs::create_dir_all(forge.work_dir)
        .map_err(|e| format!("cannot make {:?}: {e}", forge.work_dir))?;
    // From here the guard removes the directory on every path.
    let scratch = Scratch(path.clone());
    let target = path.display().to_string();
    stdout_of(run_git(
        Vec::new(),
        &["init", "--bare", "--quiet", "--template=", &target],
    ))?;
    let alternates = path.join("objects").join("info").join("alternates");
    std::fs::write(
        &alternates,
        format!("{}\n", common.join("objects").display()),
    )
    .map_err(|e| format!("cannot write {alternates:?}: {e}"))?;
    Ok(scratch)
}

/// Is the fetch URL and every push URL of the controller's remote `forge` the run's forge? The
/// controller asks this twice: before it asks the forge for the branch, and again right before the
/// push. Git reads the global and system config at each call, and a seat can write the operator's
/// global config from its shell, so the second ask narrows the time in which a `pushurl` or a
/// `pushInsteadOf` written there could move the push. It does not close that time (ADR-0005).
/// `Err` is the refusal.
fn check_own_remote(
    forge: &Forge,
    git_dir: &Path,
    dir: &str,
    refuse: &dyn Fn(String) -> String,
) -> Result<(), String> {
    let in_own = |args: &[&str]| own(git_dir, args);
    for push in [false, true] {
        let urls = remote_urls(&in_own, "forge", push).map_err(|cause| {
            refuse(with_cause(
                format!(
                    "cannot read the {} URL of the controller's remote for {dir}",
                    if push { "push" } else { "fetch" }
                ),
                &cause,
            ))
        })?;
        if let Some(bad) = urls.iter().find(|u| !is_run_forge(forge, u)) {
            return Err(refuse(format!(
                "the controller's remote for {dir} is {bad}, which is not the run's forge {}",
                forge.repo
            )));
        }
    }
    Ok(())
}

/// The refs, one per line, that `git for-each-ref --format=%(refname)` printed, other than
/// `pattern`. The glob refspec of the base push sends every ref whose name starts with `pattern`,
/// so the push is safe only when `pattern` is the only ref of the controller's repository.
fn refs_besides<'a>(stdout: &'a str, pattern: &str) -> Vec<&'a str> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && *l != pattern)
        .collect()
}

/// Make `pattern` in the controller's repository `git_dir` point at `sha`, check that it is the
/// only ref there, and push it to `forge`. Never a force flag and never a `+`. The push sends the
/// one SHA to the one ref `pattern` (`refs/heads/<branch>`). A plain `<sha>:refs/heads/<branch>` is
/// not safe on a forge that holds `refs/tags/refs/heads/<branch>`: git reads the destination as an
/// abbreviation and aims the push at that tag. A glob refspec names the remote ref by its full
/// name, so the push can reach nothing else on the forge. But the glob also sends every ref of the
/// controller's repository whose name starts with `pattern`. That repository is made fresh for
/// this call and a seat can reach it only by a shell write (ADR-0005), so the controller lists its
/// refs after it makes its own and sends nothing when another is there.
///
/// `Err` is the cause, for the ledger line of a failed push.
fn push_armed_ref(git_dir: &Path, pattern: &str, sha: &str) -> Result<(), String> {
    stdout_of(own(git_dir, &["update-ref", pattern, sha]))
        .map_err(|cause| with_cause("cannot make the ref to push".to_string(), &cause))?;
    let listed = stdout_of(own(git_dir, &["for-each-ref", "--format=%(refname)"]))
        .map_err(|cause| with_cause("cannot list the refs to push".to_string(), &cause))?;
    let others = refs_besides(&listed, pattern);
    if !others.is_empty() {
        return Err(redact(&format!(
            "the controller's repository holds {} besides {pattern}, and nothing was pushed",
            others.join(", ")
        )));
    }
    let refspec = format!("{pattern}*:{pattern}*");
    match own(git_dir, &["push", "forge", &refspec]) {
        Ok(out) if out.exit_code == Some(0) => Ok(()),
        Ok(out) => {
            let line = stderr_line(&out);
            Err(if line.is_empty() {
                out.exit_code.map_or("ended by a signal".to_string(), |n| {
                    format!("exited with code {n}")
                })
            } else {
                line
            })
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Make sure `origin` holds the branch of the stage's `base_push`, before the stage launches.
///
/// `Ok` means the stage may go on: `origin` had the branch, or the controller pushed it and wrote
/// the ledger line. The `Ok` value lists the notes the ledger refused: a note that fails is not a
/// park reason, so the caller reports it. `Err` is the reason to park the unit with, and the
/// controller pushed nothing the reason does not name.
pub fn ensure(
    engine: &Engine,
    forge: &Forge,
    stage: &StageSpec,
    base: &BasePush,
    unit: &str,
    park_generation: u32,
    ctx: &RenderContext,
) -> Result<Vec<String>, String> {
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
    let refuse =
        |what: String| one_line_reason(&format!("base push of branch `{branch}` refused: {what}"));

    // The seat's repository gives names and objects. Its common directory holds the objects that
    // the controller's own repository borrows.
    let common = stdout_of(seat_read(
        &dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ))
    .map_err(|cause| {
        refuse(with_cause(
            format!("cannot read the common git directory of {dir}"),
            &cause,
        ))
    })?;
    let common = PathBuf::from(common);
    if !common.is_absolute() || !common.join("objects").is_dir() {
        return Err(refuse(format!(
            "cannot read the common git directory of {dir}: {} has no objects directory",
            common.display()
        )));
    }
    let scratch = make_scratch(forge, unit, &common).map_err(|cause| {
        refuse(with_cause(
            format!("cannot make the controller's repository for {dir}"),
            &cause,
        ))
    })?;
    let git_dir = scratch.0.as_path();

    // One ledger line for each seat-written setting the controller ignores. The key is named, never
    // the value: a value can hold a command or a URL with a secret.
    let mut note_failures: Vec<String> = Vec::new();
    let settings = match seat_read(
        &dir,
        &["config", "-z", "--show-scope", "--get-regexp", CONFIG_KEYS],
    ) {
        Ok(out) if out.exit_code == Some(0) => seat_settings(&String::from_utf8_lossy(&out.stdout)),
        // Exit 1: no key matches.
        Ok(out) if out.exit_code == Some(1) => Vec::new(),
        other => {
            let cause = match other {
                Ok(out) => stdout_of(Ok(out)).err().unwrap_or_default(),
                Err(e) => e.to_string(),
            };
            return Err(refuse(with_cause(
                format!("cannot read the git config of {dir}"),
                &cause,
            )));
        }
    };
    for (key, scope) in &settings {
        let id = format!("base-push-config:{unit}:{park_generation}:{key}");
        let what = format!("base push ignored git config {key} ({scope}) of {dir}");
        if let Err(e) = engine.record_note(unit, &id, &what) {
            note_failures.push(format!(
                "the note for git config {key} of {dir} failed: {e}"
            ));
        }
    }

    // Only the forge the operator armed against receives a base push. The fetch URL and every push
    // URL of `origin` (`remote.origin.pushurl`, `pushInsteadOf`) must be the run's forge, so that the
    // branch check and the push go to one place. When `origin` cannot be read, the controller
    // refuses: with no remote named `origin`, git reads a directory of that name in `dir` as the
    // remote. The first push URL is the one the controller uses, in its own repository.
    let in_dir = |args: &[&str]| seat_read(&dir, args);
    let mut url = String::new();
    for push in [false, true] {
        let urls = remote_urls(&in_dir, "origin", push).map_err(|cause| {
            refuse(with_cause(
                format!(
                    "cannot read the {} URL of origin of {dir}",
                    if push { "push" } else { "fetch" }
                ),
                &cause,
            ))
        })?;
        if let Some(bad) = urls.iter().find(|u| !is_run_forge(forge, u)) {
            let what = if push { "pushes to" } else { "is" };
            return Err(refuse(format!(
                "origin of {dir} {what} {bad}, which is not the run's forge {}",
                forge.repo
            )));
        }
        if push {
            url.clone_from(&urls[0]);
        }
    }

    // The URL that was checked is the URL the controller's repository uses. Nothing in `dir` is read
    // again, so a seat that changes the config of `dir` from here on changes nothing the push uses.
    stdout_of(own(git_dir, &["config", "remote.forge.url", &url])).map_err(|cause| {
        refuse(with_cause(
            format!("cannot set the controller's remote for {dir}"),
            &cause,
        ))
    })?;
    check_own_remote(forge, git_dir, &dir, &refuse)?;

    let unreadable = |how: String| format!("cannot read {branch} on origin of {dir} ({how})");
    let pattern = format!("refs/heads/{branch}");
    match own(git_dir, &["ls-remote", "forge", &pattern]) {
        Ok(out) => match out.exit_code {
            // Exit 0 with no line for the exact ref: `origin` lacks the branch.
            Some(0) => {
                if holds_exact(&String::from_utf8_lossy(&out.stdout), &pattern) {
                    return Ok(note_failures);
                }
            }
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

    let local = seat_read(&dir, &["rev-parse", "--verify", &pattern]);
    let sha = match local {
        Ok(out) if out.exit_code == Some(0) => {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
        _ => String::new(),
    };
    if sha.is_empty() {
        return Err(failed("", format!("no local branch {branch}")));
    }

    // The URL check again, right before the push (see `check_own_remote`).
    check_own_remote(forge, git_dir, &dir, &refuse)?;
    let cause = push_armed_ref(git_dir, &pattern, &sha).err();
    match cause {
        Some(cause) => Err(failed(&sha, cause)),
        None => record(&sha, PushOutcome::Pushed)
            .map(|()| note_failures)
            .map_err(|e| {
                one_line_reason(&format!(
                    "base push of {branch} to origin was done, but its ledger line was not written: {e}"
                ))
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn github(repo: &'static str) -> Forge<'static> {
        Forge {
            repo,
            default_branch: "main",
            paper: false,
            work_dir: Path::new("/nonexistent"),
        }
    }

    #[test]
    fn is_run_forge_reads_the_github_url_forms() {
        let forge = github("owner/name");
        for url in [
            "https://github.com/owner/name",
            "https://github.com/owner/name.git",
            "https://github.com/owner/name/",
            "https://user@github.com/owner/name.git",
            "git@github.com:owner/name.git",
            "git@github.com:owner/name",
            "ssh://git@github.com/owner/name.git",
            "ssh://git@github.com:22/owner/name.git",
            "ssh://git@ssh.github.com:443/owner/name.git",
            // GitHub names are not case sensitive.
            "https://github.com/Owner/NAME.git",
            "https://GitHub.com/owner/name",
        ] {
            assert!(is_run_forge(&forge, url), "{url}");
        }
    }

    #[test]
    fn is_run_forge_refuses_another_repo_owner_host_or_lookalike() {
        let forge = github("owner/name");
        for url in [
            // Another repository, and another owner.
            "https://github.com/owner/other",
            "https://github.com/other/name",
            "git@github.com:other/name.git",
            // A longer name, a nested path and an empty one.
            "https://github.com/owner/name-x",
            "https://github.com/owner/name/extra",
            "https://github.com/",
            // Lookalike hosts, and user info that names github.com but is not the host.
            "https://github.com.evil.example/owner/name",
            "https://notgithub.com/owner/name",
            "https://evilgithub.com/owner/name",
            "https://github.com@evil.example/owner/name",
            "git@github.com.evil.example:owner/name.git",
            // Other schemes, and a URL that is an option.
            "http://github.com/owner/name",
            "git://github.com/owner/name",
            "file:///srv/owner/name",
            "-owner/name",
            "--upload-pack=x",
            "",
        ] {
            assert!(!is_run_forge(&forge, url), "{url}");
        }
    }

    #[test]
    fn refused_branch_refuses_a_name_git_would_expand_or_read_as_an_option() {
        let forge = github("owner/name");
        // `check-ref-format --branch` expands `@{-1}` to the previous branch, so the printed name
        // is not the given one.
        for branch in ["@{-1}", "", "  ", "-x", "bad..name", "a b", "main"] {
            assert!(refused_branch(branch, &forge).is_some(), "{branch:?}");
        }
        for branch in ["spine/r1", "work/u1", "base-x", "feature.1"] {
            assert_eq!(refused_branch(branch, &forge), None, "{branch:?}");
        }
        let reason = refused_branch("@{-1}", &forge).unwrap();
        assert!(reason.contains("check-ref-format"), "{reason}");
    }

    #[test]
    fn holds_exact_takes_the_full_ref_name_and_nothing_that_only_ends_like_it() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let refname = "refs/heads/base-x";
        // The exact ref.
        assert!(holds_exact(&format!("{sha}\trefs/heads/base-x\n"), refname));
        assert!(holds_exact(
            &format!("{sha}\trefs/heads/other\n{sha}\trefs/heads/base-x\n"),
            refname
        ));
        // `ls-remote` prints these for the pattern `refs/heads/base-x`, and none is the branch.
        assert!(!holds_exact(
            &format!("{sha}\trefs/tags/refs/heads/base-x\n"),
            refname
        ));
        assert!(!holds_exact(
            &format!("{sha}\trefs/heads/base-x-x\n"),
            refname
        ));
        assert!(!holds_exact(
            &format!("{sha}\trefs/heads/x/refs/heads/base-x\n"),
            refname
        ));
        assert!(!holds_exact(
            &format!("{sha}\trefs/heads/base-x/y\n"),
            refname
        ));
        assert!(!holds_exact(
            &format!("{sha}\trefs/remotes/refs/heads/base-x\n"),
            refname
        ));
        // No output, and a line with one field.
        assert!(!holds_exact("", refname));
        assert!(!holds_exact("\n\n", refname));
        assert!(!holds_exact(refname, refname));
        // The ref name is the second field, not the first.
        assert!(!holds_exact(&format!("{refname}\t{sha}\n"), refname));
    }

    fn entry(scope: &str, key: &str, value: &str) -> String {
        format!("{scope}\0{key}\n{value}\0")
    }

    #[test]
    fn seat_settings_keeps_the_local_and_worktree_scopes_and_each_key_once() {
        let out = [
            entry("system", "core.sshcommand", "a"),
            entry("global", "credential.helper", "!gh"),
            entry("local", "core.hookspath", "/x"),
            entry("local", "credential.helper", "!f"),
            entry("local", "credential.helper", "!g"),
            entry("worktree", "core.fsmonitor", "/y"),
            entry("command", "core.askpass", "z"),
            entry("local", "url.https://a b/.insteadof", "c"),
            entry("local", "remote.origin.receivepack", "r"),
        ]
        .concat();
        let got = seat_settings(&out);
        let want: Vec<(String, String)> = [
            ("core.hookspath", "local"),
            ("credential.helper", "local"),
            ("core.fsmonitor", "worktree"),
            ("url.https://a b/.insteadof", "local"),
            ("remote.origin.receivepack", "local"),
        ]
        .iter()
        .map(|(k, s)| ((*k).to_string(), (*s).to_string()))
        .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn seat_settings_cannot_be_forged_by_a_value_and_drops_a_key_that_is_not_listed() {
        // A value with a newline and a fake entry in it is one value of one key.
        let forged = entry("local", "core.hookspath", "x\nlocal\0core.sshcommand\ny");
        assert_eq!(
            seat_settings(&forged),
            vec![("core.hookspath".to_string(), "local".to_string())]
        );
        // Git printed it, but the list does not name it.
        assert!(seat_settings(&entry("local", "user.name", "a")).is_empty());
        assert!(seat_settings("").is_empty());
        // An odd tail is ignored.
        assert!(seat_settings("local").is_empty());
    }

    #[test]
    fn the_listed_keys_are_those_of_the_regular_expression() {
        for key in [
            "core.hookspath",
            "core.fsmonitor",
            "core.sshcommand",
            "core.gitproxy",
            "core.askpass",
            "core.alternaterefscommand",
            "credential.helper",
            "credential.https://x.helper",
            "url.https://x/.insteadof",
            "url.https://x/.pushinsteadof",
            "remote.origin.receivepack",
            "remote.origin.uploadpack",
            "remote.origin.vcs",
            "remote.origin.proxy",
            "http.proxy",
            "http.https://x/.sslverify",
            "protocol.ext.allow",
            "include.path",
            "includeif.gitdir:/x.path",
        ] {
            assert!(is_listed_key(key), "{key}");
        }
        for key in [
            "core.editor",
            "core.hookspathx",
            "user.name",
            "url.https://x/.pushurl",
            "remote.origin.url",
            "remote.other.receivepack",
            "include.pathx",
            "includes.path",
            "",
        ] {
            assert!(!is_listed_key(key), "{key}");
        }
    }

    fn stderr_of(text: &str) -> RunOutput {
        RunOutput {
            exit_code: Some(128),
            stdout: Vec::new(),
            stderr: text.as_bytes().to_vec(),
            stdout_cut: false,
            stderr_cut: false,
        }
    }

    #[test]
    fn stderr_line_redacts_the_whole_line_before_it_cuts() {
        let token = "ghp_0123456789abcdefghijklmnopqrstuvwxyz";
        // The token starts before character 160 and ends after it.
        let line = format!("fatal: {} {token} rejected", "x".repeat(140));
        let got = stderr_line(&stderr_of(&line));
        assert!(!got.contains("ghp_"), "{got}");
        assert!(!got.contains("0123456789"), "{got}");
        assert!(got.chars().count() <= 160, "{got}");
        // A short line is kept, the first non-empty line wins, and no line gives an empty string.
        assert_eq!(
            stderr_line(&stderr_of("\n  \n fatal: nope \nsecond\n")),
            "fatal: nope"
        );
        assert_eq!(stderr_line(&stderr_of("")), "");
    }

    #[test]
    fn a_git_call_in_the_seats_repository_or_the_controllers_own_sets_the_safe_config() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("r");
        std::fs::create_dir_all(&repo).unwrap();
        let init = run_git(Vec::new(), &["init", "--quiet", repo.to_str().unwrap()]).unwrap();
        assert_eq!(init.exit_code, Some(0));
        // The repository sets `core.hooksPath`, and the call must still see `/dev/null`.
        let set = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["config", "core.hooksPath", "/seat/hooks"])
            .status()
            .unwrap();
        assert!(set.success());
        let seen = |out: Result<RunOutput, CommandFailure>| {
            String::from_utf8_lossy(&out.unwrap().stdout)
                .trim()
                .to_string()
        };
        assert_eq!(
            seen(seat_read(
                repo.to_str().unwrap(),
                &["config", "--get", "core.hooksPath"]
            )),
            "/dev/null"
        );
        assert_eq!(
            seen(own(
                &repo.join(".git"),
                &["config", "--get", "core.hooksPath"]
            )),
            "/dev/null"
        );
        assert_eq!(
            seen(seat_read(
                repo.to_str().unwrap(),
                &["config", "--get", "core.fsmonitor"]
            )),
            "false"
        );
    }
    #[test]
    fn refs_besides_keeps_every_ref_but_the_armed_one() {
        let pattern = "refs/heads/work/u1";
        assert!(refs_besides("refs/heads/work/u1\n", pattern).is_empty());
        assert!(refs_besides("", pattern).is_empty());
        // The glob refspec `refs/heads/work/u1*` would send each of these.
        assert_eq!(
            refs_besides(
                "refs/heads/work/u1\nrefs/heads/work/u1x\nrefs/heads/work/u1/y\n",
                pattern
            ),
            vec!["refs/heads/work/u1x", "refs/heads/work/u1/y"]
        );
    }

    /// Run `git <args>` and panic unless it exits 0.
    fn git_ok(args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A bare forge, a seat repository with one commit, and a controller repository that borrows
    /// the seat's objects and has the forge as `forge`. Returns (dir, forge, controller, sha).
    fn push_fixture() -> (tempfile::TempDir, PathBuf, PathBuf, String) {
        let dir = tempfile::tempdir().unwrap();
        let forge = dir.path().join("forge.git");
        let seat = dir.path().join("seat");
        let own_git = dir.path().join("own.git");
        git_ok(&["init", "--quiet", "--bare", forge.to_str().unwrap()]);
        git_ok(&["init", "--quiet", seat.to_str().unwrap()]);
        git_ok(&[
            "-C",
            seat.to_str().unwrap(),
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "x",
        ]);
        let sha = git_ok(&["-C", seat.to_str().unwrap(), "rev-parse", "HEAD"]);
        git_ok(&["init", "--quiet", "--bare", own_git.to_str().unwrap()]);
        std::fs::write(
            own_git.join("objects/info/alternates"),
            format!("{}\n", seat.join(".git/objects").display()),
        )
        .unwrap();
        git_ok(&[
            "--git-dir",
            own_git.to_str().unwrap(),
            "config",
            "remote.forge.url",
            forge.to_str().unwrap(),
        ]);
        (dir, forge, own_git, sha)
    }

    fn forge_refs(forge: &Path) -> String {
        git_ok(&[
            "--git-dir",
            forge.to_str().unwrap(),
            "for-each-ref",
            "--format=%(refname)",
        ])
    }

    #[test]
    fn the_push_sends_the_armed_ref_and_nothing_else() {
        let (_dir, forge, own_git, sha) = push_fixture();
        push_armed_ref(&own_git, "refs/heads/work/u1", &sha).unwrap();
        assert_eq!(forge_refs(&forge), "refs/heads/work/u1");
    }

    /// PR #28, C1: with the refspec `refs/heads/*`, or with no check of the refs, a ref that a
    /// seat wrote into the controller's repository would reach the forge.
    #[test]
    fn the_push_sends_nothing_when_another_ref_starts_with_the_armed_name() {
        // `refs/heads/work/u1/y` cannot exist beside the armed ref: git refuses the pair.
        for extra in ["refs/heads/work/u1x", "refs/heads/work/u1-evil"] {
            let (_dir, forge, own_git, sha) = push_fixture();
            git_ok(&[
                "--git-dir",
                own_git.to_str().unwrap(),
                "update-ref",
                extra,
                &sha,
            ]);
            let cause = push_armed_ref(&own_git, "refs/heads/work/u1", &sha).unwrap_err();
            assert!(cause.contains(extra), "{cause}");
            assert!(cause.contains("nothing was pushed"), "{cause}");
            assert_eq!(forge_refs(&forge), "", "{extra}");
        }
    }
}

//! The few `git` reads the adapter makes: a branch's SHA, the default branch of a remote, and the
//! remote-tracking refs that a push moved. Every call is `git -C <dir> …` with prompts off and a
//! 30-second limit. No read takes its answer from a seat's text.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const LIMIT: Duration = Duration::from_secs(30);

/// Run `git -C dir args…` and give its trimmed stdout, or `None` when it fails.
pub fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let end = Instant::now() + LIMIT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut out = String::new();
                if let Some(mut so) = child.stdout.take() {
                    std::io::Read::read_to_string(&mut so, &mut out).ok()?;
                }
                return Some(out.trim().to_string());
            }
            Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// The full SHA a name points to.
pub fn rev_parse(dir: &Path, rev: &str) -> Option<String> {
    let sha = git(dir, &["rev-parse", "--verify", "--quiet", rev])?;
    (!sha.is_empty()).then_some(sha)
}

/// The default branch of a remote and its SHA, from `ls-remote --symref <remote> HEAD`.
pub fn remote_head(dir: &Path, remote: &str) -> Option<(String, String)> {
    let out = git(dir, &["ls-remote", "--symref", remote, "HEAD"])?;
    let mut branch = None;
    let mut sha = None;
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("ref: refs/heads/") {
            branch = rest.split_whitespace().next().map(str::to_string);
        } else if line.ends_with("\tHEAD") {
            sha = line.split_whitespace().next().map(str::to_string);
        }
    }
    Some((branch?, sha?))
}

/// The SHA of `refs/heads/<branch>` on a remote, from `ls-remote --exit-code`. A branch the remote
/// lacks, and a read that fails, give `None`. Only a line for exactly that ref counts, so a branch
/// `x/main` is not `main`.
pub fn remote_branch_sha(dir: &Path, remote: &str, branch: &str) -> Option<String> {
    let want = format!("refs/heads/{branch}");
    let out = git(dir, &["ls-remote", "--exit-code", remote, &want])?;
    out.lines().find_map(|line| {
        let (sha, name) = line.split_once('\t')?;
        (name == want).then(|| sha.trim().to_string())
    })
}

/// The remote-tracking refs of a remote and their SHAs, by branch name, without `<remote>/HEAD`.
/// A failed read gives `None`.
pub fn remote_refs(dir: &Path, remote: &str) -> Option<BTreeMap<String, String>> {
    let prefix = format!("refs/remotes/{remote}/");
    let out = git(
        dir,
        &[
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            &format!("refs/remotes/{remote}"),
        ],
    )?;
    let mut refs = BTreeMap::new();
    for line in out.lines() {
        let Some((sha, name)) = line.split_once(' ') else {
            continue;
        };
        let Some(branch) = name.strip_prefix(&prefix) else {
            continue;
        };
        if branch != "HEAD" && !branch.is_empty() {
            refs.insert(branch.to_string(), sha.to_string());
        }
    }
    Some(refs)
}

/// Does the repository hold this commit?
pub fn has_commit(dir: &Path, sha: &str) -> bool {
    !sha.is_empty()
        && !sha.starts_with('-')
        && git(dir, &["cat-file", "-e", &format!("{sha}^{{commit}}")]).is_some()
}

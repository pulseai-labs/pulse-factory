//! The few `git` reads the adapter makes: a branch's SHA, the default branch of a remote, and the
//! summary line of a push. Every call is `git -C <dir> …` with prompts off and a time limit.

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

/// The SHA of `refs/heads/<branch>` on a remote.
pub fn remote_branch_sha(dir: &Path, remote: &str, branch: &str) -> Option<String> {
    let r = format!("refs/heads/{branch}");
    let out = git(dir, &["ls-remote", remote, &r])?;
    out.split_whitespace().next().map(str::to_string)
}

/// What a push's summary line says: the remote branch and, when the line gives one, the SHA the
/// branch moved to. The SHA can be short.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    pub branch: String,
    pub sha: Option<String>,
}

/// Read the first accepted line of `git push` output, such as
/// `   abc1234..def5678  local -> remote` or ` * [new branch]      local -> remote`.
/// A rejected line (` ! `) and a text with no line of that form give `None`.
pub fn summary_of(output: &str) -> Option<Summary> {
    for line in output.lines() {
        if line.starts_with(" !") || !line.starts_with(' ') {
            continue;
        }
        let Some((left, right)) = line.split_once(" -> ") else {
            continue;
        };
        let branch = right.split_whitespace().next()?;
        let branch = branch.strip_prefix("refs/heads/").unwrap_or(branch);
        let sha = left
            .split_whitespace()
            .find_map(|w| w.rsplit_once("..").map(|(_, b)| b.trim_start_matches('.')))
            .filter(|s| s.len() >= 7 && s.chars().all(|c| c.is_ascii_hexdigit()))
            .map(str::to_string);
        return Some(Summary {
            branch: branch.to_string(),
            sha,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_an_update_line() {
        let out = "To /tmp/r.git\n   abc1234..def5678  topic -> topic\n";
        assert_eq!(
            summary_of(out),
            Some(Summary {
                branch: "topic".into(),
                sha: Some("def5678".into())
            })
        );
    }

    #[test]
    fn a_new_branch_has_no_sha_and_a_rejection_is_skipped() {
        assert_eq!(
            summary_of(" * [new branch]      a -> b").map(|s| (s.branch, s.sha)),
            Some(("b".to_string(), None))
        );
        assert_eq!(
            summary_of(" ! [rejected]        a -> b (fetch first)"),
            None
        );
        assert_eq!(summary_of("Everything up-to-date"), None);
    }
}

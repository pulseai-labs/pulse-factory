//! A real bare repo for the forge tests, built with real git. No test reaches the network, and
//! no test deletes a branch.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use pf_forge_bare::BareForge;

pub const BASE: &str = "spine";

/// Run git in `dir`, away from the host's git config. It panics when git fails.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = git_out(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Run git in `dir` and give the whole output, whatever the exit code.
pub fn git_out(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=spine",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .expect("git runs")
}

/// A bare repo, a work clone to build commits in, and the forge over the bare repo.
pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub bare: PathBuf,
    pub work: PathBuf,
    pub forge: BareForge,
}

impl Fixture {
    /// A bare repo whose `spine` branch holds one commit, `README`.
    pub fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let bare = dir.path().join("repo.git");
        let work = dir.path().join("work");
        std::fs::create_dir_all(&bare).unwrap();
        git(
            &bare,
            &["init", "--quiet", "--bare", "--initial-branch=spine"],
        );
        git(
            dir.path(),
            &[
                "clone",
                "--quiet",
                bare.to_str().unwrap(),
                work.to_str().unwrap(),
            ],
        );
        git(&work, &["checkout", "--quiet", "-b", BASE]);
        std::fs::write(work.join("README"), "line one\nline two\nline three\n").unwrap();
        git(&work, &["add", "-A"]);
        git(&work, &["commit", "--quiet", "-m", "start"]);
        git(&work, &["push", "--quiet", "origin", BASE]);
        let forge = BareForge::open(&bare).unwrap();
        Fixture {
            dir,
            bare,
            work,
            forge,
        }
    }

    /// The repo name the forge answers to, as a PR names it.
    pub fn repo(&self) -> String {
        self.forge.repo().to_string()
    }

    /// Commit `content` to `file` on `branch`, cut from `from` when the branch is new, and push.
    /// It returns the new head.
    pub fn commit(&self, branch: &str, from: &str, file: &str, content: &str) -> String {
        let exists = git_out(
            &self.work,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/remotes/origin/{branch}"),
            ],
        )
        .status
        .success();
        git(&self.work, &["fetch", "--quiet", "origin"]);
        let start = if exists { branch } else { from };
        git(
            &self.work,
            &[
                "checkout",
                "--quiet",
                "-B",
                branch,
                &format!("origin/{start}"),
            ],
        );
        std::fs::write(self.work.join(file), content).unwrap();
        git(&self.work, &["add", "-A"]);
        git(
            &self.work,
            &["commit", "--quiet", "-m", &format!("{branch}: {file}")],
        );
        git(&self.work, &["push", "--quiet", "origin", branch]);
        self.head(branch)
    }

    /// The tip of a branch in the bare repo.
    pub fn head(&self, branch: &str) -> String {
        git(&self.bare, &["rev-parse", &format!("refs/heads/{branch}")])
    }

    pub fn has_branch(&self, branch: &str) -> bool {
        git_out(
            &self.bare,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
        )
        .status
        .success()
    }

    /// The parents of a commit in the bare repo.
    pub fn parents(&self, commit: &str) -> Vec<String> {
        git(&self.bare, &["rev-list", "--parents", "-n", "1", commit])
            .split_whitespace()
            .skip(1)
            .map(str::to_string)
            .collect()
    }

    /// The text of `file` on `branch` in the bare repo.
    pub fn show(&self, branch: &str, file: &str) -> String {
        let out = git_out(
            &self.bare,
            &["show", &format!("refs/heads/{branch}:{file}")],
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    /// Every directory the forge left under its scratch directory.
    pub fn scratch_leftovers(&self) -> Vec<PathBuf> {
        let scratch = self.dir.path().join("scratch");
        std::fs::read_dir(scratch)
            .map(|rd| rd.filter_map(Result::ok).map(|e| e.path()).collect())
            .unwrap_or_default()
    }
}

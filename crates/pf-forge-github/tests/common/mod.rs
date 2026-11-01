//! A stub `gh` for the forge tests. No test reaches the network.
//!
//! The stub is a shell script in a temporary directory. It records its argv, one argument per
//! line and each call after a `--call` line, then answers from files in the same directory. A
//! call picks its answer by name: `pr view` reads `pr-view.*`, `pr merge` reads `pr-merge.*`
//! and `api` reads `api.*`. The files are `.out` (stdout), `.err` (stderr), `.rc` (exit code)
//! and `.sleep` (seconds to wait before the answer).
//!
//! The forge puts the stub directory first on the `PATH` of its own child process
//! (`GithubForge::with_path_prefix`), so tests that run in parallel never share a `PATH`.
#![allow(dead_code)]

use std::path::Path;

use pf_forge_github::GithubForge;
use serde_json::{Value, json};

pub const REPO: &str = "synthetic/repo";
pub const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
pub const MERGE_SHA: &str = "fedcba9876543210fedcba9876543210fedcba98";

pub struct Stub {
    pub dir: tempfile::TempDir,
}

impl Stub {
    pub fn new() -> Stub {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().display();
        let script = format!(
            r#"#!/bin/sh
d='{d}'
echo "--call" >> "$d/argv.log"
for a in "$@"; do printf '%s\n' "$a" >> "$d/argv.log"; done
if [ "$1" = pr ]; then key="pr-$2"; else key="$1"; fi
if [ -f "$d/$key.sleep" ]; then exec sleep "$(cat "$d/$key.sleep")"; fi
if [ -f "$d/$key.out" ]; then cat "$d/$key.out"; fi
if [ -f "$d/$key.err" ]; then cat "$d/$key.err" >&2; fi
if [ -f "$d/$key.rc" ]; then exit "$(cat "$d/$key.rc")"; fi
exit 0
"#
        );
        let gh = dir.path().join("gh");
        std::fs::write(&gh, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Stub { dir }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// A forge for `REPO` whose `gh` is this stub.
    pub fn forge(&self) -> GithubForge {
        GithubForge::new(REPO)
            .unwrap()
            .with_path_prefix(self.path())
    }

    /// Script one answer: stdout, stderr and the exit code.
    pub fn answer(&self, key: &str, out: &str, err: &str, rc: i32) {
        let p = |ext: &str| self.path().join(format!("{key}.{ext}"));
        std::fs::write(p("out"), out).unwrap();
        std::fs::write(p("err"), err).unwrap();
        std::fs::write(p("rc"), rc.to_string()).unwrap();
    }

    pub fn out(&self, key: &str, out: &str) {
        self.answer(key, out, "", 0);
    }

    pub fn sleep(&self, key: &str, seconds: u32) {
        std::fs::write(
            self.path().join(format!("{key}.sleep")),
            seconds.to_string(),
        )
        .unwrap();
    }

    /// Every call the stub got, each as its argument list.
    pub fn calls(&self) -> Vec<Vec<String>> {
        let text = std::fs::read_to_string(self.path().join("argv.log")).unwrap_or_default();
        let mut calls: Vec<Vec<String>> = Vec::new();
        for line in text.lines() {
            if line == "--call" {
                calls.push(Vec::new());
            } else if let Some(last) = calls.last_mut() {
                last.push(line.to_string());
            }
        }
        calls
    }

    /// The calls whose first two arguments are `a` and `b`.
    pub fn calls_of(&self, a: &str, b: &str) -> Vec<Vec<String>> {
        self.calls()
            .into_iter()
            .filter(|c| {
                c.first().map(String::as_str) == Some(a) && c.get(1).map(String::as_str) == Some(b)
            })
            .collect()
    }

    pub fn merge_calls(&self) -> Vec<Vec<String>> {
        self.calls_of("pr", "merge")
    }
}

/// The JSON of `gh pr view --json headRefName,headRefOid,baseRefName,mergeable,mergeStateStatus,statusCheckRollup`.
pub fn view_json(mergeable: &str, rollup: Vec<Value>) -> String {
    json!({
        "headRefName": "work/u1",
        "headRefOid": SHA,
        "baseRefName": "spine/r0",
        "mergeable": mergeable,
        "mergeStateStatus": "CLEAN",
        "statusCheckRollup": rollup,
    })
    .to_string()
}

/// A finished check run with a conclusion.
pub fn run(conclusion: &str) -> Value {
    json!({"__typename": "CheckRun", "name": "ci", "status": "COMPLETED", "conclusion": conclusion})
}

/// A check run in a state that is not finished.
pub fn running(status: &str) -> Value {
    json!({"__typename": "CheckRun", "name": "ci", "status": status, "conclusion": ""})
}

/// A commit status, as the rollup reports one.
pub fn context(state: &str) -> Value {
    json!({"__typename": "StatusContext", "context": "ci/legacy", "state": state})
}

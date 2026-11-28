//! A stub `gh` for the forge tests. No test reaches the network.
//!
//! The stub is a shell script in a temporary directory. It records its argv, one argument per
//! line and each call after a `--call` line, then answers from files in the same directory. A
//! call picks its answer by name: `pr view` reads `pr-view.*`, `pr merge` reads `pr-merge.*`,
//! `run rerun` reads `run.*` and `api` reads `api.*`, except an API call for the repo itself
//! (`api-repo.*`), a tree (`api-tree.*`) or a ref (`api-ref.*`). The files are `.out` (stdout),
//! `.err` (stderr), `.rc` (exit code) and `.sleep` (seconds to wait before the answer).
//!
//! A key can also hold a sequence: the nth call of a key reads `<key>.<n>.out`, `.err` and `.rc`
//! when any of them exists, and the plain key files otherwise.
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
if [ "$1" = api ]; then
  for a in "$@"; do
    case "$a" in
      repos/*/git/trees/*) key=api-tree ;;
      repos/*/git/ref/*) key=api-ref ;;
      repos/*/compare/*) key=api ;;
      repos/*) key=api-repo ;;
    esac
  done
fi
n=$(cat "$d/$key.n" 2>/dev/null || echo 0)
n=$((n + 1))
echo "$n" > "$d/$key.n"
if [ -f "$d/$key.$n.out" ] || [ -f "$d/$key.$n.rc" ] || [ -f "$d/$key.$n.err" ]; then key="$key.$n"; fi
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

    /// Script the nth call of a key (from 1): stdout, stderr and the exit code.
    pub fn answer_nth(&self, key: &str, n: u32, out: &str, err: &str, rc: i32) {
        self.answer(&format!("{key}.{n}"), out, err, rc);
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

    /// The calls of the `gh run rerun` kind.
    pub fn rerun_calls(&self) -> Vec<Vec<String>> {
        self.calls_of("run", "rerun")
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

/// An entry of the rollup: a finished GitHub Actions check run of this repo's workflow run `id`.
pub fn actions_run(name: &str, conclusion: &str, id: u64) -> Value {
    json!({
        "__typename": "CheckRun",
        "name": name,
        "status": "COMPLETED",
        "conclusion": conclusion,
        "detailsUrl": format!("https://github.com/{REPO}/actions/runs/{id}/job/9{id}"),
    })
}

/// A finished check run of another app, with a URL outside GitHub Actions.
pub fn other_app_run(name: &str, conclusion: &str) -> Value {
    json!({
        "__typename": "CheckRun",
        "name": name,
        "status": "COMPLETED",
        "conclusion": conclusion,
        "detailsUrl": "https://ci.example.invalid/builds/1",
    })
}

/// A failed commit status that an outside CI wrote.
pub fn failed_status(context: &str) -> Value {
    json!({"__typename": "StatusContext", "context": context, "state": "FAILURE"})
}

/// The JSON of `gh pr view --json headRefOid,statusCheckRollup`.
pub fn failed_view_json(head: &str, rollup: Vec<Value>) -> String {
    json!({"headRefOid": head, "statusCheckRollup": rollup}).to_string()
}

/// The JSON of `gh api repos/<repo> --jq .permissions`.
pub fn permissions(push: bool) -> String {
    json!({"admin": false, "maintain": false, "push": push, "pull": true}).to_string()
}

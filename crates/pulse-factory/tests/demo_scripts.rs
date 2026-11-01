//! AC-14: the two demo scripts run against the built binary, and each fails when its journey is
//! broken. The scripts run the paper env: no model, no spend, no network, no `gh`, and no build.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const LIMIT: Duration = Duration::from_secs(120);

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn script(name: &str) -> PathBuf {
    root().join("scripts/demo").join(name)
}

fn run(name: &str, env: &[(&str, &str)]) -> (Output, Duration) {
    let mut cmd = Command::new("bash");
    cmd.arg(script(name))
        .env("PF_BIN", env!("CARGO_BIN_EXE_pulse-factory"))
        .env_remove("PF_SEAT_TOKEN")
        .env_remove("PF_STATE_DIR")
        .env_remove("PF_SHARE_DIR");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let start = Instant::now();
    let out = cmd.output().expect("bash starts");
    (out, start.elapsed())
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// A copy of `scripts/demo/paper` that a test may break.
fn paper_copy() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let paper = dir.path().join("paper");
    copy_dir(&root().join("scripts/demo/paper"), &paper);
    (dir, paper)
}

fn edit(paper: &Path, rel: &str, f: impl FnOnce(String) -> String) {
    let path = paper.join(rel);
    let text = std::fs::read_to_string(&path).unwrap();
    let new = f(text.clone());
    assert_ne!(new, text, "the edit of {rel} changed nothing");
    std::fs::write(&path, new).unwrap();
}

#[test]
fn both_scripts_exit_0_within_120_seconds() {
    for name in ["golden-journey-a.sh", "arm-dry-run.sh"] {
        let (out, took) = run(name, &[]);
        assert!(
            out.status.success(),
            "{name}: {:?}\nstdout:\n{}\nstderr:\n{}",
            out.status,
            stdout(&out),
            stderr(&out)
        );
        assert!(took < LIMIT, "{name} took {took:?}");
    }
}

#[test]
fn the_golden_journey_reports_what_it_checked() {
    let (out, _) = run("golden-journey-a.sh", &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("every item merged"), "{text}");
    assert!(text.contains("total cost 9000"), "{text}");
}

#[test]
fn the_scripts_run_no_build_and_hold_no_real_forge_call() {
    for name in ["golden-journey-a.sh", "arm-dry-run.sh"] {
        let text = std::fs::read_to_string(script(name)).unwrap();
        for bad in [
            "cargo build",
            "cargo run",
            "cargo install",
            "gh pr",
            "gh api",
        ] {
            assert!(!text.contains(bad), "{name} holds {bad:?}");
        }
        assert!(text.contains("mktemp -d"), "{name} makes a temp directory");
        assert!(text.contains("PF_BIN"), "{name} takes PF_BIN");
    }
}

#[test]
fn a_seat_script_that_exits_non_zero_fails_the_golden_journey() {
    let (dir, paper) = paper_copy();
    edit(&paper, "seats/implementer.sh", |t| {
        t.replacen("set -eu\n", "set -eu\nexit 3\n", 1)
    });
    let (out, took) = run(
        "golden-journey-a.sh",
        &[("PAPER_SRC", paper.to_str().unwrap())],
    );
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(stderr(&out).contains("FAIL"), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("stuck") && stderr(&out).contains("no done signal"),
        "{}",
        stderr(&out)
    );
    assert!(took < LIMIT, "it fails at once, not at the limit: {took:?}");
    drop(dir);
}

#[test]
fn a_changed_usage_number_fails_the_golden_journey() {
    let (dir, paper) = paper_copy();
    edit(&paper, "seats/implementer.sh", |t| {
        t.replace("\"cost_micros\":2000", "\"cost_micros\":2001")
    });
    let (out, _) = run(
        "golden-journey-a.sh",
        &[("PAPER_SRC", paper.to_str().unwrap())],
    );
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(
        stderr(&out).contains("role implementer cost"),
        "{}",
        stderr(&out)
    );
    drop(dir);
}

#[test]
fn a_usage_reading_counted_twice_fails_the_total() {
    // A seat that writes its usage line twice has its cost counted twice: the run total is not
    // the fixed sum. (A role's cost fails first, so the message names the role.)
    let (dir, paper) = paper_copy();
    edit(&paper, "seats/verifier.sh", |t| {
        let line = t
            .lines()
            .find(|l| l.starts_with("echo '{\"usage\""))
            .unwrap()
            .to_string();
        t.replacen(&line, &format!("{line}\n{line}"), 1)
    });
    let (out, _) = run(
        "golden-journey-a.sh",
        &[("PAPER_SRC", paper.to_str().unwrap())],
    );
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(
        stderr(&out).contains("role verifier cost"),
        "{}",
        stderr(&out)
    );
    drop(dir);
}

#[test]
fn a_push_that_the_seat_does_not_report_fails_the_golden_journey() {
    let (dir, paper) = paper_copy();
    edit(&paper, "seats/close.sh", |t| {
        t.lines()
            .filter(|l| !(l.contains("push") && l.contains("PF_PAPER_EVENTS")))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    });
    let (out, _) = run(
        "golden-journey-a.sh",
        &[("PAPER_SRC", paper.to_str().unwrap())],
    );
    assert!(!out.status.success(), "{}", stdout(&out));
    drop(dir);
}

#[test]
fn arm_dry_run_with_a_failing_gh_exits_non_zero() {
    let dir = tempfile::tempdir().unwrap();
    let gh = dir.path().join("gh");
    std::fs::write(&gh, "#!/bin/sh\necho 'gh: boom' >&2\nexit 1\n").unwrap();
    let (out, _) = run("arm-dry-run.sh", &[("GH_STUB", gh.to_str().unwrap())]);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(
        stderr(&out).contains("arm refused") || stderr(&out).contains("arm --dry-run exited"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn arm_dry_run_fails_when_the_summary_changes() {
    // A workflow that differs from the shipped one in a cap: the script asserts the shipped numbers.
    let dir = tempfile::tempdir().unwrap();
    let wf = dir.path().join("ossify-spine");
    copy_dir(&root().join("workflows/ossify-spine"), &wf);
    let file = wf.join("workflow.yaml");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("model_sessions: 2"));
    std::fs::write(
        &file,
        text.replace("model_sessions: 2", "model_sessions: 3"),
    )
    .unwrap();
    let (out, _) = run("arm-dry-run.sh", &[("WORKFLOW_DIR", wf.to_str().unwrap())]);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(
        stderr(&out).contains("model_sessions 2"),
        "{}",
        stderr(&out)
    );
}

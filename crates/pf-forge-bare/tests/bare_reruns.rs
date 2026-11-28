//! AC-7 of `r2.s3.w3`: the bare forge runs the failed checks of a PR again, and reads the files of a
//! commit and the tip of a branch. A PR file of before reads as it did. `failed` and `rerun_error`
//! drive the answers, a run on the head appends one line to `prs/<n>.reruns` and leaves the checks
//! `pending`, and a moved head appends nothing.

mod common;

use common::*;
use pf_engine::port::forge::{Checks, ForgePort, PrRef};
use pf_forge_bare::{FailedSpec, PrChecks};

fn pr(f: &Fixture, number: u64) -> PrRef {
    PrRef {
        repo: f.repo(),
        number,
    }
}

fn spec(name: &str, rerunnable: bool) -> FailedSpec {
    FailedSpec {
        name: name.to_string(),
        rerunnable,
    }
}

fn lines_of(f: &Fixture, number: u64) -> Vec<String> {
    std::fs::read_to_string(f.forge.reruns_path(number))
        .map(|t| t.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

#[test]
fn a_pr_file_of_today_reads_as_before_and_red_checks_are_one_rerunnable_check() {
    let f = Fixture::new();
    let head = f.commit("work/u1", BASE, "a.txt", "one\n");
    // The file of today: no `failed`, no `rerun_error`.
    std::fs::create_dir_all(f.dir.path().join("prs")).unwrap();
    std::fs::write(
        f.dir.path().join("prs/1.json"),
        format!(r#"{{"head_branch":"work/u1","base":"{BASE}","checks":"red"}}"#),
    )
    .unwrap();
    let status = f.forge.pr_status(&pr(&f, 1)).unwrap();
    assert_eq!(status.checks, Checks::Red);
    let read = f.forge.failed_checks(&pr(&f, 1)).unwrap();
    assert_eq!(read.head_sha, head);
    assert_eq!(read.failed.len(), 1);
    assert_eq!(read.failed[0].name, "checks");
    assert!(read.failed[0].blocked.is_none());
    // A file written by `write_pr` holds no new key.
    f.forge
        .write_pr(2, "work/u1", BASE, PrChecks::Green)
        .unwrap();
    let text = std::fs::read_to_string(f.dir.path().join("prs/2.json")).unwrap();
    assert!(
        !text.contains("failed") && !text.contains("rerun_error"),
        "{text}"
    );
    // Green checks have no failed check.
    assert!(f.forge.failed_checks(&pr(&f, 2)).unwrap().failed.is_empty());
}

#[test]
fn failed_drives_the_answers_and_a_check_that_is_not_rerunnable_is_blocked() {
    let f = Fixture::new();
    f.commit("work/u1", BASE, "a.txt", "one\n");
    f.forge
        .write_pr_with(
            1,
            "work/u1",
            BASE,
            PrChecks::Red,
            &[spec("unit", true), spec("outside", false)],
            None,
        )
        .unwrap();
    let read = f.forge.failed_checks(&pr(&f, 1)).unwrap();
    let names: Vec<(&str, bool)> = read
        .failed
        .iter()
        .map(|c| (c.name.as_str(), c.blocked.is_some()))
        .collect();
    assert_eq!(names, [("unit", false), ("outside", true)]);
    // The run again runs the rerunnable one and names the other.
    let effect = f
        .forge
        .rerun_failed_checks(&pr(&f, 1), &read.head_sha)
        .unwrap();
    assert_eq!(effect.ran, ["unit"]);
    assert_eq!(effect.not_ran.len(), 1);
    assert_eq!(effect.not_ran[0].0, "outside");
    assert!(effect.head_moved.is_none());
}

#[test]
fn a_run_on_the_head_appends_one_line_and_reads_pending_after() {
    let f = Fixture::new();
    let head = f.commit("work/u1", BASE, "a.txt", "one\n");
    f.forge
        .write_pr_with(
            1,
            "work/u1",
            BASE,
            PrChecks::Red,
            &[spec("unit", true)],
            None,
        )
        .unwrap();
    let effect = f.forge.rerun_failed_checks(&pr(&f, 1), &head).unwrap();
    assert_eq!((effect.number, effect.sha.as_str()), (1, head.as_str()));
    assert_eq!(effect.ran, ["unit"]);
    assert_eq!(effect.repo, f.repo());
    let lines = lines_of(&f, 1);
    assert_eq!(lines.len(), 1, "{lines:?}");
    let line: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(line["sha"], head);
    assert_eq!(line["ran"], serde_json::json!(["unit"]));
    assert_eq!(
        f.forge.pr_status(&pr(&f, 1)).unwrap().checks,
        Checks::Pending
    );
    // The checks are not red any more: no failed check is left to run again.
    assert!(f.forge.failed_checks(&pr(&f, 1)).unwrap().failed.is_empty());
}

#[test]
fn a_moved_head_appends_nothing_and_names_the_head_it_found() {
    let f = Fixture::new();
    let old = f.commit("work/u1", BASE, "a.txt", "one\n");
    f.forge
        .write_pr_with(
            1,
            "work/u1",
            BASE,
            PrChecks::Red,
            &[spec("unit", true)],
            None,
        )
        .unwrap();
    let new = f.commit("work/u1", BASE, "a.txt", "two\n");
    assert_ne!(old, new);
    let effect = f.forge.rerun_failed_checks(&pr(&f, 1), &old).unwrap();
    assert_eq!(effect.head_moved.as_deref(), Some(new.as_str()));
    assert!(effect.ran.is_empty());
    assert!(lines_of(&f, 1).is_empty());
    assert!(!f.forge.reruns_path(1).exists());
    assert_eq!(f.forge.pr_status(&pr(&f, 1)).unwrap().checks, Checks::Red);
}

#[test]
fn rerun_error_makes_the_call_fail_and_a_foreign_repo_and_a_bad_sha_are_refused() {
    let f = Fixture::new();
    let head = f.commit("work/u1", BASE, "a.txt", "one\n");
    f.forge
        .write_pr_with(
            1,
            "work/u1",
            BASE,
            PrChecks::Red,
            &[spec("unit", true)],
            Some("the forge is down"),
        )
        .unwrap();
    let e = f.forge.rerun_failed_checks(&pr(&f, 1), &head).unwrap_err();
    assert!(e.0.contains("the forge is down"), "{e}");
    assert!(!f.forge.reruns_path(1).exists());
    let foreign = PrRef {
        repo: "elsewhere/repo".to_string(),
        number: 1,
    };
    assert!(f.forge.failed_checks(&foreign).is_err());
    assert!(f.forge.rerun_failed_checks(&foreign, &head).is_err());
    f.forge
        .write_pr_with(2, "work/u1", BASE, PrChecks::Red, &[], None)
        .unwrap();
    assert!(
        f.forge
            .rerun_failed_checks(&pr(&f, 2), "not-a-sha")
            .is_err()
    );
}

#[test]
fn tree_and_branch_tip_read_the_bare_repo() {
    let f = Fixture::new();
    f.commit("work/u1", BASE, "a.txt", "one\n");
    let head = f.commit("work/u1", BASE, "b.txt", "two\n");
    let tree = f.forge.tree(&f.repo(), &head).unwrap();
    let paths: Vec<&str> = tree.keys().map(String::as_str).collect();
    assert_eq!(paths, ["README", "a.txt", "b.txt"]);
    // The values are blob IDs: the same content gives the same ID.
    let blob = git(&f.bare, &["rev-parse", &format!("{head}:b.txt")]);
    assert_eq!(tree["b.txt"], blob);
    assert_eq!(f.forge.branch_tip(&f.repo(), "work/u1").unwrap(), head);
    assert_eq!(f.forge.branch_tip(&f.repo(), BASE).unwrap(), f.head(BASE));
    assert!(f.forge.branch_tip(&f.repo(), "missing").is_err());
    assert!(f.forge.branch_tip(&f.repo(), "--oops").is_err());
    assert!(f.forge.tree(&f.repo(), "nope").is_err());
    assert!(f.forge.tree("elsewhere/repo", &head).is_err());
}

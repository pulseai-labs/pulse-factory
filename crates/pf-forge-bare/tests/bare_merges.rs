//! AC-4: with a real bare repo, a green PR merges and the base holds the merge. A conflict and a
//! moved head each return their outcome, and the base does not move. A red and a pending PR report
//! as such.

mod common;

use common::*;
use pf_engine::port::forge::{Checks, ForgeOutcome, ForgePort, Mergeable, PrRef};
use pf_forge_bare::PrChecks;

fn pr(f: &Fixture, number: u64) -> PrRef {
    PrRef {
        repo: f.repo(),
        number,
    }
}

#[test]
fn a_green_pr_merges_and_the_base_holds_the_merge() {
    let f = Fixture::new();
    let before = f.head(BASE);
    let head = f.commit("work/u1", BASE, "a.txt", "from u1\n");
    f.forge
        .write_pr(1, "work/u1", BASE, PrChecks::Green)
        .unwrap();

    let status = f.forge.pr_status(&pr(&f, 1)).unwrap();
    assert_eq!(status.checks, Checks::Green);
    assert_eq!(status.mergeable, Mergeable::Clean);
    assert_eq!(status.sha, head);
    assert_eq!(status.branch, "work/u1");
    assert_eq!(status.base, BASE);
    assert_eq!(status.repo, f.repo());

    let effect = f.forge.merge(&pr(&f, 1), &head).unwrap();
    assert_eq!(effect.outcome, ForgeOutcome::Merged);
    assert_eq!(effect.repo, f.repo());
    assert_eq!(effect.branch, "work/u1");
    assert_eq!(effect.sha, head);

    // The base holds a merge commit with the old base and the PR head as parents.
    let after = f.head(BASE);
    assert_ne!(after, before);
    assert_eq!(
        f.parents(&after),
        vec![before, head.clone()],
        "a --no-ff merge"
    );
    assert_eq!(f.show(BASE, "a.txt"), "from u1\n");

    let landed = f.forge.merge_landed(&pr(&f, 1)).unwrap();
    assert_eq!(landed.outcome, ForgeOutcome::Landed);
    assert_eq!(landed.sha, head);
    assert_eq!(landed.branch, "work/u1");

    // Nothing deletes a branch, and the scratch clone is gone.
    assert!(f.has_branch("work/u1"));
    assert!(
        f.scratch_leftovers().is_empty(),
        "{:?}",
        f.scratch_leftovers()
    );
}

#[test]
fn a_pr_that_is_not_merged_has_not_landed() {
    let f = Fixture::new();
    f.commit("work/u1", BASE, "a.txt", "from u1\n");
    f.forge
        .write_pr(1, "work/u1", BASE, PrChecks::Green)
        .unwrap();
    let landed = f.forge.merge_landed(&pr(&f, 1)).unwrap();
    assert_eq!(landed.outcome, ForgeOutcome::NotLanded);
}

#[test]
fn a_conflict_is_read_before_the_merge_and_reported_by_it_and_the_base_does_not_move() {
    let f = Fixture::new();
    // The PR and the base change the same line.
    f.commit("work/u1", BASE, "README", "line one\nPR line\nline three\n");
    let head = f.head("work/u1");
    f.commit(BASE, BASE, "README", "line one\nbase line\nline three\n");
    let before = f.head(BASE);
    f.forge
        .write_pr(1, "work/u1", BASE, PrChecks::Green)
        .unwrap();

    let status = f.forge.pr_status(&pr(&f, 1)).unwrap();
    assert_eq!(status.mergeable, Mergeable::Conflict);

    let effect = f.forge.merge(&pr(&f, 1), &head).unwrap();
    assert_eq!(effect.outcome, ForgeOutcome::Conflict, "{effect:?}");
    assert_eq!(f.head(BASE), before, "the base does not move");
    assert_eq!(f.show(BASE, "README"), "line one\nbase line\nline three\n");
    assert!(
        f.scratch_leftovers().is_empty(),
        "{:?}",
        f.scratch_leftovers()
    );
    assert_eq!(
        f.forge.merge_landed(&pr(&f, 1)).unwrap().outcome,
        ForgeOutcome::NotLanded
    );
}

#[test]
fn a_moved_head_is_refused_and_the_base_does_not_move() {
    let f = Fixture::new();
    let read = f.commit("work/u1", BASE, "a.txt", "first\n");
    f.forge
        .write_pr(1, "work/u1", BASE, PrChecks::Green)
        .unwrap();
    let status = f.forge.pr_status(&pr(&f, 1)).unwrap();
    assert_eq!(status.sha, read);

    // The seat pushes again after the read and before the merge.
    let moved = f.commit("work/u1", BASE, "a.txt", "second\n");
    assert_ne!(moved, read);
    let before = f.head(BASE);

    let effect = f.forge.merge(&pr(&f, 1), &status.sha).unwrap();
    assert!(
        matches!(&effect.outcome, ForgeOutcome::Refused(why) if why.contains(&read) && why.contains(&moved)),
        "{effect:?}"
    );
    assert_eq!(f.head(BASE), before, "the base does not move");
    assert!(f.scratch_leftovers().is_empty());
}

#[test]
fn a_red_and_a_pending_pr_report_as_such() {
    let f = Fixture::new();
    f.commit("work/u1", BASE, "a.txt", "x\n");
    f.forge.write_pr(1, "work/u1", BASE, PrChecks::Red).unwrap();
    f.forge
        .write_pr(2, "work/u1", BASE, PrChecks::Pending)
        .unwrap();
    assert_eq!(f.forge.pr_status(&pr(&f, 1)).unwrap().checks, Checks::Red);
    assert_eq!(
        f.forge.pr_status(&pr(&f, 2)).unwrap().checks,
        Checks::Pending
    );
    // A test may also write the file by hand. The format is `{head_branch, base, checks}`.
    std::fs::write(
        f.dir.path().join("prs/3.json"),
        r#"{"head_branch":"work/u1","base":"spine","checks":"green"}"#,
    )
    .unwrap();
    assert_eq!(f.forge.pr_status(&pr(&f, 3)).unwrap().checks, Checks::Green);
}

#[test]
fn the_base_a_pr_names_is_the_base_it_reports() {
    // A PR aimed at another branch reports that branch, so the engine can see the difference.
    let f = Fixture::new();
    f.commit("work/u1", BASE, "a.txt", "x\n");
    git(
        &f.work,
        &[
            "push",
            "--quiet",
            "origin",
            &format!("{BASE}:refs/heads/main"),
        ],
    );
    f.forge
        .write_pr(1, "work/u1", "main", PrChecks::Green)
        .unwrap();
    assert_eq!(f.forge.pr_status(&pr(&f, 1)).unwrap().base, "main");
}

#[test]
fn a_pr_of_another_repo_is_refused_on_merge_and_an_error_on_a_read() {
    let f = Fixture::new();
    let head = f.commit("work/u1", BASE, "a.txt", "x\n");
    f.forge
        .write_pr(1, "work/u1", BASE, PrChecks::Green)
        .unwrap();
    let other = PrRef {
        repo: "someone/else".to_string(),
        number: 1,
    };
    let before = f.head(BASE);
    let effect = f.forge.merge(&other, &head).unwrap();
    assert!(
        matches!(effect.outcome, ForgeOutcome::Refused(_)),
        "{effect:?}"
    );
    assert_eq!(f.head(BASE), before);
    assert!(f.forge.pr_status(&other).is_err());
    assert!(f.forge.merge_landed(&other).is_err());
}

#[test]
fn an_unknown_pr_and_a_bad_pr_file_are_forge_errors() {
    let f = Fixture::new();
    let head = f.commit("work/u1", BASE, "a.txt", "x\n");
    assert!(f.forge.pr_status(&pr(&f, 99)).is_err());
    assert!(f.forge.merge(&pr(&f, 99), &head).is_err());

    let prs = f.dir.path().join("prs");
    std::fs::create_dir_all(&prs).unwrap();
    for (n, text) in [
        (1, "not json"),
        (
            2,
            r#"{"head_branch":"work/u1","base":"spine","checks":"purple"}"#,
        ),
        (
            3,
            r#"{"head_branch":"--upload-pack=x","base":"spine","checks":"green"}"#,
        ),
        (
            4,
            r#"{"head_branch":"work/u1","base":"spine","checks":"green","extra":1}"#,
        ),
        (
            5,
            r#"{"head_branch":"no-such-branch","base":"spine","checks":"green"}"#,
        ),
        (
            6,
            r#"{"head_branch":"work/u1","base":"no-such-base","checks":"green"}"#,
        ),
    ] {
        std::fs::write(prs.join(format!("{n}.json")), text).unwrap();
        assert!(f.forge.pr_status(&pr(&f, n)).is_err(), "PR {n}: {text}");
        assert!(f.forge.merge(&pr(&f, n), &head).is_err(), "PR {n}: {text}");
    }
    assert!(f.scratch_leftovers().is_empty());
}

#[test]
fn an_expected_sha_that_is_not_an_object_name_is_an_error() {
    let f = Fixture::new();
    f.commit("work/u1", BASE, "a.txt", "x\n");
    f.forge
        .write_pr(1, "work/u1", BASE, PrChecks::Green)
        .unwrap();
    let before = f.head(BASE);
    assert!(f.forge.merge(&pr(&f, 1), "--help").is_err());
    assert!(f.forge.merge(&pr(&f, 1), "").is_err());
    assert_eq!(f.head(BASE), before);
}

#[test]
fn a_head_that_is_already_in_the_base_merges_with_nothing_to_do() {
    let f = Fixture::new();
    let head = f.commit("work/u1", BASE, "a.txt", "x\n");
    f.forge
        .write_pr(1, "work/u1", BASE, PrChecks::Green)
        .unwrap();
    assert_eq!(
        f.forge.merge(&pr(&f, 1), &head).unwrap().outcome,
        ForgeOutcome::Merged
    );
    let after_first = f.head(BASE);
    // The same call again changes nothing and reports the same outcome.
    assert_eq!(
        f.forge.merge(&pr(&f, 1), &head).unwrap().outcome,
        ForgeOutcome::Merged
    );
    assert_eq!(f.head(BASE), after_first);
}

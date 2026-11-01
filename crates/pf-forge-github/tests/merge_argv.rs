//! AC-2: the stub `gh`'s recorded argv holds `--merge` and `--match-head-commit <sha>`. The test
//! fails on `--admin`, `--auto`, `--squash`, `--rebase` or `--delete-branch`. A `PrRef` for another
//! repo reaches no `gh` call and returns `Refused`.

mod common;

use common::*;
use pf_engine::port::forge::{ForgeOutcome, ForgePort, PrRef};

const FORBIDDEN: [&str; 5] = [
    "--admin",
    "--auto",
    "--squash",
    "--rebase",
    "--delete-branch",
];

fn pr() -> PrRef {
    PrRef {
        repo: REPO.to_string(),
        number: 7,
    }
}

#[test]
fn the_merge_argv_pins_the_head_and_asks_for_a_merge_commit() {
    let stub = Stub::new();
    stub.out("pr-merge", "");
    let effect = stub.forge().merge(&pr(), SHA).unwrap();
    assert_eq!(effect.outcome, ForgeOutcome::Merged);
    assert_eq!(effect.repo, REPO);
    assert_eq!(effect.sha, SHA);

    let merges = stub.merge_calls();
    assert_eq!(merges.len(), 1, "{:?}", stub.calls());
    assert_eq!(
        merges[0],
        [
            "pr",
            "merge",
            "7",
            "-R",
            REPO,
            "--merge",
            "--match-head-commit",
            SHA
        ]
    );
}

#[test]
fn the_merge_argv_holds_none_of_the_forbidden_flags() {
    let stub = Stub::new();
    stub.forge().merge(&pr(), SHA).unwrap();
    for call in stub.calls() {
        for flag in FORBIDDEN {
            assert!(
                !call
                    .iter()
                    .any(|a| a == flag || a.starts_with(&format!("{flag}="))),
                "{flag} in {call:?}"
            );
        }
    }
    // The flags that pin the head and pick the merge style are there.
    let merge = &stub.merge_calls()[0];
    assert!(merge.iter().any(|a| a == "--merge"), "{merge:?}");
    let at = merge
        .iter()
        .position(|a| a == "--match-head-commit")
        .unwrap();
    assert_eq!(merge[at + 1], SHA);
}

#[test]
fn a_pr_of_another_repo_reaches_no_gh_call_and_is_refused() {
    let stub = Stub::new();
    let other = PrRef {
        repo: "someone/else".to_string(),
        number: 7,
    };
    let effect = stub.forge().merge(&other, SHA).unwrap();
    assert!(
        matches!(effect.outcome, ForgeOutcome::Refused(_)),
        "{effect:?}"
    );
    assert!(stub.calls().is_empty(), "{:?}", stub.calls());
}

#[test]
fn a_repo_with_a_flag_or_a_path_in_it_cannot_build_a_forge() {
    for bad in [
        "-R/x",
        "owner/-x",
        "owner",
        "owner/name/extra",
        "a b/c",
        "",
        "../x",
    ] {
        assert!(
            pf_forge_github::GithubForge::new(bad).is_err(),
            "{bad:?} must be refused"
        );
    }
}

#[test]
fn an_expected_sha_that_is_not_a_full_object_name_is_an_error_with_no_call() {
    let stub = Stub::new();
    for bad in [
        "",
        "--admin",
        "abc",
        "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
    ] {
        assert!(stub.forge().merge(&pr(), bad).is_err(), "{bad:?}");
    }
    assert!(stub.calls().is_empty());
}

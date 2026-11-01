//! AC-3: a non-zero `gh` exit, malformed JSON and a call past its timeout each return `ForgeError`
//! with no second merge call. A token in stderr is absent from the error. A multi-line stderr on a
//! merge parks a real `Engine` unit with one merge call and a ledger line. Stderr that names a
//! moved head returns `Refused`. `merge_landed` reports `NotLanded` when the commit is not an
//! ancestor of the base.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use pf_adapter_fake::{FakeAdapter, Scenario, Step};
use pf_engine::capacity::Settings;
use pf_engine::merge::MergeStep;
use pf_engine::port::agent::RuleSet;
use pf_engine::port::forge::{ForgeOutcome, ForgePort, PrRef};
use pf_engine::port::store::Store;
use pf_engine::port::unit_source::{UnitRecord, UnitSource, UnitSourceError};
use pf_engine::unit::Signal;
use pf_engine::{Engine, EngineConfig, MergeRule, StageDef, Status, Workflow};
use pf_store_sqlite::SqliteStore;

fn pr() -> PrRef {
    PrRef {
        repo: REPO.to_string(),
        number: 7,
    }
}

/// A made-up token with the shape of a GitHub personal access token.
const TOKEN: &str = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";

// ---------------------------------------------------------------- a failed call is an error

#[test]
fn a_non_zero_exit_is_a_forge_error_and_the_merge_is_not_called_twice() {
    let stub = Stub::new();
    stub.answer(
        "pr-merge",
        "",
        "GraphQL: Pull request is not mergeable\n",
        1,
    );
    let err = stub.forge().merge(&pr(), SHA).unwrap_err();
    assert!(err.0.contains("not mergeable"), "{err}");
    assert!(err.0.contains("exit 1"), "{err}");
    assert_eq!(stub.merge_calls().len(), 1, "nothing retries a merge");
}

#[test]
fn a_non_zero_exit_on_a_read_is_a_forge_error() {
    let stub = Stub::new();
    stub.answer("pr-view", "", "could not resolve to a PullRequest\n", 1);
    assert!(stub.forge().pr_status(&pr()).is_err());
    stub.answer("pr-view", "", "boom\n", 4);
    assert!(stub.forge().merge_landed(&pr()).is_err());
}

#[test]
fn malformed_json_is_a_forge_error() {
    for text in [
        "",
        "not json",
        "{",
        "[]",
        "null",
        // Strict JSON: text after the value is an error.
        &format!("{} trailing", view_json("MERGEABLE", vec![run("SUCCESS")])),
        // A field with the wrong type.
        r#"{"headRefName":1,"headRefOid":"x","baseRefName":"b","mergeable":"MERGEABLE","statusCheckRollup":[]}"#,
        // A head that is not a full object name.
        r#"{"headRefName":"b","headRefOid":"--admin","baseRefName":"b","mergeable":"MERGEABLE","statusCheckRollup":[]}"#,
        // A mergeable value the forge does not know.
        &view_json("SORT_OF", vec![run("SUCCESS")]),
    ] {
        let stub = Stub::new();
        stub.out("pr-view", text);
        let err = stub.forge().pr_status(&pr());
        assert!(err.is_err(), "{text:?} must be a forge error: {err:?}");
    }
    let stub = Stub::new();
    stub.out("pr-view", "{\"state\":\"MERGED\"");
    assert!(stub.forge().merge_landed(&pr()).is_err());
}

#[test]
fn a_call_past_its_timeout_is_a_forge_error_and_the_merge_is_not_called_twice() {
    let stub = Stub::new();
    stub.sleep("pr-merge", 30);
    let forge = stub.forge().with_timeout(Duration::from_millis(300));
    let started = std::time::Instant::now();
    let err = forge.merge(&pr(), SHA).unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(err.0.contains("timed out"), "{err}");
    assert_eq!(stub.merge_calls().len(), 1, "nothing retries a merge");

    // A read that hangs is an error too.
    let stub = Stub::new();
    stub.sleep("pr-view", 30);
    let forge = stub.forge().with_timeout(Duration::from_millis(300));
    assert!(forge.pr_status(&pr()).unwrap_err().0.contains("timed out"));
}

#[test]
fn the_default_limit_is_sixty_seconds() {
    assert_eq!(pf_forge_github::DEFAULT_TIMEOUT, Duration::from_secs(60));
}

#[test]
fn gh_that_is_not_installed_is_a_forge_error() {
    let forge = pf_forge_github::GithubForge::new(REPO)
        .unwrap()
        .with_path_prefix("/nonexistent-dir-for-the-test");
    // Whether or not a real `gh` sits on the rest of the PATH, the call must not panic. This
    // forge never reaches the network: the program name is changed to one that cannot exist.
    let forge = forge.with_program("pf-no-such-gh-binary");
    assert!(forge.pr_status(&pr()).is_err());
}

// ---------------------------------------------------------------- a token never leaves

#[test]
fn a_token_in_stderr_is_absent_from_the_error() {
    let stub = Stub::new();
    stub.answer(
        "pr-merge",
        "",
        &format!("error connecting with token {TOKEN}\nAuthorization: Bearer {TOKEN}\n"),
        1,
    );
    let err = stub.forge().merge(&pr(), SHA).unwrap_err();
    assert!(!err.0.contains(TOKEN), "{err}");
    assert!(!err.0.contains("abcdefghijklmnop"), "{err}");

    stub.answer("pr-view", "", &format!("GH_TOKEN={TOKEN}\n"), 1);
    let err = stub.forge().pr_status(&pr()).unwrap_err();
    assert!(!err.0.contains(TOKEN), "{err}");
    assert!(!err.0.contains("abcdefghijklmnop"), "{err}");
}

// ---------------------------------------------------------------- a moved head is a refusal

#[test]
fn stderr_that_names_a_moved_head_is_a_refusal() {
    for words in [
        "X Pull request is not mergeable: the head branch was modified. Review the changes and try the merge again.",
        "GraphQL: Head branch was modified. Review and try the merge again. (mergePullRequest)",
        "expected head sha didn't match current head ref",
        "the Expected Head commit does not match",
    ] {
        let stub = Stub::new();
        stub.answer("pr-merge", "", &format!("{words}\n"), 1);
        let effect = stub.forge().merge(&pr(), SHA).unwrap();
        assert!(
            matches!(&effect.outcome, ForgeOutcome::Refused(why) if !why.contains('\n')),
            "{words:?} gave {effect:?}"
        );
        assert_eq!(effect.sha, SHA);
        assert_eq!(stub.merge_calls().len(), 1);
    }
}

#[test]
fn any_other_wording_falls_to_a_forge_error() {
    let stub = Stub::new();
    stub.answer("pr-merge", "", "the branch was updated by someone\n", 1);
    assert!(stub.forge().merge(&pr(), SHA).is_err());
}

// ---------------------------------------------------------------- landed or not

fn view_merged(state: &str, merge_commit: Option<&str>) -> String {
    let commit = merge_commit.map_or("null".to_string(), |oid| format!(r#"{{"oid":"{oid}"}}"#));
    format!(
        r#"{{"state":"{state}","mergeCommit":{commit},"headRefName":"work/u1","headRefOid":"{SHA}","baseRefName":"spine/r0"}}"#
    )
}

#[test]
fn merge_landed_reports_landed_when_the_commit_is_an_ancestor_of_the_base() {
    for status in ["ahead", "identical"] {
        let stub = Stub::new();
        stub.out("pr-view", &view_merged("MERGED", Some(MERGE_SHA)));
        stub.out("api", &format!(r#"{{"status":"{status}","ahead_by":3}}"#));
        let effect = stub.forge().merge_landed(&pr()).unwrap();
        assert_eq!(effect.outcome, ForgeOutcome::Landed, "{status}");
        assert_eq!(effect.repo, REPO);
        assert_eq!(effect.branch, "work/u1");
        assert_eq!(effect.sha, SHA);

        // The ancestor check is a GET through `gh api`, on the merge commit and the base.
        let api = stub.calls_of("api", "--method");
        assert_eq!(api.len(), 1, "{:?}", stub.calls());
        assert_eq!(
            api[0],
            [
                "api",
                "--method",
                "GET",
                &format!("repos/{REPO}/compare/{MERGE_SHA}...spine/r0")
            ]
        );
    }
}

#[test]
fn merge_landed_reports_not_landed_when_the_commit_is_not_an_ancestor_of_the_base() {
    for status in ["behind", "diverged"] {
        let stub = Stub::new();
        stub.out("pr-view", &view_merged("MERGED", Some(MERGE_SHA)));
        stub.out("api", &format!(r#"{{"status":"{status}"}}"#));
        let effect = stub.forge().merge_landed(&pr()).unwrap();
        assert_eq!(effect.outcome, ForgeOutcome::NotLanded, "{status}");
    }
}

#[test]
fn merge_landed_reports_not_landed_for_a_pr_that_is_not_merged() {
    for state in ["OPEN", "CLOSED"] {
        let stub = Stub::new();
        stub.out("pr-view", &view_merged(state, None));
        let effect = stub.forge().merge_landed(&pr()).unwrap();
        assert_eq!(effect.outcome, ForgeOutcome::NotLanded, "{state}");
        assert!(stub.calls_of("api", "--method").is_empty(), "{state}");
    }
}

#[test]
fn merge_landed_does_not_trust_unreadable_output() {
    // A merged PR without a merge commit, a merge commit that is not an object name, a base
    // that cannot sit in an API path, and an answer from `gh api` with no status.
    let cases: [(String, &str); 4] = [
        (view_merged("MERGED", None), r#"{"status":"ahead"}"#),
        (
            view_merged("MERGED", Some("--admin")),
            r#"{"status":"ahead"}"#,
        ),
        (
            view_merged("MERGED", Some(MERGE_SHA)).replace("spine/r0", "x/../y?z"),
            r#"{"status":"ahead"}"#,
        ),
        (view_merged("MERGED", Some(MERGE_SHA)), r#"{"ahead_by":1}"#),
    ];
    for (view, api) in cases {
        let stub = Stub::new();
        stub.out("pr-view", &view);
        stub.out("api", api);
        assert!(stub.forge().merge_landed(&pr()).is_err(), "{view} / {api}");
    }
}

// ---------------------------------------------------------------- a real Engine unit parks

struct OneUnit;
impl UnitSource for OneUnit {
    fn poll(&mut self) -> Result<Vec<UnitRecord>, UnitSourceError> {
        Ok(vec![UnitRecord::new("u1", "g1", 1, Default::default())])
    }
}

#[test]
fn a_multi_line_stderr_on_a_merge_parks_a_real_engine_unit_with_one_merge_call() {
    let stub = Stub::new();
    stub.out("pr-view", &view_json("MERGEABLE", vec![run("SUCCESS")]));
    stub.answer(
        "pr-merge",
        "",
        &format!(
            "X Pull request #7 is not mergeable\n\n  - a required review is missing\n\tdetails: token {TOKEN}\r\n  - try again later\n"
        ),
        1,
    );

    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(FakeAdapter::new());
    fake.script("u1", Scenario::Script(vec![Step::TurnEnd]));
    let mut stage = StageDef::new("build", "implementer", "model-a");
    stage.rules = RuleSet {
        name: "implementer".into(),
        prompts_off: true,
        deny: vec![],
    };
    let store = Arc::new(SqliteStore::open(dir.path().join("run.db")).unwrap());
    let cfg = EngineConfig {
        run_id: "run-1".into(),
        workflow: Workflow::single_track(
            "test-flow",
            vec![stage],
            Some(MergeRule::GreenChecksInDeclaredOrder),
            Settings::default(),
        ),
        ledger_path: dir.path().join("ledger.md"),
        state_dir: dir.path().join("state"),
        run_dir: dir.path().join("run"),
        params: Default::default(),
    };
    let engine = Engine::start(cfg, store.clone(), fake, Arc::new(stub.forge())).unwrap();
    engine.ingest(&mut OneUnit).unwrap();
    engine.start_stage("u1").unwrap();
    engine
        .signal(
            "u1",
            "build",
            Signal::PrState {
                state: "open".into(),
            },
            Some(pr()),
        )
        .unwrap();

    let step = engine.merge_next().unwrap();
    let MergeStep::Parked { unit, reason } = step else {
        panic!("expected a park, got {step:?}");
    };
    assert_eq!(unit, "u1");
    assert!(
        !reason.contains('\n') && !reason.contains('\r'),
        "{reason:?}"
    );
    assert!(reason.contains("not mergeable"), "{reason}");
    assert!(!reason.contains(TOKEN), "{reason}");

    let nodes = store.nodes("run-1").unwrap();
    let u1 = nodes.iter().find(|n| n.id == "u1").unwrap();
    assert_eq!(u1.status, Status::Parked);
    assert_eq!(u1.reason.as_deref(), Some(reason.as_str()));

    assert_eq!(stub.merge_calls().len(), 1, "one merge call");
    // The ledger holds one merge line, on one line, with repo, branch, SHA and the outcome.
    let ledger = std::fs::read_to_string(dir.path().join("ledger.md")).unwrap();
    let merge_lines: Vec<&str> = ledger
        .lines()
        .filter(|l| l.contains("merge of unit u1:"))
        .collect();
    assert_eq!(merge_lines.len(), 1, "{ledger}");
    let line = merge_lines[0];
    assert!(line.contains(&format!("repo={REPO}")), "{line}");
    assert!(line.contains("branch=work/u1"), "{line}");
    assert!(line.contains(&format!("sha={SHA}")), "{line}");
    assert!(line.contains("not mergeable"), "{line}");
    assert!(!ledger.contains(TOKEN), "{ledger}");
}

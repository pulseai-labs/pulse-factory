//! AC-6: the engine, `pf-store-sqlite`, the fake adapter and the bare forge run two units in the
//! declared order on real git. The first merges, its ledger line holds repo, branch, SHA and
//! outcome, then the second merges. A red second unit parks while the first stays merged.

mod common;

use std::sync::Arc;

use common::*;
use pf_adapter_fake::FakeAdapter;
use pf_engine::admission::PushOutcome;
use pf_engine::capacity::Settings;
use pf_engine::merge::MergeStep;
use pf_engine::port::agent::RuleSet;
use pf_engine::port::forge::{ForgeOutcome, PrRef};
use pf_engine::port::store::Store;
use pf_engine::port::unit_source::{UnitRecord, UnitSource, UnitSourceError};
use pf_engine::unit::Signal;
use pf_engine::workflow::{MergeDef, Track};
use pf_engine::{Engine, EngineConfig, MergeRule, StageDef, Status, Workflow};
use pf_forge_bare::PrChecks;
use pf_store_sqlite::SqliteStore;

struct Units;
impl UnitSource for Units {
    fn poll(&mut self) -> Result<Vec<UnitRecord>, UnitSourceError> {
        // The declared order: u1 first, u2 second, in one group.
        Ok(vec![
            UnitRecord::new("u2", "g1", 2, Default::default()),
            UnitRecord::new("u1", "g1", 1, Default::default()),
        ])
    }
}

struct Run {
    f: Fixture,
    engine: Engine,
    store: Arc<SqliteStore>,
    ledger: std::path::PathBuf,
    heads: [String; 2],
    _state: tempfile::TempDir,
}

/// Two units with real commits on real branches in the bare repo, a PR for each, an engine on the
/// SQLite store and the fake adapter, and the bare forge. Both units have finished their stage
/// and name their PR. The merge definition declares the bare repo, the spine branch and the head
/// check, and each unit's push is recorded.
fn start(second_checks: PrChecks) -> Run {
    let f = Fixture::new();
    let h1 = f.commit("work/u1", BASE, "u1.txt", "work of u1\n");
    let h2 = f.commit("work/u2", BASE, "u2.txt", "work of u2\n");
    f.forge
        .write_pr(1, "work/u1", BASE, PrChecks::Green)
        .unwrap();
    f.forge.write_pr(2, "work/u2", BASE, second_checks).unwrap();

    let state = tempfile::tempdir().unwrap();
    let mut stage = StageDef::new("build", "implementer", "model-a");
    stage.rules = RuleSet {
        name: "implementer".into(),
        prompts_off: true,
        deny: vec![],
    };
    let mut merge = MergeDef::new(MergeRule::GreenChecksInDeclaredOrder);
    merge.repo = f.repo();
    merge.base = BASE.to_string();
    merge.head_check = true;
    let workflow = Workflow {
        name: "paper-env".into(),
        tracks: vec![Track {
            name: "main".into(),
            stages: vec![stage],
            merge: Some(merge),
        }],
        settings: Settings::default(),
        roots: Vec::new(),
    };
    let store = Arc::new(SqliteStore::open(state.path().join("run.db")).unwrap());
    let ledger = state.path().join("ledger.md");
    let cfg = EngineConfig {
        run_id: "run-1".into(),
        workflow,
        ledger_path: ledger.clone(),
        state_dir: state.path().join("state"),
        run_dir: state.path().join("run"),
        params: Default::default(),
    };
    let engine = Engine::start(
        cfg,
        store.clone(),
        Arc::new(FakeAdapter::new()),
        Arc::new(f.forge.clone()),
    )
    .unwrap();
    engine.ingest(&mut Units).unwrap();
    for (n, (unit, head)) in [("u1", &h1), ("u2", &h2)].into_iter().enumerate() {
        engine.start_stage(unit).unwrap();
        engine
            .record_push(
                unit,
                &format!("push-{unit}"),
                &f.repo(),
                &format!("work/{unit}"),
                head,
                PushOutcome::Pushed,
            )
            .unwrap();
        engine
            .signal(
                unit,
                "build",
                Signal::PrState {
                    state: "open".into(),
                },
                Some(PrRef {
                    repo: f.repo(),
                    number: n as u64 + 1,
                }),
            )
            .unwrap();
    }
    Run {
        f,
        engine,
        store,
        ledger,
        heads: [h1, h2],
        _state: state,
    }
}

fn status_of(run: &Run, unit: &str) -> (Status, Option<String>) {
    let node = run
        .store
        .nodes("run-1")
        .unwrap()
        .into_iter()
        .find(|n| n.id == unit)
        .unwrap();
    (node.status, node.reason)
}

fn ledger(run: &Run) -> String {
    std::fs::read_to_string(&run.ledger).unwrap()
}

#[test]
fn two_units_merge_in_the_declared_order_on_real_git() {
    let run = start(PrChecks::Green);
    let base0 = run.f.head(BASE);

    // The first unit merges.
    let step = run.engine.merge_next().unwrap();
    let MergeStep::Merged { unit, effect } = step else {
        panic!("expected the first merge, got {step:?}");
    };
    assert_eq!(unit, "u1", "the declared order");
    assert_eq!(effect.outcome, ForgeOutcome::Landed);
    assert_eq!(effect.repo, run.f.repo());
    assert_eq!(effect.branch, "work/u1");
    assert_eq!(effect.sha, run.heads[0]);

    // Its ledger line holds repo, branch, SHA and outcome, and the second has not merged yet.
    let line = format!(
        "merge of unit u1: repo={} branch=work/u1 sha={} outcome=landed",
        run.f.repo(),
        run.heads[0]
    );
    assert!(ledger(&run).contains(&line), "{}", ledger(&run));
    assert!(!ledger(&run).contains("merge of unit u2:"));
    let after_first = run.f.head(BASE);
    assert_eq!(
        run.f.parents(&after_first),
        vec![base0, run.heads[0].clone()]
    );
    assert_eq!(run.f.show(BASE, "u1.txt"), "work of u1\n");
    assert_eq!(run.f.show(BASE, "u2.txt"), "", "u2 is not in the base yet");
    assert_eq!(
        status_of(&run, "u1"),
        (Status::Done, Some(format!("merged at {}", run.heads[0])))
    );

    // Then the second merges, on top of the first.
    let step = run.engine.merge_next().unwrap();
    let MergeStep::Merged { unit, effect } = step else {
        panic!("expected the second merge, got {step:?}");
    };
    assert_eq!(unit, "u2");
    assert_eq!(effect.sha, run.heads[1]);
    let line2 = format!(
        "merge of unit u2: repo={} branch=work/u2 sha={} outcome=landed",
        run.f.repo(),
        run.heads[1]
    );
    let text = ledger(&run);
    assert!(text.contains(&line2), "{text}");
    assert!(text.find(&line).unwrap() < text.find(&line2).unwrap());
    let after_second = run.f.head(BASE);
    assert_eq!(
        run.f.parents(&after_second),
        vec![after_first, run.heads[1].clone()]
    );
    assert_eq!(run.f.show(BASE, "u1.txt"), "work of u1\n");
    assert_eq!(run.f.show(BASE, "u2.txt"), "work of u2\n");
    assert_eq!(status_of(&run, "u2").0, Status::Done);

    assert!(matches!(run.engine.merge_next().unwrap(), MergeStep::Idle));
    // Nothing deleted a branch.
    assert!(run.f.has_branch("work/u1") && run.f.has_branch("work/u2"));
    assert!(run.f.scratch_leftovers().is_empty());
}

#[test]
fn a_red_second_unit_parks_while_the_first_stays_merged() {
    let run = start(PrChecks::Red);
    let step = run.engine.merge_next().unwrap();
    assert!(
        matches!(&step, MergeStep::Merged { unit, .. } if unit == "u1"),
        "{step:?}"
    );
    let after_first = run.f.head(BASE);

    let step = run.engine.merge_next().unwrap();
    assert_eq!(
        step,
        MergeStep::Parked {
            unit: "u2".to_string(),
            reason: "checks red".to_string()
        }
    );
    assert_eq!(
        status_of(&run, "u2"),
        (Status::Parked, Some("checks red".to_string()))
    );
    assert_eq!(
        status_of(&run, "u1").0,
        Status::Done,
        "the first stays merged"
    );

    // The base holds the first unit and not the second.
    assert_eq!(
        run.f.head(BASE),
        after_first,
        "the red PR did not move the base"
    );
    assert_eq!(run.f.show(BASE, "u1.txt"), "work of u1\n");
    assert_eq!(run.f.show(BASE, "u2.txt"), "");
    let text = ledger(&run);
    assert!(
        text.contains(&format!(
            "merge of unit u2: repo={} branch=work/u2 sha={} outcome=checks red",
            run.f.repo(),
            run.heads[1]
        )),
        "{text}"
    );
    // A parked unit holds the order: nothing merges after it.
    assert!(matches!(
        run.engine.merge_next().unwrap(),
        MergeStep::Waiting { unit, .. } if unit == "u2"
    ));
}

#[test]
fn a_pr_aimed_at_another_branch_parks_on_real_git() {
    // The same guard as the fake-forge test, on a real repo: a PR file that names `main` for an
    // item PR parks the unit, and the spine branch does not move.
    let run = start(PrChecks::Green);
    git(
        &run.f.work,
        &[
            "push",
            "--quiet",
            "origin",
            &format!("{BASE}:refs/heads/main"),
        ],
    );
    run.f
        .forge
        .write_pr(1, "work/u1", "main", PrChecks::Green)
        .unwrap();
    let before = run.f.head(BASE);
    let step = run.engine.merge_next().unwrap();
    let MergeStep::Parked { unit, reason } = step else {
        panic!("expected a park, got {step:?}");
    };
    assert_eq!(unit, "u1");
    assert!(reason.contains("main") && reason.contains(BASE), "{reason}");
    assert_eq!(run.f.head(BASE), before);
    assert_eq!(run.f.head("main"), before);
}

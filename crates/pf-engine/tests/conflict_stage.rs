//! AC-2 of `r2.s3.w3`: the conflict stage and the guarded compare (ADR-0004, ADR-0005, ADR-0006,
//! amended). A track's `merge.on_conflict` names a stage that only a merge conflict enters, and the
//! stage `then` that follows it. When the conflict stage is done, the engine compares the paths the
//! resolution changed with `merge.guarded`. A match, or a read that fails, parks the unit above the
//! line. Otherwise the unit goes on to `then`, so a resolution is reviewed before the merge rule runs.

mod common;

use std::collections::BTreeMap;

use common::*;
use pf_engine::claim::{Act, KEY_DECISION, SettleAsk, SettleFacts, acts_of, line_of, sha256_hex};
use pf_engine::merge::{
    LISTED_MAX, Listed, MergeStep, Resolution, glob_match, resolution_changes, stored_resolution,
    verdict,
};
use pf_engine::port::forge::{Mergeable, PrRef};
use pf_engine::port::store::{
    AdmissionOutcome, CONTROLLER_SCOPE, Commit, Effect, NewAdmission, NewClearGrant,
    ORCHESTRATOR_AGENT, RequestKind, RequestRow, Store,
};
use pf_engine::port::unit_source::UnitRecord;
use pf_engine::unit::Signal;
use pf_engine::workflow::OnConflict;
use pf_engine::{Cause, Command, CommandBody, EngineError, MergeRule, Status};
use serde_json::Value;

const FORGE: (&str, &str) = ("synthetic/repo", "main");
const HEAD: &str = "1111111111111111111111111111111111111111";
const NEW_HEAD: &str = "3333333333333333333333333333333333333333";
const BASE_TIP: &str = "4444444444444444444444444444444444444444";

fn tree(files: &[(&str, &str)]) -> BTreeMap<String, String> {
    files
        .iter()
        .map(|(p, b)| (p.to_string(), b.to_string()))
        .collect()
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

// ---------------------------------------------------------------------------- the pure compare

#[test]
fn a_path_that_only_took_the_base_change_is_not_changed() {
    let old = tree(&[("a", "1"), ("b", "1")]);
    let base = tree(&[("a", "2"), ("b", "1")]);
    let new = tree(&[("a", "2"), ("b", "1")]);
    assert!(resolution_changes(&old, &new, &base).is_empty());
}

#[test]
fn a_path_the_resolution_wrote_is_changed() {
    let old = tree(&[("a", "1")]);
    let base = tree(&[("a", "1")]);
    let new = tree(&[("a", "9")]);
    assert_eq!(resolution_changes(&old, &new, &base), strings(&["a"]));
}

#[test]
fn a_path_both_sides_changed_is_changed_and_a_deleted_path_is_changed() {
    let old = tree(&[("both", "1"), ("gone", "1"), ("kept", "1")]);
    let base = tree(&[("both", "2"), ("gone", "1"), ("kept", "1")]);
    let new = tree(&[("both", "3"), ("kept", "1")]);
    assert_eq!(
        resolution_changes(&old, &new, &base),
        strings(&["both", "gone"])
    );
    // A path the resolution adds is changed as well; one that the base deleted too is not.
    let old = tree(&[("x", "1")]);
    let base = tree(&[]);
    let new = tree(&[("y", "1")]);
    assert_eq!(resolution_changes(&old, &new, &base), strings(&["y"]));
}

#[test]
fn a_resolution_by_a_merge_commit_and_by_a_rebase_read_the_same_three_trees() {
    // A merge commit keeps the old head as an ancestor, a rebase does not: the compare reads trees
    // only, so both give the same answer for the same three trees.
    let old = tree(&[("src/a.rs", "1"), ("docs/x.md", "1")]);
    let base = tree(&[("src/a.rs", "2"), ("docs/x.md", "1")]);
    let merged = tree(&[("src/a.rs", "2"), ("docs/x.md", "7")]);
    let rebased = merged.clone();
    assert_eq!(
        resolution_changes(&old, &merged, &base),
        resolution_changes(&old, &rebased, &base)
    );
    assert_eq!(
        resolution_changes(&old, &merged, &base),
        strings(&["docs/x.md"])
    );
}

#[test]
fn the_glob_grammar_reads_star_double_star_and_question_mark() {
    assert!(glob_match("**/tests/**", "crates/a/tests/x.rs"));
    assert!(glob_match("**/tests/**", "tests/x.rs"));
    assert!(!glob_match("**/tests/**", "crates/a/src/x.rs"));
    assert!(glob_match("docs/adr/**", "docs/adr/adr-0001.md"));
    assert!(glob_match("docs/adr/**", "docs/adr/sub/a.md"));
    assert!(!glob_match("docs/adr/**", "docs/contracts/a.md"));
    assert!(glob_match("**/*.test.*", "web/src/app.test.ts"));
    assert!(glob_match("**/*.test.*", "app.test.ts"));
    assert!(!glob_match("**/*.test.*", "web/src/app.ts"));
    assert!(glob_match("**/*_test.*", "pkg/foo_test.go"));
    // `*` stays within one segment.
    assert!(glob_match("src/*.rs", "src/a.rs"));
    assert!(!glob_match("src/*.rs", "src/deep/a.rs"));
    // `**` matches none or many whole segments.
    assert!(glob_match("a/**/b", "a/b"));
    assert!(glob_match("a/**/b", "a/x/y/b"));
    assert!(!glob_match("a/**/b", "a/x/y/c"));
    // `?` is one character.
    assert!(glob_match("a?c", "abc"));
    assert!(!glob_match("a?c", "ac"));
    assert!(!glob_match("a?c", "abbc"));
    // Everything else is literal.
    assert!(glob_match("a.b", "a.b"));
    assert!(!glob_match("a.b", "axb"));
}

#[test]
fn the_verdict_cuts_each_list_at_twenty_and_keeps_the_count() {
    let paths: Vec<String> = (0..30).map(|i| format!("docs/{i}.md")).collect();
    match verdict(&paths, &strings(&["docs/**"])) {
        Resolution::Guarded { paths, globs } => {
            assert_eq!(paths.items.len(), LISTED_MAX);
            assert_eq!(paths.total, 30);
            assert_eq!(globs, Listed::cut(&strings(&["docs/**"])));
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        verdict(&strings(&["src/a.rs"]), &strings(&["docs/**"])),
        Resolution::Clean { .. }
    ));
}

// ---------------------------------------------------------------------------- a unit at its conflict stage

fn rig_workflow() -> pf_engine::Workflow {
    let mut wf = item_workflow(&["build", "review", "check"], true);
    let mut resolve = pf_engine::StageDef::new("resolve", "close-role", "model-a");
    resolve.entered_by_conflict = true;
    wf.tracks[0].stages.push(resolve);
    let def = wf.tracks[0].merge.as_mut().unwrap();
    def.rule = MergeRule::GreenChecksInDeclaredOrder;
    def.repo = "synthetic/repo".to_string();
    def.base = "spine".to_string();
    def.on_conflict = Some(OnConflict {
        stage: "resolve".to_string(),
        then: "review".to_string(),
    });
    def.guarded = strings(&["docs/**", "**/tests/**"]);
    wf
}

fn store_commit(env: &Env, id: &str, effects: Vec<Effect>) {
    let applied = env
        .store
        .commit(Commit {
            run_id: CONTROLLER_SCOPE.to_string(),
            command_id: id.to_string(),
            content: id.to_string(),
            admission: NewAdmission {
                kind: "probe".to_string(),
                outcome: AdmissionOutcome::Admitted,
                reason: None,
                unit_id: None,
                role: None,
                model: None,
                attempt: None,
                admitted_at: "t".to_string(),
            },
            effects,
            ledger: None,
        })
        .unwrap();
    assert!(matches!(
        applied,
        pf_engine::port::store::Committed::Applied { .. }
    ));
}

fn request_of(env: &Env, unit: &str) -> RequestRow {
    env.store
        .all_requests()
        .into_iter()
        .rfind(|r| r.unit_id.as_deref() == Some(unit))
        .unwrap()
}

fn block_of(request: &RequestRow) -> Value {
    let body: Value = serde_json::from_str(&request.body).unwrap();
    body[KEY_DECISION].clone()
}

fn pr() -> PrRef {
    PrRef {
        repo: "synthetic/repo".to_string(),
        number: 7,
    }
}

fn done(env: &Env, unit: &str, stage: &str) {
    env.engine
        .signal(
            unit,
            stage,
            Signal::DoneFile {
                path: "done".to_string(),
            },
            None,
        )
        .unwrap();
}

/// `u1` is parked at its merge with a conflict and settled with `resolve_conflict`, and its conflict
/// stage `resolve` is running. The settle stored the head `HEAD`.
fn at_the_conflict_stage() -> Env {
    conflict_stage_by(true)
}

/// The same, but the claim sent the unit to the conflict stage with a clear: no settle stored a head.
fn conflict_stage_by(settled: bool) -> Env {
    let env = env_with_params(rig_workflow(), BTreeMap::new());
    env.engine
        .ingest(&mut TrackedSource {
            track: "item".to_string(),
            records: vec![UnitRecord::new("u1", "g1", 0, BTreeMap::new())],
        })
        .unwrap();
    env.store.add_project("P-1");
    store_commit(
        &env,
        "grant",
        vec![Effect::GrantClear(NewClearGrant {
            project_id: "P-1".to_string(),
            generation: 1,
            agent_id: ORCHESTRATOR_AGENT.to_string(),
            granted_by: "cli".to_string(),
            granted_at: "t".to_string(),
        })],
    );
    store_commit(
        &env,
        "bound",
        vec![Effect::SetRunLaunchBound {
            run_id: "run-1".to_string(),
            bound: 20,
        }],
    );
    for stage in ["build", "review", "check"] {
        env.engine.start_stage("u1").unwrap();
        if stage == "check" {
            env.engine
                .signal(
                    "u1",
                    stage,
                    Signal::PrState {
                        state: "open".to_string(),
                    },
                    Some(pr()),
                )
                .unwrap();
        } else {
            done(&env, "u1", stage);
        }
    }
    let mut script = PrScript::green("work/u1", HEAD);
    script.mergeable = Mergeable::Conflict;
    env.forge.script(7, script);
    assert!(matches!(
        env.engine.merge_next().unwrap(),
        MergeStep::Parked { .. }
    ));
    let request = request_of(&env, "u1");
    if !settled {
        let generation = env
            .engine
            .claim("op@host", &sha256_hex(b"a token"))
            .unwrap()
            .generation;
        env.engine
            .clear_unit("u1", Some("resolve"), generation, &request.id)
            .unwrap();
    } else {
        env.engine
            .settle(
                SettleAsk {
                    request_id: request.id.clone(),
                    revision: request.revision,
                    act: Act::ResolveConflict,
                    to_stage: None,
                    facts: SettleFacts {
                        reason: "send it to the conflict stage".to_string(),
                        session: "S-1".to_string(),
                        project: "P-1".to_string(),
                        generation: 1,
                        agent: ORCHESTRATOR_AGENT.to_string(),
                        ..SettleFacts::default()
                    },
                },
                Some(FORGE),
            )
            .unwrap();
    }
    env.engine.start_stage("u1").unwrap();
    // The seat merges the base into the branch and pushes: the PR's head changes.
    env.forge.scripts.lock().unwrap().get_mut(&7).unwrap().sha = NEW_HEAD.to_string();
    env.forge
        .scripts
        .lock()
        .unwrap()
        .get_mut(&7)
        .unwrap()
        .mergeable = Mergeable::Clean;
    env.forge.script_tip("spine", BASE_TIP);
    env.forge
        .script_tree(HEAD, &[("src/a.rs", "1"), ("docs/x.md", "1")]);
    env.forge
        .script_tree(BASE_TIP, &[("src/a.rs", "2"), ("docs/x.md", "1")]);
    env
}

fn end_signal() -> Signal {
    Signal::DoneFile {
        path: "resolve-done".to_string(),
    }
}

#[test]
fn a_clean_end_moves_the_unit_to_then_and_every_later_stage_runs_before_the_merge() {
    let env = at_the_conflict_stage();
    // The resolution took the base's change only: nothing it wrote is guarded.
    env.forge
        .script_tree(NEW_HEAD, &[("src/a.rs", "2"), ("docs/x.md", "1")]);
    let admission = env
        .engine
        .conflict_done("u1", "resolve", end_signal(), None)
        .unwrap();
    assert!(!admission.replayed);
    let stored = stored_resolution(&admission.row.content).unwrap();
    assert!(matches!(stored, Resolution::Clean { .. }), "{stored:?}");
    let view = env.engine.view().unwrap();
    let detail = view.unit_detail(view.node("u1").unwrap()).unwrap();
    // `review` is the place 1 of the track: the unit never goes from `resolve` to its merge.
    assert_eq!(detail.stages_done, 1);
    assert_eq!(
        env.store.node("run-1", "u1").unwrap().status,
        Status::Running
    );
    assert_eq!(
        env.store.node("run-1", "u1/resolve#1").unwrap().status,
        Status::Done
    );
    assert!(matches!(
        env.engine.merge_next().unwrap(),
        MergeStep::Waiting { .. }
    ));
    for stage in ["review", "check"] {
        env.engine.start_stage("u1").unwrap();
        assert!(
            env.store
                .admission("run-1", &format!("launch:u1:{stage}:2"))
                .unwrap()
                .is_some(),
            "{stage} launches before the merge"
        );
        assert!(matches!(
            env.engine.merge_next().unwrap(),
            MergeStep::Waiting { .. }
        ));
        done(&env, "u1", stage);
    }
    // Every stage of the sequence is done: now the merge rule runs.
    let status = env.engine.merge_next().unwrap();
    assert!(
        !matches!(status, MergeStep::Waiting { .. }),
        "the merge rule runs after the later stages: {status:?}"
    );
}

#[test]
fn a_guarded_end_parks_the_unit_above_the_line_in_the_same_commit() {
    let env = at_the_conflict_stage();
    env.forge
        .script_tree(NEW_HEAD, &[("src/a.rs", "2"), ("docs/x.md", "9")]);
    let before = env.store.admissions("run-1").unwrap().len();
    let admission = env
        .engine
        .conflict_done("u1", "resolve", end_signal(), None)
        .unwrap();
    // One admission carries the stage's end, the verdict and the park.
    assert_eq!(env.store.admissions("run-1").unwrap().len(), before + 1);
    assert_eq!(admission.row.command_id, "signal:u1/resolve#1");
    assert!(matches!(
        stored_resolution(&admission.row.content),
        Some(Resolution::Guarded { .. })
    ));
    assert_eq!(
        env.store.node("run-1", "u1").unwrap().status,
        Status::Parked
    );
    assert_eq!(
        env.store.node("run-1", "u1/resolve#1").unwrap().status,
        Status::Done
    );
    let request = request_of(&env, "u1");
    assert_eq!(request.kind, RequestKind::Park);
    assert_eq!(request.stage, "review");
    let block = block_of(&request);
    assert_eq!(block["cause"], "resolution_changed_guarded");
    let body: Value = serde_json::from_str(&request.body).unwrap();
    assert_eq!(body["paths"], serde_json::json!(["docs/x.md"]));
    assert_eq!(body["globs"], serde_json::json!(["docs/**"]));
    // The class is above for any merge, and the grant has no act for it.
    let view = env.engine.view().unwrap();
    assert_eq!(
        env.engine.request_line(&view, &request, Some(FORGE)),
        pf_engine::claim::Line::Above
    );
    // The claim's clear resumes at `then`.
    let generation = env
        .engine
        .claim("op@host", &sha256_hex(b"a token"))
        .unwrap()
        .generation;
    env.engine
        .clear_unit("u1", None, generation, &request.id)
        .unwrap();
    let view = env.engine.view().unwrap();
    assert_eq!(
        view.unit_detail(view.node("u1").unwrap())
            .unwrap()
            .stages_done,
        1
    );
}

#[test]
fn a_read_that_fails_parks_the_unit_too() {
    // The tree of the new head cannot be read.
    let env = at_the_conflict_stage();
    env.forge
        .tree_errors
        .lock()
        .unwrap()
        .insert(NEW_HEAD.to_string());
    let admission = env
        .engine
        .conflict_done("u1", "resolve", end_signal(), None)
        .unwrap();
    match stored_resolution(&admission.row.content) {
        Some(Resolution::Unread { why }) => assert!(why.contains("cannot be read"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        env.store.node("run-1", "u1").unwrap().status,
        Status::Parked
    );
    let request = request_of(&env, "u1");
    assert_eq!(block_of(&request)["cause"], "resolution_changed_guarded");
    assert_eq!(request.stage, "review");
    let body: Value = serde_json::from_str(&request.body).unwrap();
    assert!(body["why"].as_str().unwrap().contains("cannot be read"));

    // The tip of the base cannot be read.
    let env = at_the_conflict_stage();
    env.forge.tips.lock().unwrap().clear();
    env.engine
        .conflict_done("u1", "resolve", end_signal(), None)
        .unwrap();
    assert_eq!(
        env.store.node("run-1", "u1").unwrap().status,
        Status::Parked
    );
    assert_eq!(
        block_of(&request_of(&env, "u1"))["cause"],
        "resolution_changed_guarded"
    );

    // The PR's head cannot be read.
    let env = at_the_conflict_stage();
    env.forge.scripts.lock().unwrap().remove(&7);
    env.engine
        .conflict_done("u1", "resolve", end_signal(), None)
        .unwrap();
    assert_eq!(
        env.store.node("run-1", "u1").unwrap().status,
        Status::Parked
    );
}

#[test]
fn conflict_done_reads_the_stored_head_and_a_missing_one_is_unread() {
    let env = at_the_conflict_stage();
    // The compare reads `HEAD`, stored by the settle, as the old tree: only that tree holds
    // `docs/x.md` as `1`. Scripting no tree for any other SHA proves which one it read.
    env.forge
        .script_tree(NEW_HEAD, &[("src/a.rs", "2"), ("docs/x.md", "1")]);
    env.forge.trees.lock().unwrap().remove(HEAD);
    let admission = env
        .engine
        .conflict_done("u1", "resolve", end_signal(), None)
        .unwrap();
    match stored_resolution(&admission.row.content) {
        Some(Resolution::Unread { why }) => assert!(why.contains(HEAD), "{why}"),
        other => panic!("{other:?}"),
    }

    // A unit that the claim sent to the conflict stage has no stored head: the compare is unread.
    let env = conflict_stage_by(false);
    env.forge
        .script_tree(NEW_HEAD, &[("src/a.rs", "2"), ("docs/x.md", "1")]);
    let admission = env
        .engine
        .conflict_done("u1", "resolve", end_signal(), None)
        .unwrap();
    assert_eq!(
        stored_resolution(&admission.row.content),
        Some(Resolution::Unread {
            why: "no recorded head".to_string()
        })
    );
    assert_eq!(
        env.store.node("run-1", "u1").unwrap().status,
        Status::Parked
    );

    // A failed signal fails the unit, as on every stage.
    let env = at_the_conflict_stage();
    env.engine
        .signal("u1", "resolve", Signal::ExitCode { code: 1 }, None)
        .unwrap();
    assert_eq!(
        env.store.node("run-1", "u1").unwrap().status,
        Status::Failed
    );
}

#[test]
fn a_signal_with_no_resolution_and_an_outcome_are_refused_at_the_conflict_stage() {
    let env = at_the_conflict_stage();
    let plain = env.engine.signal("u1", "resolve", end_signal(), None);
    match plain {
        Err(EngineError::Rejected(why)) => {
            assert!(why.contains("needs the guarded compare"), "{why}")
        }
        other => panic!("{other:?}"),
    }
    let outcome = env.engine.outcome("u1", "resolve", 0, end_signal(), None);
    assert!(
        matches!(&outcome, Err(EngineError::Rejected(_))),
        "{outcome:?}"
    );
    // A submitted `Signal` that carries no verdict is refused too.
    let direct = env.engine.submit(Command {
        id: "signal:u1/resolve#1".to_string(),
        body: CommandBody::Signal {
            unit_id: "u1".to_string(),
            stage: "resolve".to_string(),
            signal: end_signal(),
            pr: None,
            resolution: None,
        },
    });
    assert!(
        matches!(direct, Err(EngineError::Rejected(_))),
        "{direct:?}"
    );
    // Nothing advanced: the stage still runs.
    assert_eq!(
        env.store.node("run-1", "u1/resolve#1").unwrap().status,
        Status::Running
    );
}

// ---------------------------------------------------------------------------- the new cause

#[test]
fn the_new_cause_is_above_admits_no_act_and_offers_resume_with_no_recommendation() {
    assert_eq!(Cause::ALL.len(), 23);
    assert_eq!(
        Cause::parse("resolution_changed_guarded"),
        Some(Cause::ResolutionChangedGuarded)
    );
    for free in [true, false] {
        assert_eq!(
            line_of(Some(Cause::ResolutionChangedGuarded), free),
            pf_engine::claim::Line::Above
        );
    }
    assert!(acts_of(Cause::ResolutionChangedGuarded).is_empty());
    let env = at_the_conflict_stage();
    env.forge.script_tree(NEW_HEAD, &[("docs/x.md", "9")]);
    env.engine
        .conflict_done("u1", "resolve", end_signal(), None)
        .unwrap();
    let block = block_of(&request_of(&env, "u1"));
    let options = block["options"].as_array().unwrap();
    assert_eq!(options.len(), 1);
    assert_eq!(options[0]["id"], "resume");
    assert_eq!(
        options[0]["detail"],
        "Resume the unit at the stage it parked in."
    );
    assert!(block["recommended"].is_null(), "{block}");
}

//! AC-1 of `r2.s3.w1`: a request has a line class, `below` or `above`, that the engine computes
//! from the request's cause and its track's merge (`docs/contracts/requests.md`, "The line"; ADR-0006,
//! amended). `line_of` is one exhaustive table over the causes. `Engine::request_line` reads the
//! cause from the stored block, the track from the unit, and the merge from the engine's own
//! workflow. Every read that fails gives `above`, and the class is never stored.

mod common;

use std::collections::BTreeMap;

use common::*;
use pf_engine::claim::{Line, line_of};
use pf_engine::port::store::{
    AdmissionOutcome, CONTROLLER_SCOPE, Cause, Commit, Committed, Effect, NewAdmission,
    NewClearGrant, ORCHESTRATOR_AGENT, RequestKind, RequestRow, Store,
};
use pf_engine::port::unit_source::UnitRecord;
use pf_engine::{MergeRule, Workflow};
use serde_json::{Value, json};

/// The forge's repo and default branch, as the controller passes them.
const FORGE: (&str, &str) = ("synthetic/repo", "main");

/// The kind a cause's park opens with in code.
fn kind_of(cause: Cause) -> RequestKind {
    match cause {
        Cause::StopFile => RequestKind::Stop,
        Cause::Stalled => RequestKind::Stall,
        Cause::SeatLost | Cause::HostUnreachable => RequestKind::SeatLost,
        Cause::MergeConflict | Cause::MergeChecksRed | Cause::MergeRefused | Cause::MergeError => {
            RequestKind::MergeFailed
        }
        _ => RequestKind::Park,
    }
}

fn facts_of(cause: Cause) -> Option<Value> {
    match cause {
        Cause::StopFile => Some(json!({
            "stop_text": "stop: pick one",
            "stop_options": [
                { "text": "go on", "recommended": false },
                { "text": "stop here", "recommended": true },
            ],
        })),
        _ => None,
    }
}

/// What a test says about one unit: its ID and its variables.
type UnitSpec<'a> = (&'a str, &'a [(&'a str, &'a str)]);

/// A workflow with the track `item`, one stage `build` and a merge definition.
fn merge_workflow(rule: MergeRule, repo: &str, base: &str, head_branch: &str) -> Workflow {
    let mut wf = item_workflow(&["build"], true);
    let def = wf.tracks[0].merge.as_mut().unwrap();
    def.rule = rule;
    def.repo = repo.to_string();
    def.base = base.to_string();
    def.head_branch = head_branch.to_string();
    wf
}

/// An engine over `FakeStore` with these units on the track `item`.
fn engine_with(wf: Workflow, params: &[(&str, &str)], units: &[UnitSpec]) -> Env {
    let params: BTreeMap<String, String> = params
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    let env = env_with_params(wf, params);
    let records = units
        .iter()
        .enumerate()
        .map(|(i, (id, vars))| {
            let vars = vars
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect();
            UnitRecord::new(*id, "g1", u32::try_from(i).unwrap(), vars)
        })
        .collect();
    env.engine
        .ingest(&mut TrackedSource {
            track: "item".to_string(),
            records,
        })
        .unwrap();
    env
}

/// Park `unit` with `cause`, and return the request the park opened.
fn park(env: &Env, unit: &str, cause: Cause) -> RequestRow {
    env.engine.start_stage(unit).unwrap();
    env.engine
        .park_with(
            unit,
            kind_of(cause),
            cause,
            "parked",
            facts_of(cause).as_ref(),
        )
        .unwrap();
    env.engine
        .requests()
        .unwrap()
        .into_iter()
        .find(|r| r.unit_id.as_deref() == Some(unit))
        .unwrap_or_else(|| panic!("no request for {unit}"))
}

fn line(env: &Env, request: &RequestRow, forge: Option<(&str, &str)>) -> Line {
    let view = env.engine.view().unwrap();
    env.engine.request_line(&view, request, forge)
}

/// The class of a red check and of a conflict on the one unit `u`, each on a track with this merge.
fn merge_lines(
    wf: Workflow,
    params: &[(&str, &str)],
    vars: &[(&str, &str)],
    forge: Option<(&str, &str)>,
) -> (Line, Line) {
    let env = engine_with(wf, params, &[("red", vars), ("conflict", vars)]);
    let red = park(&env, "red", Cause::MergeChecksRed);
    let conflict = park(&env, "conflict", Cause::MergeConflict);
    (line(&env, &red, forge), line(&env, &conflict, forge))
}

/// A merge on a track that needs no ack, into `base` of `repo`, as the forge's default is `FORGE`.
fn free(base: &str) -> Workflow {
    merge_workflow(
        MergeRule::GreenChecksInDeclaredOrder,
        "synthetic/repo",
        base,
        "",
    )
}

fn both(lines: (Line, Line), expected: Line, why: &str) {
    assert_eq!(lines.0, expected, "a red check: {why}");
    assert_eq!(lines.1, expected, "a conflict: {why}");
}

// ---------------------------------------------------------------- the table

fn expected(cause: Cause, free_merge: bool) -> Line {
    match cause {
        Cause::Stalled | Cause::SeatLost | Cause::HostUnreachable => Line::Below,
        Cause::MergeChecksRed | Cause::MergeConflict if free_merge => Line::Below,
        _ => Line::Above,
    }
}

#[test]
fn line_of_gives_the_row_of_the_table_for_every_cause_and_both_values_of_free_merge() {
    for cause in Cause::ALL {
        for free_merge in [false, true] {
            assert_eq!(
                line_of(Some(cause), free_merge),
                expected(cause, free_merge),
                "{cause:?}, free_merge {free_merge}"
            );
        }
    }
}

#[test]
fn exactly_five_causes_are_ever_below_and_launch_failed_is_not_one() {
    let below: Vec<Cause> = Cause::ALL
        .into_iter()
        .filter(|c| line_of(Some(*c), true) == Line::Below)
        .collect();
    assert_eq!(
        below,
        vec![
            Cause::Stalled,
            Cause::MergeConflict,
            Cause::MergeChecksRed,
            Cause::SeatLost,
            Cause::HostUnreachable,
        ]
    );
    assert_eq!(line_of(Some(Cause::LaunchFailed), true), Line::Above);
    // With no free merge, three of them stay.
    let always: Vec<Cause> = Cause::ALL
        .into_iter()
        .filter(|c| line_of(Some(*c), false) == Line::Below)
        .collect();
    assert_eq!(
        always,
        vec![Cause::Stalled, Cause::SeatLost, Cause::HostUnreachable]
    );
}

#[test]
fn no_cause_gives_above_and_the_class_names_itself() {
    assert_eq!(line_of(None, true), Line::Above);
    assert_eq!(line_of(None, false), Line::Above);
    assert_eq!(Line::Below.as_str(), "below");
    assert_eq!(Line::Above.as_str(), "above");
}

// ---------------------------------------------------------------- a request

#[test]
fn a_stall_is_below_whatever_the_track_and_the_forge() {
    // A track with no merge at all, and no forge given.
    let env = engine_with(item_workflow(&["build"], false), &[], &[("u1", &[])]);
    let request = park(&env, "u1", Cause::Stalled);
    assert_eq!(line(&env, &request, None), Line::Below);
    assert_eq!(line(&env, &request, Some(FORGE)), Line::Below);
    // So are a lost seat and a host that does not answer.
    let env = engine_with(
        item_workflow(&["build"], false),
        &[],
        &[("u1", &[]), ("u2", &[])],
    );
    let lost = park(&env, "u1", Cause::SeatLost);
    let silent = park(&env, "u2", Cause::HostUnreachable);
    assert_eq!(line(&env, &lost, None), Line::Below);
    assert_eq!(line(&env, &silent, None), Line::Below);
}

#[test]
fn a_red_check_and_a_conflict_are_below_on_a_track_that_needs_no_ack_and_merges_elsewhere() {
    both(
        merge_lines(free("feature/x"), &[], &[], Some(FORGE)),
        Line::Below,
        "the base is not the default branch",
    );
}

#[test]
fn a_red_check_and_a_conflict_are_above_on_a_claim_ack_track() {
    let wf = merge_workflow(MergeRule::ClaimAck, "synthetic/repo", "feature/x", "");
    both(
        merge_lines(wf, &[], &[], Some(FORGE)),
        Line::Above,
        "the track needs the claim's ack",
    );
}

#[test]
fn a_red_check_and_a_conflict_are_above_when_the_base_is_the_default_branch() {
    both(
        merge_lines(free("main"), &[], &[], Some(FORGE)),
        Line::Above,
        "the base is the default branch",
    );
}

#[test]
fn a_red_check_and_a_conflict_are_above_when_no_forge_is_given() {
    both(
        merge_lines(free("feature/x"), &[], &[], None),
        Line::Above,
        "no forge repo and no default branch",
    );
}

#[test]
fn a_red_check_and_a_conflict_are_above_when_the_base_does_not_render() {
    both(
        merge_lines(free("{param.nothing}"), &[], &[], Some(FORGE)),
        Line::Above,
        "the base names a parameter that is not there",
    );
}

#[test]
fn the_other_causes_are_above() {
    let env = engine_with(
        item_workflow(&["build"], false),
        &[],
        &[("stop", &[]), ("launch", &[])],
    );
    let stop = park(&env, "stop", Cause::StopFile);
    let launch = park(&env, "launch", Cause::LaunchFailed);
    assert_eq!(line(&env, &stop, Some(FORGE)), Line::Above, "a stop file");
    assert_eq!(
        line(&env, &launch, Some(FORGE)),
        Line::Above,
        "a failed launch is below only once an adapter marks it transient"
    );
}

#[test]
fn a_request_with_no_block_a_block_of_another_version_or_no_unit_is_above() {
    let env = engine_with(item_workflow(&["build"], false), &[], &[("u1", &[])]);
    let stalled = park(&env, "u1", Cause::Stalled);
    assert_eq!(line(&env, &stalled, None), Line::Below, "the control");
    let with_body = |body: Value| RequestRow {
        body: body.to_string(),
        ..stalled.clone()
    };
    let block = |v: u64| json!({ "decision": { "v": v, "cause": "stalled" } });
    // No block (a park stored before the cause existed), and a body that is not JSON at all.
    assert_eq!(line(&env, &with_body(json!({})), None), Line::Above);
    let broken = RequestRow {
        body: "not json".to_string(),
        ..stalled.clone()
    };
    assert_eq!(line(&env, &broken, None), Line::Above);
    // The version this binary reads, and one it does not.
    assert_eq!(line(&env, &with_body(block(1)), None), Line::Below);
    assert_eq!(line(&env, &with_body(block(2)), None), Line::Above);
    // A cause this binary does not know.
    let unknown = json!({ "decision": { "v": 1, "cause": "cosmic_ray" } });
    assert_eq!(line(&env, &with_body(unknown), None), Line::Above);
    // A request with no unit, and a unit the run does not hold.
    let run_level = RequestRow {
        unit_id: None,
        ..stalled.clone()
    };
    assert_eq!(line(&env, &run_level, None), Line::Above);
    let stranger = RequestRow {
        unit_id: Some("nobody".to_string()),
        ..stalled
    };
    assert_eq!(line(&env, &stranger, None), Line::Above);
}

#[test]
fn a_park_stores_no_line_key_in_the_request_body() {
    let env = engine_with(free("feature/x"), &[], &[("u1", &[]), ("u2", &[])]);
    for (unit, cause) in [("u1", Cause::Stalled), ("u2", Cause::MergeChecksRed)] {
        let request = park(&env, unit, cause);
        let body: Value = serde_json::from_str(&request.body).unwrap();
        assert!(body.get("line").is_none(), "{body}");
        assert!(body["decision"].get("line").is_none(), "{body}");
        assert!(!request.body.contains("below") && !request.body.contains("above"));
        // Reading the class changes nothing that is stored.
        let before = env.store.all_requests();
        line(&env, &request, Some(FORGE));
        assert_eq!(env.store.all_requests(), before);
    }
}

// ---------------------------------------------------------------- the audit's cases (C12)

#[test]
fn a_leading_refs_heads_is_cut_from_each_name_before_the_compare() {
    both(
        merge_lines(free("refs/heads/main"), &[], &[], Some(FORGE)),
        Line::Above,
        "the base refs/heads/main is the default branch main",
    );
    both(
        merge_lines(
            free("main"),
            &[],
            &[],
            Some(("synthetic/repo", "refs/heads/main")),
        ),
        Line::Above,
        "the base main is the default branch refs/heads/main",
    );
    both(
        merge_lines(free("refs/heads/feature/x"), &[], &[], Some(FORGE)),
        Line::Below,
        "the cut names another branch",
    );
}

#[test]
fn nothing_else_is_folded_the_compare_is_exact_and_case_sensitive() {
    both(
        merge_lines(free("Main"), &[], &[], Some(FORGE)),
        Line::Below,
        "Main is not main",
    );
    // Only one leading refs/heads/ is cut.
    both(
        merge_lines(free("refs/heads/refs/heads/main"), &[], &[], Some(FORGE)),
        Line::Below,
        "one cut leaves refs/heads/main, which is not main",
    );
}

#[test]
fn a_repo_that_is_not_the_forges_gives_above() {
    let wf = merge_workflow(
        MergeRule::GreenChecksInDeclaredOrder,
        "synthetic/other",
        "feature/x",
        "",
    );
    both(
        merge_lines(wf, &[], &[], Some(FORGE)),
        Line::Above,
        "the unit merges into another repo, so its default branch is not known",
    );
}

#[test]
fn an_empty_base_an_empty_repo_and_a_name_that_does_not_render_give_above() {
    both(
        merge_lines(free(""), &[], &[], Some(FORGE)),
        Line::Above,
        "an empty base declares nothing",
    );
    let empty_repo = merge_workflow(MergeRule::GreenChecksInDeclaredOrder, "", "feature/x", "");
    both(
        merge_lines(empty_repo, &[], &[], Some(FORGE)),
        Line::Above,
        "an empty repo declares nothing",
    );
    let bad_repo = merge_workflow(
        MergeRule::GreenChecksInDeclaredOrder,
        "{param.nothing}",
        "feature/x",
        "",
    );
    both(
        merge_lines(bad_repo, &[], &[], Some(FORGE)),
        Line::Above,
        "the repo does not render",
    );
    let bad_head = merge_workflow(
        MergeRule::GreenChecksInDeclaredOrder,
        "synthetic/repo",
        "feature/x",
        "{param.nothing}",
    );
    both(
        merge_lines(bad_head, &[], &[], Some(FORGE)),
        Line::Above,
        "the head branch does not render, so the merge step's own render fails",
    );
}

#[test]
fn a_base_with_a_unit_variable_renders_through_the_merge_steps_own_function() {
    let wf = free("{unit.vars.base}");
    let env = engine_with(
        wf,
        &[],
        &[
            ("onto-default", &[("base", "main")]),
            ("onto-topic", &[("base", "topic/y")]),
        ],
    );
    let default = park(&env, "onto-default", Cause::MergeChecksRed);
    let topic = park(&env, "onto-topic", Cause::MergeChecksRed);
    assert_eq!(
        line(&env, &default, Some(FORGE)),
        Line::Above,
        "this unit's base renders to the default branch"
    );
    assert_eq!(
        line(&env, &topic, Some(FORGE)),
        Line::Below,
        "this unit's base renders to another branch"
    );
}

#[test]
fn a_base_with_a_run_parameter_renders_from_the_engines_own_parameters() {
    let wf = free("{param.spine_branch}");
    both(
        merge_lines(
            wf.clone(),
            &[("spine_branch", "spine/s1")],
            &[],
            Some(FORGE),
        ),
        Line::Below,
        "the parameter names another branch",
    );
    both(
        merge_lines(wf, &[("spine_branch", "main")], &[], Some(FORGE)),
        Line::Above,
        "the parameter names the default branch",
    );
}

// ---------------------------------------------------------------- the fake store's grant rules

/// A commit of one kind with these effects, in `scope`.
fn commit_of(scope: &str, id: &str, effects: Vec<Effect>) -> Commit {
    Commit {
        run_id: scope.to_string(),
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
    }
}

// This tests the fake store alone. The real store's grant rules are tested in `pf-store-sqlite`
// (`store_clear_grants.rs`, `clear_grant_admission_property.rs`), so the name claims no parity.
#[test]
fn the_fake_store_holds_the_grant_rules() {
    let store = FakeStore::new();
    let grant = |generation| {
        Effect::GrantClear(NewClearGrant {
            project_id: "P-1".to_string(),
            generation,
            agent_id: ORCHESTRATOR_AGENT.to_string(),
            granted_by: "cli".to_string(),
            granted_at: "t".to_string(),
        })
    };
    let require = |generation| Effect::RequireLiveGrant {
        project_id: "P-1".to_string(),
        generation,
    };
    let revoke = |generation| Effect::RevokeClearGrant {
        project_id: "P-1".to_string(),
        generation,
        revoked_by: "cli".to_string(),
        revoked_at: "t".to_string(),
    };
    let applied = |c: Committed| assert!(matches!(c, Committed::Applied { .. }), "{c:?}");
    let rejected = |c: Committed| assert!(matches!(c, Committed::Rejected(_)), "{c:?}");
    // An unknown project holds no grant and carries no precondition.
    rejected(
        store
            .commit(commit_of(CONTROLLER_SCOPE, "a", vec![grant(1)]))
            .unwrap(),
    );
    rejected(
        store
            .commit(commit_of("run-1", "b", vec![require(1)]))
            .unwrap(),
    );
    store.add_project("P-1");
    // A run's scope cannot give or end a grant.
    rejected(
        store
            .commit(commit_of("run-1", "c", vec![grant(1)]))
            .unwrap(),
    );
    applied(
        store
            .commit(commit_of(CONTROLLER_SCOPE, "d", vec![grant(1)]))
            .unwrap(),
    );
    rejected(
        store
            .commit(commit_of(CONTROLLER_SCOPE, "e", vec![grant(2)]))
            .unwrap(),
    );
    // The precondition writes nothing, and rejects the whole commit when it fails.
    applied(
        store
            .commit(commit_of("run-1", "f", vec![require(1)]))
            .unwrap(),
    );
    rejected(
        store
            .commit(commit_of("run-1", "g", vec![require(2)]))
            .unwrap(),
    );
    assert!(store.admission("run-1", "g").unwrap().is_none());
    applied(
        store
            .commit(commit_of(CONTROLLER_SCOPE, "h", vec![revoke(1)]))
            .unwrap(),
    );
    rejected(
        store
            .commit(commit_of(CONTROLLER_SCOPE, "i", vec![revoke(1)]))
            .unwrap(),
    );
    rejected(
        store
            .commit(commit_of("run-1", "j", vec![require(1)]))
            .unwrap(),
    );
    applied(
        store
            .commit(commit_of(CONTROLLER_SCOPE, "k", vec![grant(2)]))
            .unwrap(),
    );
    rejected(
        store
            .commit(commit_of("run-1", "l", vec![require(1)]))
            .unwrap(),
    );
    applied(
        store
            .commit(commit_of("run-1", "m", vec![require(2)]))
            .unwrap(),
    );
    // A removed project's grant is live for no commit, and its row can still be ended.
    store.remove_project("P-1");
    rejected(
        store
            .commit(commit_of("run-1", "n", vec![require(2)]))
            .unwrap(),
    );
    applied(
        store
            .commit(commit_of(CONTROLLER_SCOPE, "o", vec![revoke(2)]))
            .unwrap(),
    );
    assert_eq!(store.clear_grants().unwrap().len(), 2);
}

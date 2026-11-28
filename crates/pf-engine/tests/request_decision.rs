//! AC-1 of `r2.s11.w1`: a park carries its cause, and the request it opens holds a decision block
//! (`docs/contracts/requests.md`, "The decision block"). The engine writes the block from the
//! cause and its own facts when it decides the park command. The block never holds a seat's
//! words, and nothing in it acts: it is data for a reader.

mod common;

use common::*;
use pf_engine::claim::sha256_hex;
use pf_engine::merge::MergeStep;
use pf_engine::port::forge::PrRef;
use pf_engine::port::store::{
    AdmissionOutcome, Cause, Commit, Effect, NewAdmission, RequestKind, RequestRow, Store,
};
use pf_engine::unit::Signal;
use pf_engine::workflow::OutcomeDef;
use pf_engine::{Command, CommandBody, EngineError, MergeRule, StageDef, Workflow};
use serde_json::{Value, json};

/// Words a seat wrote. They go into the reason and the facts of every park below, and no block may
/// hold them.
const SEAT_WORDS: &str = "SEAT-WORDS-9f3a";

/// A workflow with the stages `implement` and `verify`, then a merge.
fn item_env(units: &[&str]) -> Env {
    let env = env(item_workflow(&["implement", "verify"], true));
    let records = units
        .iter()
        .enumerate()
        .map(|(i, id)| unit(id, "g1", u32::try_from(i).unwrap()))
        .collect();
    env.engine.ingest(&mut FakeUnitSource { records }).unwrap();
    env
}

fn requests(env: &Env) -> Vec<RequestRow> {
    env.engine.requests().unwrap()
}

fn request_of(env: &Env, unit: &str) -> RequestRow {
    requests(env)
        .into_iter()
        .find(|r| r.unit_id.as_deref() == Some(unit))
        .unwrap_or_else(|| panic!("no request for {unit}"))
}

fn body_of(r: &RequestRow) -> Value {
    serde_json::from_str(&r.body).unwrap()
}

fn block_of(r: &RequestRow) -> Value {
    body_of(r)["decision"].clone()
}

fn pr(n: u64) -> Option<PrRef> {
    Some(PrRef {
        repo: "synthetic/repo".to_string(),
        number: n,
    })
}

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

/// The facts a cause's park carries besides the reason.
fn facts_of(cause: Cause) -> Option<Value> {
    match cause {
        Cause::OutcomeCap => Some(json!({ "cap_to": "verify", "cap_max": 2 })),
        Cause::StopFile => Some(json!({
            "stop_text": format!("stop: {SEAT_WORDS}"),
            "stop_options": [
                { "text": format!("go on, said {SEAT_WORDS}"), "recommended": false },
                { "text": "stop here", "recommended": true },
            ],
        })),
        _ => None,
    }
}

fn names(block: &Value) -> Vec<String> {
    block["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["id"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn every_cause_opens_a_block_that_names_it_and_holds_no_seat_words() {
    let causes: Vec<Cause> = Cause::ALL
        .into_iter()
        .filter(|c| *c != Cause::MergeWaitsAck)
        .collect();
    let ids: Vec<String> = (0..causes.len()).map(|i| format!("u{i}")).collect();
    let env = item_env(&ids.iter().map(String::as_str).collect::<Vec<_>>());
    for (cause, id) in causes.iter().zip(&ids) {
        env.engine.start_stage(id).unwrap();
        env.engine
            .park_with(
                id,
                kind_of(*cause),
                *cause,
                &format!("parked: {SEAT_WORDS}"),
                facts_of(*cause).as_ref(),
            )
            .unwrap();
        let request = request_of(&env, id);
        let block = block_of(&request);
        assert_eq!(block["v"], 1, "{cause:?}: {block}");
        assert_eq!(block["cause"], cause.as_str(), "{cause:?}");
        let summary = block["summary"].as_str().unwrap();
        assert!(!summary.is_empty(), "{cause:?}");
        assert!(summary.chars().count() <= 300, "{cause:?}: {summary}");
        assert!(
            !summary.contains(SEAT_WORDS),
            "the summary of {cause:?} holds seat words: {summary}"
        );
        assert!(block["options"].as_array().unwrap().len() <= 4, "{cause:?}");
    }
    // The cause names itself in code, and a closed list holds each once.
    assert_eq!(Cause::ALL.len(), 23);
    for c in Cause::ALL {
        assert_eq!(Cause::parse(c.as_str()), Some(c));
    }
}

#[test]
fn the_options_and_the_recommendation_follow_the_table_of_the_cause() {
    let causes: Vec<Cause> = Cause::ALL
        .into_iter()
        .filter(|c| *c != Cause::MergeWaitsAck)
        .collect();
    let ids: Vec<String> = (0..causes.len()).map(|i| format!("u{i}")).collect();
    let env = item_env(&ids.iter().map(String::as_str).collect::<Vec<_>>());
    for (cause, id) in causes.iter().zip(&ids) {
        env.engine.start_stage(id).unwrap();
        env.engine
            .park_with(
                id,
                kind_of(*cause),
                *cause,
                "parked",
                facts_of(*cause).as_ref(),
            )
            .unwrap();
        let block = block_of(&request_of(&env, id));
        let recommended = block.get("recommended");
        match cause {
            Cause::StopFile => {
                assert_eq!(names(&block), ["s1", "s2"], "{block}");
                assert_eq!(
                    block["options"][0]["effect"],
                    json!({ "verb": "answer", "text": format!("go on, said {SEAT_WORDS}") })
                );
                assert_eq!(block["options"][1]["effect"]["text"], "stop here");
                assert_eq!(recommended.unwrap()["option"], "s2");
                assert_eq!(recommended.unwrap()["by"], "seat");
            }
            // Only the workflow knows what clears its own file: with no hook it takes no text.
            Cause::ParkFile => {
                assert_eq!(names(&block), ["resume"], "{block}");
                assert!(
                    block["options"][0]["detail"]
                        .as_str()
                        .unwrap()
                        .contains("file"),
                    "{block}"
                );
                assert!(recommended.is_none());
            }
            Cause::OutcomeCap => {
                assert_eq!(names(&block), ["again", "go_on"], "{block}");
                assert_eq!(
                    block["options"][0]["effect"],
                    json!({ "verb": "resume", "to": "verify" })
                );
                assert_eq!(block["options"][1]["effect"], json!({ "verb": "resume" }));
                assert!(recommended.is_none(), "{block}");
            }
            other => {
                assert_eq!(names(&block), ["resume"], "{other:?}: {block}");
                assert_eq!(
                    block["options"][0]["effect"],
                    json!({ "verb": "resume" }),
                    "{other:?}"
                );
                let below_the_line = matches!(
                    other,
                    Cause::Stalled
                        | Cause::SeatLost
                        | Cause::HostUnreachable
                        | Cause::MergeChecksRed
                );
                if below_the_line {
                    let r = recommended.unwrap_or_else(|| panic!("{other:?}: {block}"));
                    assert_eq!(r["option"], "resume");
                    assert_eq!(r["by"], "controller");
                    assert!(!r["why"].as_str().unwrap().is_empty());
                } else {
                    // `launch_failed` too: the launch error carries no mark that it is transient.
                    assert!(recommended.is_none(), "{other:?}: {block}");
                }
            }
        }
    }
}

#[test]
fn a_park_file_that_takes_text_holds_no_option_and_one_that_does_not_holds_a_resume() {
    let env = item_env(&["takes", "plain"]);
    for (id, takes) in [("takes", true), ("plain", false)] {
        env.engine.start_stage(id).unwrap();
        env.engine
            .park_with(
                id,
                RequestKind::Park,
                Cause::ParkFile,
                "a park file appeared",
                Some(&json!({ "takes_text": takes })),
            )
            .unwrap();
    }
    let takes = block_of(&request_of(&env, "takes"));
    assert_eq!(takes["takes_text"], true);
    assert!(takes["options"].as_array().unwrap().is_empty(), "{takes}");
    let plain = block_of(&request_of(&env, "plain"));
    assert_eq!(plain["takes_text"], false);
    assert_eq!(names(&plain), ["resume"], "{plain}");
    assert!(
        plain["options"][0]["detail"]
            .as_str()
            .unwrap()
            .contains("gone"),
        "{plain}"
    );
    assert!(plain.get("recommended").is_none());
}

#[test]
fn refs_hold_the_run_the_unit_and_the_stage_and_a_pr_and_a_project_only_when_they_exist() {
    let env = item_env(&["u1", "u2"]);
    env.engine.start_stage("u1").unwrap();
    env.engine
        .park_as("u1", RequestKind::Stall, Cause::Stalled, "stalled")
        .unwrap();
    let block = block_of(&request_of(&env, "u1"));
    assert_eq!(
        block["refs"],
        json!({ "run": "run-1", "unit": "u1", "stage": "implement" }),
        "a run armed from the CLI has no project, and no PR is recorded yet"
    );

    // A PR is recorded with the first stage's signal, and the run links to a project.
    env.engine.start_stage("u2").unwrap();
    env.engine
        .signal(
            "u2",
            "implement",
            Signal::PrState {
                state: "open".to_string(),
            },
            pr(7),
        )
        .unwrap();
    env.store
        .commit(Commit {
            run_id: "run-1".to_string(),
            command_id: "run-project-run-1".to_string(),
            content: "{}".to_string(),
            admission: NewAdmission {
                kind: "run_project".to_string(),
                outcome: AdmissionOutcome::Admitted,
                reason: None,
                unit_id: None,
                role: None,
                model: None,
                attempt: None,
                admitted_at: "2026-11-21T00:00:00.000Z".to_string(),
            },
            effects: vec![Effect::SetRunProject {
                run_id: "run-1".to_string(),
                project_id: "P-1".to_string(),
            }],
            ledger: None,
        })
        .unwrap();
    env.engine.start_stage("u2").unwrap();
    env.engine
        .park_as("u2", RequestKind::Stall, Cause::Stalled, "stalled")
        .unwrap();
    let block = block_of(&request_of(&env, "u2"));
    assert_eq!(
        block["refs"],
        json!({
            "project": "P-1",
            "run": "run-1",
            "unit": "u2",
            "stage": "verify",
            "pr": "synthetic/repo#7",
        })
    );
}

#[test]
fn attempt_counts_the_launches_of_the_parked_stage_and_is_zero_at_the_merge() {
    let env = item_env(&["u1"]);
    // One launch ended, a second launched, and the park: attempt 2.
    env.engine.start_stage("u1").unwrap();
    env.engine
        .end("u1/implement#1", pf_engine::EndStatus::Failed, "stalled: x")
        .unwrap();
    env.engine.start_stage("u1").unwrap();
    env.engine
        .park_as("u1", RequestKind::Stall, Cause::Stalled, "stalled")
        .unwrap();
    assert_eq!(block_of(&request_of(&env, "u1"))["attempt"], 2);

    // Every stage done, no PR recorded: the merge parks the unit at the merge.
    let env2 = item_env(&["u2"]);
    for stage in ["implement", "verify"] {
        env2.engine.start_stage("u2").unwrap();
        env2.engine
            .signal(
                "u2",
                stage,
                Signal::DoneFile {
                    path: "x".to_string(),
                },
                None,
            )
            .unwrap();
    }
    let step = env2.engine.merge_next().unwrap();
    assert!(matches!(step, MergeStep::Parked { .. }), "{step:?}");
    let request = request_of(&env2, "u2");
    assert_eq!(request.stage, "merge");
    let block = block_of(&request);
    assert_eq!(block["cause"], "no_pr");
    assert_eq!(block["attempt"], 0);
    assert_eq!(block["refs"]["stage"], "merge");
}

#[test]
fn asked_by_is_the_seat_for_its_own_file_and_the_controller_otherwise() {
    let env = item_env(&["stop", "stall", "silent", "lost"]);
    for (id, kind, cause) in [
        ("stop", RequestKind::Stop, Cause::StopFile),
        ("stall", RequestKind::Stall, Cause::Stalled),
        ("silent", RequestKind::Park, Cause::NoDoneSignal),
        ("lost", RequestKind::SeatLost, Cause::SeatLost),
    ] {
        env.engine.start_stage(id).unwrap();
        env.engine.park_as(id, kind, cause, "parked").unwrap();
    }
    let stop = block_of(&request_of(&env, "stop"));
    assert_eq!(
        stop["asked_by"],
        json!({ "kind": "seat", "role": "implement-role", "unit": "stop" })
    );
    assert!(stop.get("about").is_none(), "{stop}");
    for id in ["stall", "silent", "lost"] {
        let block = block_of(&request_of(&env, id));
        assert_eq!(block["asked_by"], json!({ "kind": "controller" }), "{id}");
        assert_eq!(
            block["about"],
            json!({ "role": "implement-role", "unit": id }),
            "{id}"
        );
    }
    // A park at the merge concerns no seat: no `about`.
    let env2 = item_env(&["u9"]);
    env2.engine.start_stage("u9").unwrap();
    env2.engine
        .park_as("u9", RequestKind::Park, Cause::PinsChanged, "pins")
        .unwrap();
    let block = block_of(&request_of(&env2, "u9"));
    assert_eq!(block["asked_by"], json!({ "kind": "controller" }));
    assert!(block.get("about").is_none(), "{block}");
}

#[test]
fn a_fact_named_decision_cannot_replace_the_block() {
    let env = item_env(&["u1"]);
    env.engine.start_stage("u1").unwrap();
    let result = env.engine.park_with(
        "u1",
        RequestKind::Park,
        Cause::HookFailed,
        "a hook failed",
        Some(&json!({ "decision": { "cause": "stop_file" } })),
    );
    match result {
        Err(EngineError::Rejected(why)) => assert!(why.contains("decision"), "{why}"),
        other => panic!("a fact named decision must be refused, got {other:?}"),
    }
    assert!(requests(&env).is_empty(), "the refused park opened nothing");
}

#[test]
fn a_park_command_with_no_cause_opens_a_request_with_no_block_and_no_cause_key() {
    let env = item_env(&["u1"]);
    env.engine.start_stage("u1").unwrap();
    // The old shape: no `cause` field at all.
    let admission = env
        .engine
        .submit(Command {
            id: "park:u1:0".to_string(),
            body: CommandBody::Park {
                unit_id: "u1".to_string(),
                reason: "stalled".to_string(),
                request_kind: RequestKind::Stall,
                cause: None,
                detail: None,
            },
        })
        .unwrap();
    assert!(
        !admission.row.content.contains("cause"),
        "{}",
        admission.row.content
    );
    let request = request_of(&env, "u1");
    assert!(body_of(&request).get("decision").is_none());
    assert!(!request.body.contains("cause"), "{}", request.body);
    // The stored content of an old park, with no `cause`, replays as it was.
    let again = env
        .engine
        .submit(Command {
            id: "park:u1:0".to_string(),
            body: CommandBody::Park {
                unit_id: "u1".to_string(),
                reason: "stalled".to_string(),
                request_kind: RequestKind::Stall,
                cause: None,
                detail: None,
            },
        })
        .unwrap();
    assert_eq!(again.row.content, admission.row.content);
    assert_eq!(requests(&env).len(), 1);
}

const SHA: &str = "1111111111111111111111111111111111111111";
const MOVED: &str = "2222222222222222222222222222222222222222";

fn ack_workflow() -> Workflow {
    let mut wf = Workflow::single_track(
        "spine-flow",
        vec![StageDef::new("spine_close", "close-role", "model-a")],
        Some(MergeRule::ClaimAck),
        pf_engine::capacity::Settings::default(),
    );
    wf.tracks[0].name = "spine".to_string();
    let def = wf.tracks[0].merge.as_mut().unwrap();
    def.repo = "synthetic/repo".to_string();
    def.base = "main".to_string();
    def.head_branch = "spine/s1".to_string();
    wf
}

fn ack_ready() -> Env {
    let env = env(ack_workflow());
    env.engine
        .ingest(&mut TrackedSource {
            track: "spine".to_string(),
            records: vec![unit("spine", "spine", 1)],
        })
        .unwrap();
    env.forge.script(
        7,
        PrScript {
            base: "main".to_string(),
            ..PrScript::green("spine/s1", SHA)
        },
    );
    env.engine.start_stage("spine").unwrap();
    env.engine
        .signal(
            "spine",
            "spine_close",
            Signal::PrState {
                state: "open".to_string(),
            },
            pr(7),
        )
        .unwrap();
    env
}

#[test]
fn a_merge_ack_waits_with_one_approve_option_and_no_recommendation() {
    let env = ack_ready();
    let step = env.engine.merge_next().unwrap();
    assert!(matches!(step, MergeStep::AckRequested { .. }), "{step:?}");
    let block = block_of(&request_of(&env, "spine"));
    assert_eq!(block["cause"], "merge_waits_ack");
    assert_eq!(names(&block), ["approve"], "{block}");
    assert_eq!(block["options"][0]["effect"], json!({ "verb": "ack" }));
    // Never a recommendation: the ack is the operator's own act (Q3).
    assert!(block.get("recommended").is_none(), "{block}");
    assert_eq!(block["refs"]["pr"], "synthetic/repo#7");
    assert_eq!(block["attempt"], 0);
    assert_eq!(block["asked_by"], json!({ "kind": "controller" }));
    assert!(block["summary"].as_str().unwrap().contains(&SHA[..12]));
}

#[test]
fn a_revised_merge_ack_holds_a_block_built_at_the_new_revision() {
    let env = ack_ready();
    env.engine.merge_next().unwrap();
    let first = request_of(&env, "spine");
    assert_eq!(first.revision, 1);
    let holder = env
        .engine
        .claim("op", &sha256_hex(b"op"))
        .unwrap()
        .generation;
    // The head moves after the request opened: the ack revises the request before it refuses.
    env.forge.scripts.lock().unwrap().get_mut(&7).unwrap().sha = MOVED.to_string();
    let refused = env.engine.ack_merge(&first.id, 1, holder);
    assert!(
        matches!(refused, Err(EngineError::Rejected(_))),
        "{refused:?}"
    );
    let revised = request_of(&env, "spine");
    assert_eq!(revised.revision, 2);
    let block = block_of(&revised);
    assert_eq!(block["cause"], "merge_waits_ack");
    let summary = block["summary"].as_str().unwrap();
    assert!(summary.contains(&MOVED[..12]), "{summary}");
    assert!(!summary.contains(&SHA[..12]), "{summary}");
    assert_eq!(names(&block), ["approve"]);
}

fn cap_env() -> Env {
    let mut review = StageDef::new("review", "review-role", "model-a");
    review.outcomes = vec![OutcomeDef {
        to: "fix".to_string(),
        max: 1,
    }];
    let mut fix = StageDef::new("fix", "fix-role", "model-a");
    fix.entered_by_outcome = true;
    fix.outcomes = vec![OutcomeDef {
        to: "review".to_string(),
        max: 3,
    }];
    let mut wf = Workflow::single_track(
        "cap-flow",
        vec![review, fix],
        Some(MergeRule::GreenChecksInDeclaredOrder),
        pf_engine::capacity::Settings::default(),
    );
    wf.tracks[0].name = "item".to_string();
    let env = env(wf);
    env.engine
        .ingest(&mut FakeUnitSource {
            records: vec![unit("u1", "g1", 1)],
        })
        .unwrap();
    env
}

#[test]
fn the_cap_park_names_the_capped_stage_and_its_number_from_facts_not_from_the_reason() {
    let env = cap_env();
    let done = || Signal::DoneFile {
        path: "x".to_string(),
    };
    // review -> fix (visit 1 of 1), fix -> review, then review -> fix is visit 2 of 1: the cap.
    env.engine.start_stage("u1").unwrap();
    env.engine.outcome("u1", "review", 0, done(), None).unwrap();
    env.engine.start_stage("u1").unwrap();
    env.engine.outcome("u1", "fix", 0, done(), None).unwrap();
    env.engine.start_stage("u1").unwrap();
    env.engine.outcome("u1", "review", 0, done(), None).unwrap();

    let request = request_of(&env, "u1");
    let body = body_of(&request);
    assert_eq!(body["cap_to"], "fix");
    assert_eq!(body["cap_max"], 1);
    let block = body["decision"].clone();
    assert_eq!(block["cause"], "outcome_cap");
    assert_eq!(names(&block), ["again", "go_on"]);
    assert_eq!(
        block["options"][0]["effect"],
        json!({ "verb": "resume", "to": "fix" })
    );
    let summary = block["summary"].as_str().unwrap();
    assert!(summary.contains("fix"), "{summary}");
    assert!(summary.contains('1'), "{summary}");
    assert_eq!(block["asked_by"], json!({ "kind": "controller" }));
    assert!(block.get("recommended").is_none());
    // The engine derives nothing from the reason text: a reason that names no stage still works.
    assert_eq!(request.stage, "review");
}

/// A unit with every stage done and its PR (number 7) recorded, merging by rule, with the PR
/// scripted by `script`. It answers the decision block of the request the merge opens.
fn merge_block(script: impl FnOnce(&mut PrScript)) -> Value {
    let env = item_env(&["u1"]);
    let mut pr_script = PrScript::green("work/u1", SHA);
    script(&mut pr_script);
    env.forge.script(7, pr_script);
    for (stage, recorded) in [("implement", pr(7)), ("verify", None)] {
        env.engine.start_stage("u1").unwrap();
        env.engine
            .signal(
                "u1",
                stage,
                Signal::DoneFile {
                    path: "x".to_string(),
                },
                recorded,
            )
            .unwrap();
    }
    let step = env.engine.merge_next().unwrap();
    assert!(matches!(step, MergeStep::Parked { .. }), "{step:?}");
    let request = request_of(&env, "u1");
    assert_eq!(request.kind, RequestKind::MergeFailed);
    assert_eq!(request.stage, "merge");
    block_of(&request)
}

#[test]
fn a_merge_that_does_not_land_names_its_cause_from_the_merge_outcome() {
    use pf_engine::port::forge::{Checks, ForgeOutcome, Mergeable};
    let red = merge_block(|s| s.checks = Checks::Red);
    assert_eq!(red["cause"], "merge_checks_red");
    assert_eq!(red["recommended"]["option"], "resume");
    assert_eq!(red["refs"]["pr"], "synthetic/repo#7");

    let conflict = merge_block(|s| s.mergeable = Mergeable::Conflict);
    assert_eq!(conflict["cause"], "merge_conflict");
    let forge_conflict = merge_block(|s| s.merge = ForgeOutcome::Conflict);
    assert_eq!(forge_conflict["cause"], "merge_conflict");

    let refused = merge_block(|s| s.merge = ForgeOutcome::Refused("branch protected".to_string()));
    assert_eq!(refused["cause"], "merge_refused");
    assert!(
        !refused["summary"].as_str().unwrap().contains("protected"),
        "the forge's words stay out of the summary: {refused}"
    );

    // An outcome no other cause fits, and a merge that did not land.
    let other = merge_block(|s| s.merge = ForgeOutcome::NotLanded);
    assert_eq!(other["cause"], "merge_error");
    let not_landed = merge_block(|s| s.landed = false);
    assert_eq!(not_landed["cause"], "merge_error");
    assert!(not_landed.get("recommended").is_none());
    // No stage node belongs to the merge.
    assert_eq!(not_landed["attempt"], 0);
}

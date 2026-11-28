//! AC-4 of `r2.s3.w2`: the project's Orchestrator settles a park below the line with `settle`
//! (`docs/contracts/requests.md`, "Settle"; ADR-0004, ADR-0005, ADR-0006, amended). The controller
//! names the caller's session from its process and its host, admits the verb only for a live session
//! of the project's Orchestrator, under a live grant, on a request that is `below`, and every
//! refusal is audited under an ID the controller makes.
//!
//! The controller runs in the test process on the fake adapter, with a recording chat. A caller is
//! this test binary as a helper below a shell whose PID the test registers as the chat's process.

mod common;
#[path = "common/settle_rig.rs"]
mod settle_rig;

use std::sync::Arc;

use common::chat::*;
use common::*;
use pf_api::types::{RequestList, SettleAnswer};
use pf_engine::port::store::{Cause, Effect, RequestStatus, Store};
use pf_ipc::{Handler, Request};
use serde_json::{Value, json};
use settle_rig::*;

fn answer_of(resp: &Value) -> SettleAnswer {
    serde_json::from_value(done(resp)).expect("the answer decodes as SettleAnswer")
}

fn requests_json(rig: &SRig) -> Value {
    cli_json(&rig.live, &["requests", "--all"])
}

fn summary_of(rig: &SRig, id: &str) -> Value {
    requests_json(rig)["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id)
        .cloned()
        .unwrap_or_else(|| panic!("no request {id}"))
}

fn status_of(rig: &SRig, id: &str) -> String {
    rig.live
        .controller
        .store()
        .request(id)
        .unwrap()
        .unwrap()
        .status
        .as_str()
        .to_string()
}

// ------------------------------------------------------------------ the acts

#[test]
fn a_stall_is_settled_with_clear_and_the_unit_launches_again() {
    let rig = SRig::new(&["u1"], Some(10));
    let (id, rev) = rig.request("u1");
    let launches = rig.admissions("launch");
    let resp = rig.settle(&id, rev, "clear", None, WHY);
    let answer = answer_of(&resp);
    assert_eq!(
        (answer.request_id.as_str(), answer.revision),
        (id.as_str(), rev)
    );
    assert!(!answer.replayed);
    let s = &answer.settled;
    assert_eq!(
        (
            s.act.as_str(),
            s.session.as_str(),
            s.project.as_str(),
            s.generation
        ),
        ("clear", rig.session.as_str(), rig.project.as_str(), 1)
    );
    assert_eq!((s.agent.as_str(), s.reason.as_str()), ("orchestrator", WHY));
    assert!(s.to_stage.is_none());

    // The request is answered, with the record of who settled it.
    assert_eq!(status_of(&rig, &id), "answered");
    let summary = summary_of(&rig, &id);
    assert_eq!(summary["status"], "answered");
    assert_eq!(summary["settled"]["act"], "clear");
    assert_eq!(summary["settled"]["session"], rig.session.as_str());
    assert_eq!(summary["line"], "below");
    // The unit launches again on the next tick.
    rig.tick();
    assert!(
        rig.admissions("launch") > launches,
        "the unit launched again"
    );
    // The run's ledger says who settled and why.
    let lines = rig.run_ledger("settle");
    assert_eq!(lines.len(), 1, "{lines:#?}");
    assert!(
        lines[0].ends_with(&format!(
            "settle {id}@{rev} by grant {}#1 session {}: act=clear reason={WHY}",
            rig.project, rig.session
        )),
        "{lines:#?}"
    );
}

#[test]
fn a_lost_seats_request_is_settled_with_resume() {
    let rig = SRig::new(&["u1"], Some(10));
    // A second unit, parked because its seat was lost.
    rig.live.queue_item("u2", 2);
    rig.tick();
    rig.live
        .handle()
        .engine
        .park("u2", Cause::SeatLost, "seat lost: the process is gone")
        .unwrap();
    rig.tick();
    let (id, rev) = rig.request("u2");
    let resp = rig.settle(&id, rev, "resume", None, "the seat is back");
    assert_eq!(answer_of(&resp).settled.act, "resume");
    assert_eq!(status_of(&rig, &id), "answered");
    // `clear` is not an act of a lost seat.
    let (other, rev1) = rig.request("u1");
    let resp = rig.settle(&other, rev1, "resume", None, WHY);
    assert!(
        why(&resp).contains("the cause stalled admits clear, redispatch, not resume"),
        "{resp}"
    );
}

#[test]
fn a_stall_is_settled_with_redispatch_at_an_earlier_stage() {
    let rig = SRig::new(&["u1"], Some(10));
    // The unit advances to its second stage, `verify`, and stalls there.
    let spine = rig.live.sb.spine_dir.clone();
    std::fs::create_dir_all(spine.join("work-u1")).unwrap();
    // The first stage parked in `implement`: settle it with `redispatch --to implement` (the
    // parked stage is admitted), and then let it advance.
    let (id, rev) = rig.request("u1");
    let resp = rig.settle(
        &id,
        rev,
        "redispatch",
        Some("implement"),
        "start the stage over",
    );
    let answer = answer_of(&resp);
    assert_eq!(answer.settled.to_stage.as_deref(), Some("implement"));
    assert_eq!(answer.settled.act, "redispatch");
    let lines = rig.run_ledger("settle");
    assert!(
        lines[0].contains("act=redispatch --to implement reason=start the stage over"),
        "{lines:#?}"
    );
    // A later stage than the parked one is refused and names the parked stage.
    rig.tick();
    rig.live
        .handle()
        .engine
        .park("u1", Cause::Stalled, "stalled: again")
        .unwrap();
    rig.tick();
    let (id, rev) = rig.request("u1");
    let resp = rig.settle(&id, rev, "redispatch", Some("close"), WHY);
    let text = why(&resp);
    assert!(
        text.contains("--to close is after the stage the unit parked in (implement)"),
        "{text}"
    );
    assert_eq!(status_of(&rig, &id), "open", "the request stays open");
}

// ------------------------------------------------------------------ the act this server does not build

#[test]
fn close_stale_is_refused_and_writes_no_admission() {
    let rig = SRig::new(&["u1"], Some(10));
    rig.tick();
    rig.tick();
    let before = rig.admissions("settle");
    let (stall, stall_rev) = rig.request("u1");
    let text = why(&rig.settle(&stall, stall_rev, "close_stale", None, WHY));
    assert!(
        text.contains(
            "settle refused: close_stale has no object: a revision replaces its request in the same row, so no stale request exists"
        ),
        "{text}"
    );
    assert_eq!(rig.admissions("settle"), before, "no admission in the run");
    assert_eq!(status_of(&rig, &stall), "open");
}

#[test]
fn a_seats_question_above_the_line_is_refused_and_stays_open_for_the_claim() {
    let rig = SRig::new(&["u1"], Some(10));
    // A fresh unit for the stop: `u1` is parked on a stall already.
    rig.live.queue_item("u2", 2);
    rig.tick();
    rig.live
        .handle()
        .engine
        .park_with(
            "u2",
            pf_engine::port::store::RequestKind::Stop,
            Cause::StopFile,
            "Pick a branch.",
            Some(&json!({ "stop_text": "Pick a branch.\n" })),
        )
        .unwrap();
    rig.tick();
    let (id, rev) = rig.request("u2");
    assert_eq!(summary_of(&rig, &id)["line"], "above");
    let text = why(&rig.settle(&id, rev, "clear", None, WHY));
    // The stop file's cause admits no act, so the cause check would refuse this request too. The
    // text pins the class refusal, which runs first: with that rule removed this text is gone.
    assert!(
        text.contains(&format!(
            "request {id} is above the line: only the claim may settle it"
        )),
        "{text}"
    );
    assert!(!text.contains("admits none"), "{text}");
    assert_eq!(status_of(&rig, &id), "open");
    assert_eq!(rig.admissions("settle"), 0);
}

#[test]
fn the_settle_client_prints_the_text_form_the_replay_the_json_form_and_the_moved_word() {
    let rig = SRig::new(&["u1"], Some(10));
    let (id, rev) = rig.request("u1");
    let rev_text = rev.to_string();
    let args = [
        "settle",
        id.as_str(),
        "--rev",
        rev_text.as_str(),
        "--act",
        "redispatch",
        "--to",
        "implement",
        "--reason",
        WHY,
    ];
    let out = rig.cli_below_chat(&args);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let first = format!(
        "settled {id}@{rev}: act=redispatch --to implement by grant {}#1 session {}",
        rig.project, rig.session
    );
    assert_eq!(stdout(&out).trim_end(), first, "the text form");
    // The same call again is a replay, and the text says so.
    let out = rig.cli_below_chat(&args);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        stdout(&out).trim_end(),
        format!("{first} (replayed)"),
        "the replay"
    );
    // `--json` prints the answer, as the type decodes it.
    let mut json_args = args.to_vec();
    json_args.push("--json");
    let out = rig.cli_below_chat(&json_args);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let answer: SettleAnswer = serde_json::from_str(&stdout(&out)).expect("SettleAnswer");
    assert!(answer.replayed);
    assert_eq!(answer.settled.act, "redispatch");
    assert_eq!(answer.settled.to_stage.as_deref(), Some("implement"));
    // The request is closed now: another revision is a moved request, and the client names the word.
    let next = (rev + 1).to_string();
    let out = rig.cli_below_chat(&[
        "settle",
        id.as_str(),
        "--rev",
        next.as_str(),
        "--act",
        "clear",
        "--reason",
        WHY,
    ]);
    assert_eq!(code(&out), 1);
    assert!(
        stderr(&out).contains(&format!(
            "request {id} is not open any more (subject_moved)"
        )),
        "{}",
        stderr(&out)
    );
}

// ------------------------------------------------------------------ the grant

#[test]
fn after_a_revoke_a_settle_is_refused_and_after_a_new_grant_it_is_admitted_on_the_same_revision() {
    let rig = SRig::new(&["u1"], Some(10));
    let (id, rev) = rig.request("u1");
    let out = rig
        .live
        .cli(&["grant", "revoke", "--project", &rig.project]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = why(&rig.settle(&id, rev, "clear", None, WHY));
    assert!(
        text.contains(&format!(
            "settle refused: project {} holds no live clear grant",
            rig.project
        )),
        "{text}"
    );
    assert_eq!(status_of(&rig, &id), "open");
    // Nothing in the run holds the settle's ID.
    assert!(
        rig.live
            .controller
            .store()
            .admission(&rig.run, &format!("settle:{id}:{rev}"))
            .unwrap()
            .is_none()
    );
    let out = rig.live.cli(&["grant", "clear", "--project", &rig.project]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let answer = answer_of(&rig.settle(&id, rev, "clear", None, WHY));
    assert_eq!(answer.settled.generation, 2, "the new grant's generation");
}

#[test]
fn a_retry_after_a_revoke_is_refused_at_the_grant_and_the_first_record_stands() {
    let rig = SRig::new(&["u1"], Some(10));
    let (id, rev) = rig.request("u1");
    let first = answer_of(&rig.settle(&id, rev, "clear", None, WHY));
    let out = rig
        .live
        .cli(&["grant", "revoke", "--project", &rig.project]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = why(&rig.settle(&id, rev, "clear", None, "another reason"));
    assert!(text.contains("holds no live clear grant"), "{text}");
    let stored = rig
        .live
        .controller
        .store()
        .request(&id)
        .unwrap()
        .unwrap()
        .answer
        .unwrap();
    assert!(
        stored.contains(WHY) && !stored.contains("another reason"),
        "{stored}"
    );
    assert_eq!(first.settled.reason, WHY);
    assert!(
        rig.refused_settles()
            .iter()
            .any(|(r, _)| r.contains("holds no live clear grant")),
        "the refusal is audited"
    );
}

#[test]
fn a_revoke_between_the_reads_and_the_commit_refuses_the_settle_and_changes_nothing() {
    let rig = SRig::new(&["u1"], Some(10));
    let (id, rev) = rig.request("u1");
    let store = rig.live.controller.store().clone();
    let project = rig.project.clone();
    // The pause holds the verb after it read the grant. The revoke commits there.
    rig.live
        .controller
        .settle_faults()
        .set_after_grant(Some(Arc::new(move || {
            let committed = store
                .commit(pf_engine::port::store::Commit {
                    run_id: pf_engine::port::store::CONTROLLER_SCOPE.to_string(),
                    command_id: "revoke-in-the-gap".to_string(),
                    content: "{}".to_string(),
                    admission: pf_engine::port::store::NewAdmission {
                        kind: "grant_revoke".to_string(),
                        outcome: pf_engine::port::store::AdmissionOutcome::Admitted,
                        reason: None,
                        unit_id: None,
                        role: None,
                        model: None,
                        attempt: None,
                        admitted_at: "2026-11-24T00:00:00.000Z".to_string(),
                    },
                    effects: vec![Effect::RevokeClearGrant {
                        project_id: project.clone(),
                        generation: 1,
                        revoked_by: "cli".to_string(),
                        revoked_at: "2026-11-24T00:00:00.000Z".to_string(),
                    }],
                    ledger: None,
                })
                .unwrap();
            assert!(
                matches!(committed, pf_engine::port::store::Committed::Applied { .. }),
                "{committed:?}"
            );
        })));
    let text = why(&rig.settle(&id, rev, "clear", None, WHY));
    assert!(
        text.contains(&format!(
            "settle refused: project {} holds no live clear grant",
            rig.project
        )),
        "the store's reason: {text}"
    );
    assert_eq!(status_of(&rig, &id), "open", "the request stays open");
    assert_eq!(
        rig.live.unit_status("u1"),
        "parked",
        "the unit keeps its park"
    );
    assert_eq!(rig.admissions("settle"), 0);
}

#[test]
fn a_request_of_a_removed_projects_run_is_refused_and_its_grant_is_not_live() {
    // `project remove` refuses a project with a live chat session or an active run, and the store's
    // `RemoveProject` refuses a live session too. So the test marks the project removed through its
    // own connection, with the Orchestrator session still live: the state that the spec names. The
    // request is still open, and only step 6 (the grant) can refuse it.
    let rig = SRig::new(&["u1"], Some(10));
    let (id, rev) = rig.request("u1");
    let conn = rusqlite::Connection::open(rig.live.sb.state.join("store.sqlite")).unwrap();
    let marked = conn
        .execute(
            "UPDATE projects SET removed_at = '2026-11-24T00:00:00.000Z' WHERE id = ?1",
            [&rig.project],
        )
        .unwrap();
    assert_eq!(marked, 1, "the project is marked removed");
    drop(conn);
    let text = why(&rig.settle(&id, rev, "clear", None, WHY));
    assert!(
        text.contains(&format!(
            "settle refused: project {} holds no live clear grant (the project is removed)",
            rig.project
        )),
        "{text}"
    );
    assert_eq!(status_of(&rig, &id), "open");
    assert_eq!(rig.admissions("settle"), 0);
    // The grant row stays, and no reader marks it live.
    let list = cli_json(&rig.live, &["grant", "list"]);
    assert_eq!(list["grants"][0]["live"], false, "{list}");
}

// ------------------------------------------------------------------ pins, halt, moved requests

#[test]
fn a_run_whose_pinned_file_changed_after_arm_refuses_and_names_the_file() {
    let rig = SRig::new(&["u1"], Some(10));
    let (id, rev) = rig.request("u1");
    let pinned = rig.live.run_dir().join("package/workflow.yaml");
    let mut text = std::fs::read_to_string(&pinned).unwrap();
    text.push_str("# changed after arm\n");
    std::fs::write(&pinned, text).unwrap();
    let refused = why(&rig.settle(&id, rev, "clear", None, WHY));
    assert!(refused.contains("workflow.yaml"), "{refused}");
    assert_eq!(status_of(&rig, &id), "open");
    assert_eq!(rig.admissions("settle"), 0);
}

#[test]
fn a_change_to_the_source_package_after_arm_changes_no_class_and_no_act() {
    let rig = SRig::new(&["u1"], Some(10));
    let (id, rev) = rig.request("u1");
    let source = rig.live.sb.package.join("workflow.yaml");
    let text = std::fs::read_to_string(&source).unwrap();
    std::fs::write(
        &source,
        text.replace("rule: green_checks_in_declared_order", "rule: claim_ack"),
    )
    .unwrap();
    assert_eq!(summary_of(&rig, &id)["line"], "below");
    let answer = answer_of(&rig.settle(&id, rev, "clear", None, WHY));
    assert_eq!(answer.settled.act, "clear");
}

#[test]
fn a_halted_run_refuses_every_settle_and_the_refusal_is_audited() {
    let rig = SRig::new(&["u1"], Some(10));
    let (id, rev) = rig.request("u1");
    // `halt` on the CLI also stops the chats, so the run is halted through the engine here and the
    // session stays live: the decision's own refusal is the one under test.
    rig.live.handle().engine.halt("stop now").unwrap();
    let text = why(&rig.settle(&id, rev, "clear", None, WHY));
    assert!(
        text.contains("is halted: a halted run refuses every settle"),
        "{text}"
    );
    assert_eq!(status_of(&rig, &id), "open");
    assert!(
        rig.refused_settles()
            .iter()
            .any(|(r, _)| r.contains("a halted run refuses every settle"))
    );
}

#[test]
fn a_moved_request_and_a_closed_one_carry_the_word_subject_moved() {
    let rig = SRig::new(&["u1", "u2"], Some(10));
    let (id, rev) = rig.request("u1");
    let resp = rig.settle(&id, rev + 1, "clear", None, WHY);
    assert_eq!(resp["result"]["refusal"], "subject_moved", "{resp}");
    assert!(
        why(&resp).contains(&format!(
            "request {id} is at revision {rev}, not {}",
            rev + 1
        )),
        "{resp}"
    );
    // The claim closes the request first.
    let (id2, rev2) = rig.request("u2");
    let claim = pf_ipc::call(&rig.live.sb.state, "claim", json!({ "holder": "ann@host" })).unwrap();
    assert!(claim.ok, "{:?}", claim.error);
    let token = claim.result["token"].as_str().unwrap().to_string();
    let out = rig.live.cli(&[
        "clear",
        &rig.run,
        "u2",
        "--claim-token-file",
        &write_token(&rig, &token),
    ]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let resp = rig.settle(&id2, rev2, "clear", None, WHY);
    assert_eq!(resp["result"]["refusal"], "subject_moved", "{resp}");
    assert!(
        why(&resp).contains(&format!("request {id2} is not open any more")),
        "{resp}"
    );
}

fn write_token(rig: &SRig, token: &str) -> String {
    let path = rig.live.sb.root.path().join("claim-token-file");
    std::fs::write(&path, token).unwrap();
    path.display().to_string()
}

// ------------------------------------------------------------------ the audited refusal

#[test]
fn every_refusal_is_one_refused_admission_under_an_id_the_controller_makes() {
    let rig = SRig::with(&["u1"], Some(10), |cfg| {
        cfg.refusal_window = std::time::Duration::ZERO;
    });
    let (id, rev) = rig.request("u1");
    let before = rig.refused_settles().len();
    let _ = why(&rig.settle(&id, rev, "rerun_check", None, WHY));
    let _ = why(&rig.settle(&id, rev, "close_stale", None, WHY));
    let _ = why(&rig.settle("R-99", 1, "clear", None, WHY));
    let refused = rig.refused_settles();
    assert_eq!(refused.len(), before + 3, "{refused:#?}");
    for (_, command) in &refused {
        assert!(command.starts_with("caller-refused-"), "{command}");
        assert_ne!(command, &format!("settle:{id}:{rev}"));
    }
    let ledger = ledger_kind(&rig.live, "settle");
    assert!(
        ledger
            .iter()
            .any(|l| l.contains(&format!("caller={} bucket={id} refused=1", rig.session))),
        "{ledger:#?}"
    );
    assert!(
        ledger.iter().any(|l| l.contains("bucket=other")),
        "a made-up request ID goes to the bucket other: {ledger:#?}"
    );
    assert!(
        rig.live
            .controller
            .store()
            .admission(&rig.run, &format!("settle:{id}:{rev}"))
            .unwrap()
            .is_none(),
        "no admission in the run holds the settle's ID after a refusal"
    );
}

#[test]
fn repeated_refusals_in_the_window_coalesce() {
    let rig = SRig::new(&["u1"], Some(10));
    let (id, rev) = rig.request("u1");
    for _ in 0..4 {
        let _ = why(&rig.settle(&id, rev, "close_stale", None, WHY));
    }
    let ledger = ledger_kind(&rig.live, "settle");
    assert_eq!(
        ledger.len(),
        1,
        "the first refusal writes its line and the rest are counted: {ledger:#?}"
    );
}

#[test]
fn a_class_refusal_by_the_engine_is_audited_too() {
    let rig = SRig::new(&["u1"], Some(10));
    rig.live.queue_item("u2", 2);
    rig.tick();
    rig.live
        .handle()
        .engine
        .park_with(
            "u2",
            pf_engine::port::store::RequestKind::Stop,
            Cause::StopFile,
            "Pick a branch.",
            Some(&json!({ "stop_text": "Pick a branch.\n" })),
        )
        .unwrap();
    rig.tick();
    let (id, rev) = rig.request("u2");
    let _ = why(&rig.settle(&id, rev, "clear", None, WHY));
    assert!(!rig.refused_settles().is_empty());
    assert_eq!(rig.admissions("settle"), 0);
}

// ------------------------------------------------------------------ the answer

#[test]
fn settle_json_decodes_and_a_replay_answers_replayed_with_the_first_reason() {
    let rig = SRig::new(&["u1"], Some(10));
    let (id, rev) = rig.request("u1");
    let first = answer_of(&rig.settle(&id, rev, "clear", None, WHY));
    assert!(!first.replayed);
    let again = answer_of(&rig.settle(&id, rev, "clear", None, "another reason"));
    assert!(again.replayed);
    assert_eq!(again.settled.reason, WHY, "the first reason");
    assert_eq!(rig.run_ledger("settle").len(), 1, "a replay writes no line");
    // The settle's own command ID is admitted once.
    assert_eq!(rig.admissions("settle"), 1);
    // Another act on the settled revision is a conflict.
    let text = why(&rig.settle(&id, rev, "redispatch", Some("implement"), WHY));
    assert!(
        text.contains("was settled with another act") || text.contains("settle refused"),
        "{text}"
    );
}

#[test]
fn the_reason_is_redacted_and_no_log_holds_a_claim_token() {
    let rig = SRig::new(&["u1"], Some(10));
    let claim = pf_ipc::call(&rig.live.sb.state, "claim", json!({ "holder": "ann@host" })).unwrap();
    let token = claim.result["token"].as_str().unwrap().to_string();
    let (id, rev) = rig.request("u1");
    let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789ab";
    let resp = rig.settle(
        &id,
        rev,
        "clear",
        None,
        &format!("see {secret} for details"),
    );
    let answer = answer_of(&resp);
    assert!(
        !answer.settled.reason.contains(secret),
        "{}",
        answer.settled.reason
    );
    let stored = rig
        .live
        .controller
        .store()
        .request(&id)
        .unwrap()
        .unwrap()
        .answer
        .unwrap();
    assert!(
        !stored.contains(secret) && !stored.contains(&token),
        "{stored}"
    );
    for line in rig.run_ledger("settle") {
        assert!(!line.contains(secret) && !line.contains(&token), "{line}");
    }
    let ledger = std::fs::read_to_string(rig.live.sb.state.join("controller.ledger")).unwrap();
    assert!(!ledger.contains(&token) && !ledger.contains(secret));
}

#[test]
fn a_reason_that_is_empty_long_or_multiline_is_refused() {
    let rig = SRig::new(&["u1"], Some(10));
    let (id, rev) = rig.request("u1");
    for reason in ["", &"x".repeat(301), "two\nlines"] {
        let text = why(&rig.settle(&id, rev, "clear", None, reason));
        assert!(text.contains("settle refused: the reason"), "{text}");
    }
    assert_eq!(status_of(&rig, &id), "open");
}

#[test]
fn the_arguments_of_to_are_checked() {
    let rig = SRig::new(&["u1"], Some(10));
    let (id, rev) = rig.request("u1");
    let text = why(&rig.settle(&id, rev, "redispatch", None, WHY));
    assert!(
        text.contains("redispatch needs --to <stage>: a redispatch with no --to would be a clear"),
        "{text}"
    );
    let text = why(&rig.settle(&id, rev, "clear", Some("implement"), WHY));
    assert!(text.contains("--to belongs to redispatch only"), "{text}");
    let text = why(&rig.settle(&id, rev, "bogus", None, WHY));
    assert!(
        text.contains("the act \"bogus\" is none of clear, resume"),
        "{text}"
    );
}

#[test]
fn settle_is_not_on_the_device_allowlist_and_the_plain_handler_cannot_name_a_caller() {
    assert!(!pf_api::host::is_allowed("settle"));
    let rig = SRig::new(&["u1"], Some(10));
    let (id, rev) = rig.request("u1");
    let resp = rig.live.controller.handle(Request::new(
        "settle",
        settle_args(&id, rev, "clear", None, WHY),
    ));
    assert!(!resp.ok);
    assert!(
        resp.error
            .unwrap()
            .contains("the controller cannot tell which process called"),
    );
    assert_eq!(status_of(&rig, &id), "open");
}

#[test]
fn requests_json_carries_both_the_line_and_the_settled_record() {
    let rig = SRig::new(&["u1"], Some(10));
    let (id, rev) = rig.request("u1");
    let _ = answer_of(&rig.settle(&id, rev, "clear", None, WHY));
    let list: RequestList = serde_json::from_value(requests_json(&rig)).unwrap();
    let r = list.requests.iter().find(|r| r.id == id).unwrap();
    assert_eq!(r.line.as_deref(), Some("below"));
    let settled = r.settled.as_ref().unwrap();
    assert_eq!(settled.reason, WHY);
    assert_eq!(RequestStatus::Answered.as_str(), r.status);
}

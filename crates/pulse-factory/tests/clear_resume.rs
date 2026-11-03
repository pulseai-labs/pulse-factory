//! AC-7: `clear` and `answer` resume a parked unit in the same run. The driver runs on a
//! `FakeAdapter` and a bare forge, with a clock the test moves. The claim is taken on the engine,
//! as the controller's `claim` verb does.

mod common;

use std::sync::Arc;

use common::rig::*;
use common::*;
use pf_adapter_fake::{FakeAdapter, Scenario, Step};
use pf_engine::claim::sha256_hex;
use pf_engine::port::store::{RequestKind, RequestRow, RequestStatus};
use pf_engine::status::Status;
use pf_forge_bare::PrChecks;
use serde_json::{Value, json};

const PARK_ON: &str = "        park_on:
          - file: \"{run.dir}/out/{unit.id}/gaps.json\"
            reason: \"the item has gaps\"
          - file: \"{run.dir}/out/{unit.id}/correction.txt\"
            reason: \"the red gate needs a correction\"
";

fn claim(r: &Rig) -> u64 {
    r.engine
        .claim("ann@host", &sha256_hex(&[7u8; 32]))
        .unwrap()
        .generation
}

fn open_request(r: &Rig, unit: &str) -> RequestRow {
    r.view()
        .open_request_of(unit)
        .cloned()
        .unwrap_or_else(|| panic!("no open request for {unit}: {:?}", r.view().requests))
}

fn request(r: &Rig, id: &str) -> RequestRow {
    r.view()
        .requests
        .iter()
        .find(|q| q.id == id)
        .cloned()
        .unwrap()
}

fn stop_file(r: &Rig, unit: &str, text: &str) {
    std::fs::create_dir_all(r.out(unit, "")).unwrap();
    std::fs::write(r.out(unit, "stop"), text).unwrap();
}

/// A rig whose `u1` is parked by its stop file in `implement`.
fn parked_by_stop() -> (Rig, RequestRow) {
    let r = Rig::new();
    r.queue_item("u1", 1);
    r.tick();
    stop_file(&r, "u1", "need a human to look");
    r.tick();
    assert_eq!(r.unit("u1").0, Status::Parked);
    let req = open_request(&r, "u1");
    (r, req)
}

fn answer_json(r: &Rig, id: &str) -> Value {
    serde_json::from_str(&request(r, id).answer.expect("an answer")).unwrap()
}

// ------------------------------------------------------------------ clear

#[test]
fn a_stop_file_parks_and_a_clear_resumes_at_the_parked_stage() {
    let (r, req) = parked_by_stop();
    assert_eq!(req.kind, RequestKind::Stop);
    assert_eq!(req.stage, "implement");
    assert_eq!(req.status, RequestStatus::Open);
    let sessions_before = r.all_sessions("u1");

    let generation = claim(&r);
    r.driver
        .clear_unit("u1", None, generation, &req.id)
        .unwrap();
    // The operator removed the stop file: the clear does not.
    std::fs::remove_file(r.out("u1", "stop")).unwrap();
    assert_eq!(r.unit("u1").0, Status::Running);
    let answered = request(&r, &req.id);
    assert_eq!(answered.status, RequestStatus::Answered);
    assert_eq!(
        answer_json(&r, &req.id),
        json!({"verb": "clear", "to_stage": "implement"})
    );
    assert_eq!(answered.answer_generation, Some(generation));

    // No session yet: the next tick launches the stage through the normal path.
    assert_eq!(r.open_sessions("u1"), 0);
    r.tick();
    assert_eq!(r.open_sessions("u1"), 1);
    assert_eq!(r.all_sessions("u1"), sessions_before + 1);
    let stage = r.stage_node("u1", "implement");
    assert_eq!(stage.len(), 2, "{stage:?}");
    assert_eq!(stage[1].1, Status::Running);

    // The ledger names the generation, the request ID, the revision and the verb.
    let lines = r.ledger_lines("answer ");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].ends_with(&format!(
            "answer {}@1 by claim {generation}: clear u1 --to implement",
            req.id
        )),
        "{}",
        lines[0]
    );
}

#[test]
fn to_implement_launches_implement_and_an_earlier_stage_is_allowed() {
    let r = Rig::new();
    r.item("u1", 1, 5, PrChecks::Green);
    r.tick();
    // Pass implement; verify is running; park it there.
    r.pass("u1", &r.report("u1"));
    assert_eq!(r.stage_node("u1", "verify").len(), 1, "{}", r.ledger_text());
    stop_file(&r, "u1", "look at verify");
    r.tick();
    let req = open_request(&r, "u1");
    assert_eq!(req.stage, "verify");
    std::fs::remove_file(r.out("u1", "stop")).unwrap();

    let generation = claim(&r);
    r.driver
        .clear_unit("u1", Some("implement"), generation, &req.id)
        .unwrap();
    assert_eq!(
        answer_json(&r, &req.id),
        json!({"verb": "clear", "to_stage": "implement"})
    );
    // The report file of the first pass is still there, so the stage's done condition is met at
    // once; the launch is what the test checks.
    let before = r.stage_node("u1", "implement").len();
    r.tick();
    assert!(
        r.stage_node("u1", "implement").len() > before,
        "implement was launched again"
    );
    assert!(r.ledger_lines("clear u1 --to implement").len() == 1);
}

#[test]
fn an_unknown_stage_is_refused_and_nothing_changes() {
    let (r, req) = parked_by_stop();
    let generation = claim(&r);
    let ledger = r.ledger_text();
    let err = r
        .driver
        .clear_unit("u1", Some("no-such-stage"), generation, &req.id)
        .unwrap_err();
    assert!(err.contains("no-such-stage"), "{err}");
    assert_eq!(r.unit("u1").0, Status::Parked);
    assert_eq!(request(&r, &req.id).status, RequestStatus::Open);
    assert_eq!(r.ledger_text(), ledger, "a refusal writes no ledger line");
}

#[test]
fn a_clear_without_the_claim_is_refused() {
    let (r, req) = parked_by_stop();
    let ledger = r.ledger_text();
    // No claim was ever made.
    let err = r.driver.clear_unit("u1", None, 1, &req.id).unwrap_err();
    assert!(err.contains("claim 1 is not the current claim"), "{err}");
    assert_eq!(r.unit("u1").0, Status::Parked);

    // A claim by someone else fences generation 1: the old generation is refused.
    let g1 = claim(&r);
    let g2 = r
        .engine
        .claim("bob@host", &sha256_hex(&[9u8; 32]))
        .unwrap()
        .generation;
    assert_eq!((g1, g2), (1, 2));
    let err = r.driver.clear_unit("u1", None, g1, &req.id).unwrap_err();
    assert!(err.contains("claim 1 was fenced by claim 2"), "{err}");
    assert_eq!(request(&r, &req.id).status, RequestStatus::Open);
    assert!(!r.ledger_text()[ledger.len()..].contains("answer "));
}

#[test]
fn a_clear_names_the_request_it_answers() {
    let (r, req) = parked_by_stop();
    let generation = claim(&r);
    let err = r
        .driver
        .clear_unit("u1", None, generation, "R-99")
        .unwrap_err();
    assert!(err.contains(&req.id) && err.contains("R-99"), "{err}");
    assert_eq!(r.unit("u1").0, Status::Parked);
}

#[test]
fn a_cleared_unit_with_a_recorded_pr_keeps_it() {
    let r = Rig::new();
    r.item("u1", 1, 5, PrChecks::Red);
    r.reach_close("u1");
    r.close("u1", 5);
    // The checks are red: the merge does not land, the unit parks with the PR recorded.
    assert_eq!(
        r.unit("u1").0,
        Status::Parked,
        "ledger: {}",
        r.ledger_text()
    );
    let req = open_request(&r, "u1");
    assert_eq!(req.kind, RequestKind::MergeFailed);
    let pr_before = r
        .view()
        .unit_detail(r.view().node("u1").unwrap())
        .unwrap()
        .pr;
    assert!(pr_before.is_some(), "the PR is recorded");

    let generation = claim(&r);
    r.driver
        .clear_unit("u1", None, generation, &req.id)
        .unwrap();
    let node_view = r.view();
    let detail = node_view
        .unit_detail(node_view.node("u1").unwrap())
        .unwrap();
    assert_eq!(detail.pr, pr_before, "the PR survives the clear");
    assert_eq!(detail.park_generation, 1);
}

#[test]
fn a_halted_run_accepts_the_clear_and_launches_nothing() {
    let (r, req) = parked_by_stop();
    std::fs::remove_file(r.out("u1", "stop")).unwrap();
    r.engine.halt("kill switch").unwrap();
    let generation = claim(&r);
    let sessions = r.all_sessions("u1");

    r.driver
        .clear_unit("u1", None, generation, &req.id)
        .unwrap();
    assert_eq!(
        request(&r, &req.id).status,
        RequestStatus::Answered,
        "recorded"
    );
    assert_eq!(r.ledger_lines("clear u1 --to implement").len(), 1);
    for _ in 0..3 {
        r.tick();
    }
    assert_eq!(
        r.all_sessions("u1"),
        sessions,
        "nothing launched under the halt"
    );
    assert_eq!(r.open_sessions("u1"), 0);
}

#[test]
fn a_clear_on_a_run_whose_pinned_copy_changed_is_refused_naming_the_file() {
    let (r, req) = parked_by_stop();
    let generation = claim(&r);
    let brief = r.package_dir.join("briefs/common.md");
    std::fs::write(&brief, "# Common\nSomeone edited the brief.\n").unwrap();
    let ledger = r.ledger_text();

    let err = r
        .driver
        .clear_unit("u1", None, generation, &req.id)
        .unwrap_err();
    assert!(err.contains("briefs/common.md"), "{err}");
    assert_eq!(r.unit("u1").0, Status::Parked, "nothing changed");
    assert_eq!(request(&r, &req.id).status, RequestStatus::Open);
    assert_eq!(r.ledger_text(), ledger);
    // The answer is refused the same way.
    let err = r
        .driver
        .answer_request(&req.id, 1, generation, "go on")
        .unwrap_err();
    assert!(err.contains("briefs/common.md"), "{err}");
    assert_eq!(request(&r, &req.id).status, RequestStatus::Open);

    // With the copy as it was, the clear is admitted.
    std::fs::write(&brief, "# Common\nDo the work.\n").unwrap();
    r.driver
        .clear_unit("u1", None, generation, &req.id)
        .unwrap();
    assert_eq!(r.unit("u1").0, Status::Running);
}

// ------------------------------------------------------------------ a restart

#[test]
fn a_restart_on_a_new_adapter_parks_as_seat_lost_and_a_clear_relaunches() {
    let r = Rig::new();
    r.queue_item("u1", 1);
    r.tick();
    let old_row = r
        .view()
        .sessions
        .iter()
        .find(|s| s.unit_id == "u1" && !s.ended)
        .unwrap()
        .id
        .clone();

    let fake2 = Arc::new(FakeAdapter::new());
    for lane in ["lane", "lane.2", "lane.3"] {
        fake2.script(lane, Scenario::Script(vec![Step::TurnEnd]));
    }
    let (engine, driver) = r.restart_on(fake2.clone());
    let view = engine.view().unwrap();
    assert_eq!(view.node("u1").unwrap().status, Status::Parked);
    let req = view.open_request_of("u1").cloned().expect("a request");
    assert_eq!(req.kind, RequestKind::SeatLost);
    assert_eq!(req.stage, "implement");
    assert!(
        req.reason.to_lowercase().contains("seat lost"),
        "{}",
        req.reason
    );
    assert!(
        engine
            .requests()
            .unwrap()
            .iter()
            .all(|q| q.status != RequestStatus::Answered)
    );

    // The run sits with nothing to do until the claim decides: ticks launch nothing.
    driver.tick();
    driver.tick();
    assert!(
        engine.view().unwrap().sessions.iter().all(|s| s.ended),
        "no session is open before the clear"
    );

    let generation = engine
        .claim("ann@host", &sha256_hex(&[7u8; 32]))
        .unwrap()
        .generation;
    driver.clear_unit("u1", None, generation, &req.id).unwrap();
    driver.tick();
    let view = engine.view().unwrap();
    let open: Vec<_> = view
        .sessions
        .iter()
        .filter(|s| s.unit_id == "u1" && !s.ended)
        .collect();
    assert_eq!(open.len(), 1, "a new session");
    assert_ne!(open[0].id, old_row, "a new session row");
    assert_eq!(
        view.sessions.iter().filter(|s| s.unit_id == "u1").count(),
        2,
        "the lost seat's row stays, ended"
    );
    assert_eq!(view.node("u1").unwrap().status, Status::Running);
}

// ------------------------------------------------------------------ answer

#[test]
fn answer_text_resumes_the_unit_and_the_text_stays_literal() {
    let (r, req) = parked_by_stop();
    std::fs::remove_file(r.out("u1", "stop")).unwrap();
    let generation = claim(&r);
    let sentinel = r.root.path().join("sentinel");
    let text = format!(
        "use {{param.repo}} and $(rm -rf x) and `touch {s}` and $(touch {s})",
        s = sentinel.display()
    );

    r.driver
        .answer_request(&req.id, 1, generation, &text)
        .unwrap();
    assert_eq!(r.unit("u1").0, Status::Running, "the unit resumes");
    let answer = answer_json(&r, &req.id);
    assert_eq!(answer["verb"], "answer");
    assert_eq!(answer["text"], text.as_str(), "stored as given");
    assert!(!sentinel.exists(), "nothing ran the text");

    let line = &r.ledger_lines("answer ")[0];
    assert!(
        line.contains(&format!(
            "answer {}@1 by claim {generation}: use {{param.repo}} and $(rm -rf x)",
            req.id
        )),
        "{line}"
    );
    // It resumes at the parked stage, like a clear.
    r.tick();
    assert_eq!(r.open_sessions("u1"), 1);
    assert_eq!(r.stage_node("u1", "implement").len(), 2);
}

#[test]
fn the_ledger_line_of_an_answer_is_one_line_cut_and_redacted() {
    let (r, req) = parked_by_stop();
    std::fs::remove_file(r.out("u1", "stop")).unwrap();
    let generation = claim(&r);
    let token = "ghp_0123456789abcdefghijklmnopqrstuvwxyz";
    let text = format!("first\nsecond token={token} {}", "x".repeat(400));
    r.driver
        .answer_request(&req.id, 1, generation, &text)
        .unwrap();
    let line = r.ledger_lines("answer ")[0].clone();
    assert!(!line.contains(token), "{line}");
    let body = line.split_once(": ").unwrap().1;
    assert!(body.starts_with("first / second"), "{line}");
    assert!(
        body.chars().count() <= 200 + 1,
        "{} chars: {line}",
        body.chars().count()
    );
    assert!(r.ledger_text().lines().all(|l| !l.is_empty()));
    // The request keeps the whole text, as data.
    assert_eq!(answer_json(&r, &req.id)["text"], text.as_str());
}

#[test]
fn an_answer_at_another_revision_is_refused_naming_the_current_one() {
    let (r, req) = parked_by_stop();
    let generation = claim(&r);
    let err = r
        .driver
        .answer_request(&req.id, 7, generation, "go on")
        .unwrap_err();
    assert!(err.contains('7') && err.contains('1'), "{err}");
    assert_eq!(r.unit("u1").0, Status::Parked);
}

// ------------------------------------------------------------------ the kinds, through the driver

fn park_on_rig() -> Rig {
    let o = Options {
        implement_extra: PARK_ON.to_string(),
        ..Options::default()
    };
    let r = Rig::with(&o, |_| {});
    r.queue_item("u1", 1);
    r.tick();
    r
}

#[test]
fn a_gaps_file_opens_a_gaps_request() {
    let r = park_on_rig();
    r.touch(&r.out("u1", "gaps.json"));
    r.tick();
    assert_eq!(r.unit("u1").0, Status::Parked);
    let req = open_request(&r, "u1");
    assert_eq!(req.kind, RequestKind::Gaps);
    assert_eq!(req.reason, "the item has gaps");
}

#[test]
fn a_correction_file_opens_a_red_gate_request() {
    let r = park_on_rig();
    r.touch(&r.out("u1", "correction.txt"));
    r.tick();
    assert_eq!(r.unit("u1").0, Status::Parked);
    let req = open_request(&r, "u1");
    assert_eq!(req.kind, RequestKind::RedGate);
    assert_eq!(req.reason, "the red gate needs a correction");
}

#[test]
fn a_second_stall_opens_a_stall_request() {
    let o = Options {
        implement_stall: 1,
        ..Options::default()
    };
    let r = Rig::with(&o, |_| {});
    r.queue_item("u1", 1);
    r.tick();
    r.tick();
    for _ in 0..4 {
        if r.unit("u1").0 == Status::Parked {
            break;
        }
        r.advance(100);
        r.tick();
    }
    assert_eq!(
        r.unit("u1").0,
        Status::Parked,
        "ledger: {}",
        r.ledger_text()
    );
    let req = open_request(&r, "u1");
    assert_eq!(req.kind, RequestKind::Stall);
    assert!(req.reason.starts_with("stalled:"), "{}", req.reason);
}

#[test]
fn a_stop_file_after_a_clear_opens_a_second_request() {
    let (r, req) = parked_by_stop();
    let generation = claim(&r);
    r.driver
        .clear_unit("u1", None, generation, &req.id)
        .unwrap();
    // The stop file is still there: the next tick launches, then stops the seat again.
    r.tick();
    r.tick();
    assert_eq!(r.unit("u1").0, Status::Parked);
    let second = open_request(&r, "u1");
    assert_ne!(second.id, req.id);
    assert_eq!(second.kind, RequestKind::Stop);
    assert_eq!(request(&r, &req.id).status, RequestStatus::Answered);
}

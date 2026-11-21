//! AC-6 of `r2.s11.w2`: a stage declares a fallback `on_answer`, so a text typed on any park of it
//! reaches the next seat as a note. The real package's `rule.sh` is the hook, with the stop path
//! `-` as the shipped package passes it: the entry quotes no file and renames none. A stage with no
//! fallback takes no text and the refusal names `clear` (Q5). The driver runs on the fake adapter
//! and a bare forge; the test plays the seat.

mod common;

use std::path::Path;

use common::rig::*;
use common::*;
use pf_engine::Cause;
use pf_engine::claim::{AnswerAct, sha256_hex};
use pf_engine::port::store::{RequestKind, RequestRow, RequestStatus};
use pf_engine::status::Status;
use pf_forge_bare::PrChecks;
use serde_json::{Value, json};

/// The fallback of the shipped package, for the stage `stage`.
fn fallback(stage: &str) -> String {
    format!(
        "        on_answer:\n          - [sh, \"{{run.package}}/scripts/rule.sh\", \"{{run.dir}}/out/{{unit.id}}/rulings.md\", \"-\", \"{{answer.file}}\", \"{{answer.request}}\", \"{{answer.generation}}\", {stage}, \"{{answer.source}}\"]\n"
    )
}

/// The stop hook of the shipped package.
const STOP_HOOK: &str = r#"        stop_file:
          file: "{run.dir}/out/{unit.id}/stop"
          on_answer:
            - [sh, "{run.package}/scripts/rule.sh", "{run.dir}/out/{unit.id}/rulings.md", "{run.dir}/out/{unit.id}/stop", "{answer.file}", "{answer.request}", "{answer.generation}", implement, "{answer.source}"]
"#;

/// A rig whose stage `implement` has the fallback.
fn rig_with_fallback() -> Rig {
    let o = Options {
        implement_extra: fallback("implement"),
        scripts_from: Some(real_scripts_dir()),
        ..Options::default()
    };
    Rig::with(&o, |_| {})
}

/// A rig whose stage `implement` has none.
fn rig_without_fallback() -> Rig {
    let o = Options {
        scripts_from: Some(real_scripts_dir()),
        ..Options::default()
    };
    Rig::with(&o, |_| {})
}

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
        .unwrap_or_else(|| panic!("no open request for {unit}: {}", r.ledger_text()))
}

fn request(r: &Rig, id: &str) -> RequestRow {
    r.view()
        .requests
        .iter()
        .find(|q| q.id == id)
        .cloned()
        .unwrap()
}

fn block_of(request: &RequestRow) -> Value {
    let body: Value = serde_json::from_str(&request.body).unwrap();
    body["decision"].clone()
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Park `u1` as a stall and return its open request.
fn stall(r: &Rig) -> RequestRow {
    r.queue_item("u1", 1);
    r.tick();
    r.engine
        .park("u1", Cause::Stalled, "stalled: no output for 20 minutes")
        .unwrap();
    r.tick();
    assert_eq!(r.unit("u1").0, Status::Parked, "{}", r.ledger_text());
    open_request(r, "u1")
}

/// The heading line of the one entry of the rulings file, and the lines after it.
fn entry(r: &Rig) -> (String, Vec<String>) {
    let text = read(&r.out("u1", "rulings.md"));
    let mut lines = text.lines().filter(|l| !l.is_empty());
    let heading = lines.next().expect("a heading").to_string();
    (heading, lines.map(str::to_string).collect())
}

#[test]
fn a_typed_answer_on_a_stall_is_a_note_that_quotes_no_file_and_resumes_the_unit() {
    let r = rig_with_fallback();
    let req = stall(&r);
    assert_eq!(block_of(&req)["cause"], "stalled");
    let generation = claim(&r);
    let answered = r
        .driver
        .answer_request(&req.id, 1, generation, "the build is flaky: run it twice")
        .unwrap();
    assert!(
        answered.hook_ran && answered.hook_failed.is_none(),
        "{answered:?}"
    );
    assert_eq!(r.unit("u1").0, Status::Running, "{}", r.ledger_text());
    let (heading, body) = entry(&r);
    // The heading names the request, the stage and the claim; a typed answer names no option.
    assert!(
        heading.starts_with(&format!("## {} — ", req.id)),
        "{heading}"
    );
    assert!(
        heading.ends_with(" — stage implement — claim 1"),
        "{heading}"
    );
    // No file is quoted: the note is the only line below the heading.
    assert_eq!(body, ["the build is flaky: run it twice"]);
    assert!(
        !body.iter().any(|l| l.starts_with('>')),
        "no quoted file: {body:?}"
    );
    assert_eq!(request(&r, &req.id).status, RequestStatus::Answered);
}

#[test]
fn a_typed_answer_on_a_cap_park_is_a_note_too() {
    let o = Options {
        review_loop: true,
        review_extra: fallback("review"),
        scripts_from: Some(real_scripts_dir()),
        ..Options::default()
    };
    let r = Rig::with(&o, |_| {});
    r.item("u1", 1, 1, PrChecks::Green);
    r.reach_close("u1");
    r.script("u1", vec![pf_adapter_fake::Step::TurnEnd]);
    r.close("u1", 1);
    let old = git(&r.bare, &["rev-parse", "refs/heads/spine"]);
    // Three passes that quote an old head: each sends the unit to `review` again, up to its cap.
    for _ in 0..3 {
        let text = format!("verdict: pass\nhead: {old}\n\n- a finding\n");
        std::fs::write(r.out("u1", "review.md"), text).unwrap();
        r.fake.finish(&r.session_ref("u1").expect("an open review"));
        r.tick();
    }
    assert_eq!(r.unit("u1").0, Status::Parked, "{}", r.ledger_text());
    let req = open_request(&r, "u1");
    assert_eq!(block_of(&req)["cause"], "outcome_cap");
    assert_eq!(block_of(&req)["takes_text"], true);
    let generation = claim(&r);
    let answered = r
        .driver
        .answer_request(&req.id, 1, generation, "accept the stale head")
        .unwrap();
    assert!(
        answered.hook_ran && answered.hook_failed.is_none(),
        "{answered:?}"
    );
    let (heading, body) = entry(&r);
    assert!(heading.ends_with(" — stage review — claim 1"), "{heading}");
    assert_eq!(body, ["accept the stale head"]);
    assert_ne!(r.unit("u1").0, Status::Parked, "{}", r.ledger_text());
}

#[test]
fn a_seats_option_taken_on_a_stop_writes_the_heading_that_says_the_seat_wrote_it() {
    let o = Options {
        implement_stop: STOP_HOOK.to_string(),
        scripts_from: Some(real_scripts_dir()),
        ..Options::default()
    };
    let r = Rig::with(&o, |_| {});
    r.queue_item("u1", 1);
    r.tick();
    std::fs::create_dir_all(r.out("u1", "")).unwrap();
    std::fs::write(
        r.out("u1", "stop"),
        "Which branch?\nOptions:\n- build on main\n- build on the spine branch\n",
    )
    .unwrap();
    r.tick();
    let req = open_request(&r, "u1");
    assert_eq!(req.kind, RequestKind::Stop);
    let generation = claim(&r);
    r.driver
        .answer_act(&req.id, 1, generation, &AnswerAct::Option("s1"))
        .unwrap();
    let (heading, body) = entry(&r);
    assert!(
        heading.ends_with(" — stage implement — claim 1 — option s1, written by the seat"),
        "{heading}"
    );
    // The stop text is quoted, as for a typed answer, and then the option's words.
    assert!(body.iter().any(|l| l == "> Which branch?"), "{body:?}");
    assert_eq!(body.last().unwrap(), "build on main");
    assert_eq!(r.unit("u1").0, Status::Running);
}

#[test]
fn a_stage_with_no_fallback_refuses_the_text_and_records_nothing() {
    let r = rig_without_fallback();
    let req = stall(&r);
    assert_eq!(block_of(&req)["takes_text"], false);
    let generation = claim(&r);
    let err = r
        .driver
        .answer_request(&req.id, 1, generation, "go on")
        .unwrap_err();
    let want = format!(
        "request {} takes no text: no answer hook of stage implement would deliver it to a seat; resume the unit with pulse-factory clear ",
        req.id
    );
    assert!(err.starts_with(&want) && err.ends_with(" u1"), "{err}");
    assert_eq!(request(&r, &req.id).status, RequestStatus::Open);
    assert!(request(&r, &req.id).answer.is_none());
    assert_eq!(r.unit("u1").0, Status::Parked);
    assert!(!r.out("u1", "rulings.md").exists());
    assert!(r.ledger_lines("answer ").is_empty(), "{}", r.ledger_text());
    // `clear` is the way to resume it.
    r.driver
        .clear_unit("u1", None, generation, &req.id)
        .unwrap();
    assert_eq!(r.unit("u1").0, Status::Running);
}

#[test]
fn a_request_with_no_unit_still_takes_a_text() {
    let r = rig_without_fallback();
    r.queue_item("u1", 1);
    r.tick();
    r.engine
        .open_request(None, RequestKind::Stop, "", "run level", &json!({}))
        .unwrap();
    let id = r
        .view()
        .requests
        .iter()
        .find(|q| q.unit_id.is_none())
        .unwrap()
        .id
        .clone();
    let generation = claim(&r);
    let answered = r
        .driver
        .answer_request(&id, 1, generation, "noted")
        .unwrap();
    assert!(!answered.hook_ran, "{answered:?}");
    assert_eq!(request(&r, &id).status, RequestStatus::Answered);
}

#[test]
fn the_block_of_each_park_says_takes_text_by_the_stage() {
    // A stage with the fallback: a stall says `takes_text: true`.
    let with = rig_with_fallback();
    let stalled = stall(&with);
    assert_eq!(block_of(&stalled)["takes_text"], true);
    assert!(
        block_of(&stalled)["options"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["id"] == "resume"),
        "a stall keeps its resume option: {}",
        block_of(&stalled)
    );
    // A stage without it: `false`.
    let without = rig_without_fallback();
    let stalled = stall(&without);
    assert_eq!(block_of(&stalled)["takes_text"], false);
}

#[test]
fn a_failing_fallback_parks_the_unit_again_with_answer_hook_failed() {
    // The hook is `rule.sh` with a stage name it refuses: it exits 1 and the unit parks again.
    let o = Options {
        implement_extra: fallback("bad stage"),
        scripts_from: Some(real_scripts_dir()),
        ..Options::default()
    };
    let r = Rig::with(&o, |_| {});
    let req = stall(&r);
    let generation = claim(&r);
    let answered = r
        .driver
        .answer_request(&req.id, 1, generation, "go on")
        .unwrap();
    let cause = answered.hook_failed.expect("the hook failed");
    assert!(cause.contains("stage name"), "{cause}");
    assert_eq!(request(&r, &req.id).status, RequestStatus::Answered);
    let (status, reason) = r.unit("u1");
    assert_eq!(status, Status::Parked);
    assert!(
        reason
            .as_deref()
            .unwrap()
            .starts_with("answer hook failed: "),
        "{reason:?}"
    );
    let next = open_request(&r, "u1");
    assert_ne!(next.id, req.id);
    assert!(
        next.reason.starts_with("answer hook failed: "),
        "{}",
        next.reason
    );
    assert!(!r.out("u1", "rulings.md").exists());
}

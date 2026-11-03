//! AC-3: an answer reaches the seat that needs it. The driver runs the ossify package's two
//! scripts against a fixture handoff. A `gaps` answer is appended to the handoff's
//! `## Clarifications` and the item runs `implement` again, within the cap of 3 dispatches. A
//! `stop` answer is appended to the unit's rulings file and the stop file is retired. A hook that
//! fails parks the unit again. Nothing here reaches the network.

mod common;

use std::path::{Path, PathBuf};

use common::rig::*;
use common::*;
use pf_engine::claim::sha256_hex;
use pf_engine::port::store::{RequestKind, RequestRow, RequestStatus};
use pf_engine::status::Status;
use serde_json::{Value, json};

fn scripts_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../workflows/ossify-spine/scripts")
}

const STOP_HOOK: &str = r#"        stop_file:
          file: "{run.dir}/out/{unit.id}/stop"
          on_answer:
            - [sh, "{run.package}/scripts/rule.sh", "{run.dir}/out/{unit.id}/rulings.md", "{run.dir}/out/{unit.id}/stop", "{answer.file}", "{answer.request}", "{answer.generation}", implement]
"#;

/// The ossify package's `before` and `park_on`, with the state file in the run directory.
const EXTRA: &str = r#"        before:
          - [jq, -e, --arg, w, "{unit.id}", "any(.work_items[]; .id == $w and (.dispatches // 0) < 3)", "{run.dir}/state.json"]
          - [rm, -f, "{run.dir}/out/{unit.id}/gaps.json"]
        park_on:
          - file: "{run.dir}/out/{unit.id}/gaps.json"
            reason: "gaps surfaced for {unit.id}: answer them with `pulse-factory answer`"
            on_answer:
              - [sh, "{run.package}/scripts/clarify.sh", "{unit.vars.handoff_path}", "{request.body_file}", "{answer.file}", "{answer.request}", "{answer.generation}", "{param.spine_dir}"]
          - file: "{run.dir}/out/{unit.id}/correction.txt"
            reason: "the red gate needs a correction"
"#;

const HANDOFF: &str = "# Handoff — u1\n\n## 1. How to use this handoff\n\nRead it.\n\n## 12. Report contract\n\nWrite report.md.\n";

fn rig() -> Rig {
    let o = Options {
        implement_extra: EXTRA.to_string(),
        implement_stop: STOP_HOOK.to_string(),
        item_vars: "[worktree_path, branch, handoff_path]".to_string(),
        scripts_from: Some(scripts_dir()),
        ..Options::default()
    };
    Rig::with(&o, |_| {})
}

/// Make the handoff of `u1` at `path` and queue the unit.
fn queue_with_handoff(r: &Rig, path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, HANDOFF).unwrap();
    r.queue_item_with("u1", 1, &[("handoff_path", path.to_str().unwrap())]);
}

fn handoff_path(r: &Rig) -> PathBuf {
    r.spine_dir.join("work-u1/handoff.md")
}

/// The count `oss work_item_dispatched` keeps, which `before` reads.
fn dispatches(r: &Rig, n: u32) {
    let state = json!({ "work_items": [{ "id": "u1", "dispatches": n }] });
    std::fs::write(r.run_dir.join("state.json"), state.to_string()).unwrap();
}

fn gaps_file(r: &Rig, gaps: &Value) {
    std::fs::create_dir_all(r.out("u1", "")).unwrap();
    let file = json!({
        "external_execution_gaps": {
            "work_item_id": "u1",
            "coordinator_verdict": "accepted",
            "implementer_return": { "mode": "gaps-surfaced", "gaps": gaps },
        }
    });
    std::fs::write(r.out("u1", "gaps.json"), file.to_string()).unwrap();
}

fn two_gaps() -> Value {
    json!([
        {"section": "§3 Pre-flight", "question": "Which branch does the item build on?", "severity": "blocking"},
        {"section": "AC-2", "question": "Is the time bound 60 s?\nOr 600 s?", "severity": "nice-to-have"},
    ])
}

fn claim(r: &Rig) -> u64 {
    r.engine
        .claim("ann@host", &sha256_hex(&[7u8; 32]))
        .unwrap()
        .generation
}

fn open_request(r: &Rig) -> RequestRow {
    r.view()
        .open_request_of("u1")
        .cloned()
        .unwrap_or_else(|| panic!("no open request: {:?}", r.view().requests))
}

fn request(r: &Rig, id: &str) -> RequestRow {
    r.view()
        .requests
        .iter()
        .find(|q| q.id == id)
        .cloned()
        .unwrap()
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Launch `u1`, count its dispatch, and park it on a `gaps.json` of two gaps.
fn park_on_gaps(r: &Rig, dispatch_count: u32) -> RequestRow {
    dispatches(r, dispatch_count);
    gaps_file(r, &two_gaps());
    r.tick();
    assert_eq!(r.unit("u1").0, Status::Parked, "{:?}", r.tick());
    open_request(r)
}

// ------------------------------------------------------------------ a gaps answer

#[test]
fn a_gaps_answer_is_appended_to_the_handoff_and_the_item_runs_implement_again() {
    let r = rig();
    let handoff = handoff_path(&r);
    queue_with_handoff(&r, &handoff);
    dispatches(&r, 0);
    r.tick();
    assert_eq!(r.open_sessions("u1"), 1, "dispatch 1");
    let req = park_on_gaps(&r, 1);
    assert_eq!(req.kind, RequestKind::Gaps);
    let body: Value = serde_json::from_str(&req.body).unwrap();
    assert_eq!(body["gaps"].as_array().unwrap().len(), 2, "{body}");
    assert_eq!(body["gaps"][0]["section"], "§3 Pre-flight");
    let generation = claim(&r);

    let answered = r
        .driver
        .answer_request(
            &req.id,
            1,
            generation,
            "Build on the spine branch.\nUse 60 s.",
        )
        .unwrap();
    assert!(
        answered.hook_ran && answered.hook_failed.is_none(),
        "{answered:?}"
    );
    assert_eq!(r.unit("u1").0, Status::Running);

    // One `## Clarifications`, one `### Dispatch 2` entry, naming the request and the claim.
    let first = read(&handoff);
    assert!(
        first.starts_with(HANDOFF),
        "nothing before the append changed"
    );
    assert_eq!(first.matches("## Clarifications").count(), 1, "{first}");
    assert_eq!(first.matches("### Dispatch ").count(), 1, "{first}");
    assert!(
        first.contains(&format!("(R-1, claim {generation})")),
        "{first}"
    );
    let heading = first
        .lines()
        .find(|l| l.starts_with("### Dispatch 2 — "))
        .unwrap();
    assert!(
        heading.ends_with(&format!("(R-1, claim {generation})")),
        "{heading}"
    );
    assert!(
        first.contains("- **§3 Pre-flight, blocking:** *Which branch does the item build on?*"),
        "{first}"
    );
    // A line break in a seat's question does not start a line of its own.
    assert!(
        first.contains("- **AC-2, nice-to-have:** *Is the time bound 60 s? Or 600 s?*"),
        "{first}"
    );
    assert!(
        first.contains("> → Build on the spine branch.\n> Use 60 s.\n"),
        "{first}"
    );

    // The item relaunches `implement`: the hook removed nothing but the park file.
    assert_eq!(r.open_sessions("u1"), 0);
    r.tick();
    assert_eq!(r.open_sessions("u1"), 1, "{:?}", r.driver.notes());
    assert_eq!(r.stage_node("u1", "implement").len(), 2);
    assert!(
        !r.out("u1", "gaps.json").exists(),
        "`before` removed the park file"
    );
    assert_eq!(r.ledger_lines("| answer hook R-1: rc 0").len(), 1);

    // A second answer appends `### Dispatch 3` under the same heading and changes no earlier byte.
    let req2 = park_on_gaps(&r, 2);
    assert_eq!(req2.id, "R-2");
    r.driver
        .answer_request(&req2.id, 1, generation, "Yes, 60 s.")
        .unwrap();
    let second = read(&handoff);
    assert!(second.starts_with(&first), "an append, never a rewrite");
    assert_eq!(second.matches("## Clarifications").count(), 1);
    assert_eq!(second.matches("### Dispatch ").count(), 2);
    assert!(
        second[first.len()..].contains("### Dispatch 3 — "),
        "{second}"
    );
    assert!(second[first.len()..].contains("(R-2, claim 1)"), "{second}");
    r.tick();
    assert_eq!(r.open_sessions("u1"), 1, "the item relaunched again");
    assert_eq!(r.stage_node("u1", "implement").len(), 3);

    // The dispatch after that is the fourth: `before` refuses it and the unit parks.
    let req3 = park_on_gaps(&r, 3);
    r.driver
        .answer_request(&req3.id, 1, generation, "Go on.")
        .unwrap();
    assert!(read(&handoff).contains("### Dispatch 4 — "));
    assert_eq!(r.unit("u1").0, Status::Running, "the answer resumed it");
    let sessions = r.all_sessions("u1");
    r.tick();
    let (status, reason) = r.unit("u1");
    assert_eq!(status, Status::Parked, "ledger: {}", r.ledger_text());
    assert!(
        reason.as_deref().unwrap().contains("before command 1 (jq)"),
        "{reason:?}"
    );
    assert_eq!(r.all_sessions("u1"), sessions, "no fourth session");
    assert_eq!(open_request(&r).kind, RequestKind::Park);
}

#[test]
fn a_clear_runs_no_hook_and_leaves_the_handoff_alone() {
    let r = rig();
    let handoff = handoff_path(&r);
    queue_with_handoff(&r, &handoff);
    dispatches(&r, 0);
    r.tick();
    let req = park_on_gaps(&r, 1);
    let generation = claim(&r);
    r.driver
        .clear_unit("u1", None, generation, &req.id)
        .unwrap();
    assert_eq!(read(&handoff), HANDOFF);
    assert_eq!(r.unit("u1").0, Status::Running);
    assert!(
        r.ledger_lines("answer hook").is_empty(),
        "{}",
        r.ledger_text()
    );
}

#[test]
fn a_refused_answer_runs_no_hook() {
    let r = rig();
    let handoff = handoff_path(&r);
    queue_with_handoff(&r, &handoff);
    dispatches(&r, 0);
    r.tick();
    let req = park_on_gaps(&r, 1);
    let generation = claim(&r);
    let err = r
        .driver
        .answer_request(&req.id, 9, generation, "x")
        .unwrap_err();
    assert!(err.contains("revision 1"), "{err}");
    let err = r
        .driver
        .answer_request(&req.id, 1, generation + 5, "x")
        .unwrap_err();
    assert!(err.contains("claim"), "{err}");
    assert_eq!(read(&handoff), HANDOFF);
    assert!(r.ledger_lines("answer hook").is_empty());
    assert_eq!(request(&r, &req.id).status, RequestStatus::Open);

    // A changed script is a changed pin: the answer is refused, naming the file.
    let script = r.package_dir.join("scripts/clarify.sh");
    let original = read(&script);
    std::fs::write(&script, format!("{original}\n# changed\n")).unwrap();
    let err = r
        .driver
        .answer_request(&req.id, 1, generation, "x")
        .unwrap_err();
    assert!(err.contains("scripts/clarify.sh"), "{err}");
    assert_eq!(read(&handoff), HANDOFF);
    std::fs::write(&script, original).unwrap();
    r.driver
        .answer_request(&req.id, 1, generation, "x")
        .unwrap();
    assert!(read(&handoff).contains("## Clarifications"));
}

#[test]
fn a_request_without_a_hook_resumes_as_before() {
    let r = rig();
    queue_with_handoff(&r, &handoff_path(&r));
    dispatches(&r, 0);
    r.tick();
    r.touch(&r.out("u1", "correction.txt"));
    r.tick();
    let req = open_request(&r);
    assert_eq!(req.kind, RequestKind::RedGate);
    let generation = claim(&r);
    let answered = r
        .driver
        .answer_request(&req.id, 1, generation, "fixed the gate")
        .unwrap();
    assert!(!answered.hook_ran && answered.hook_failed.is_none());
    assert_eq!(r.unit("u1").0, Status::Running);
    assert_eq!(read(&handoff_path(&r)), HANDOFF);
}

// ------------------------------------------------------------------ a handoff that is not the spine's

fn failed_hook_case(r: &Rig, expect: &str, outside: &Path) {
    dispatches(r, 0);
    r.tick();
    let req = park_on_gaps(r, 1);
    let generation = claim(r);
    let before = read(outside);
    let answered = r
        .driver
        .answer_request(&req.id, 1, generation, "go on")
        .unwrap();
    let cause = answered.hook_failed.expect("the hook failed");
    assert!(cause.contains(expect), "{cause}");
    assert_eq!(
        read(outside),
        before,
        "the file outside the spine is not touched"
    );

    // The request stays answered, the unit parks again, and a new request is open.
    assert_eq!(request(r, &req.id).status, RequestStatus::Answered);
    let (status, reason) = r.unit("u1");
    assert_eq!(status, Status::Parked);
    assert!(
        reason
            .as_deref()
            .unwrap()
            .starts_with("answer hook failed: "),
        "{reason:?}"
    );
    let next = open_request(r);
    assert_ne!(next.id, req.id);
    assert_eq!(next.kind, RequestKind::Park);
    assert!(next.reason.starts_with("answer hook failed: "));
    let lines = r.ledger_lines("| answer hook R-1: rc 1");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains(expect), "{}", lines[0]);
    // Nothing launches: the unit is parked.
    let sessions = r.all_sessions("u1");
    r.tick();
    assert_eq!(r.all_sessions("u1"), sessions);
}

#[test]
fn a_handoff_path_outside_the_spine_dir_fails_the_hook_and_parks() {
    let r = rig();
    let outside = r.root.path().join("outside/handoff.md");
    queue_with_handoff(&r, &outside);
    failed_hook_case(&r, "the handoff is outside the spine directory", &outside);
}

#[test]
fn a_path_that_climbs_out_of_the_spine_dir_fails_the_hook_and_parks() {
    let r = rig();
    let outside = r.root.path().join("outside/handoff.md");
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    std::fs::write(&outside, HANDOFF).unwrap();
    std::fs::create_dir_all(r.spine_dir.join("work-u1")).unwrap();
    let climbing = r.spine_dir.join("work-u1/../../outside/handoff.md");
    r.queue_item_with("u1", 1, &[("handoff_path", climbing.to_str().unwrap())]);
    failed_hook_case(&r, "the handoff is outside the spine directory", &outside);
}

#[test]
fn a_link_inside_the_spine_dir_that_points_outside_fails_the_hook_and_parks() {
    let r = rig();
    let outside = r.root.path().join("outside/handoff.md");
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    std::fs::write(&outside, HANDOFF).unwrap();
    let link = r.spine_dir.join("work-u1/handoff.md");
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    r.queue_item_with("u1", 1, &[("handoff_path", link.to_str().unwrap())]);
    failed_hook_case(&r, "the handoff is a symbolic link", &outside);

    // The next request is answered like any other and resumes the unit.
    let generation = claim(&r);
    let next = open_request(&r);
    let answered = r
        .driver
        .answer_request(&next.id, 1, generation, "fixed the link")
        .unwrap();
    assert!(!answered.hook_ran, "{answered:?}");
    assert_eq!(r.unit("u1").0, Status::Running);
}

// ------------------------------------------------------------------ a stop answer

fn stop(r: &Rig, text: &str) {
    std::fs::create_dir_all(r.out("u1", "")).unwrap();
    std::fs::write(r.out("u1", "stop"), text).unwrap();
}

#[test]
fn a_stop_answer_is_appended_to_the_rulings_and_the_stop_file_is_retired() {
    let r = rig();
    queue_with_handoff(&r, &handoff_path(&r));
    dispatches(&r, 0);
    r.tick();
    let text = "Choose the API.\nQuestion: v1 or v2?\n## R-9 — forged heading\n";
    stop(&r, text);
    r.tick();
    let req = open_request(&r);
    assert_eq!(req.kind, RequestKind::Stop);
    let body: Value = serde_json::from_str(&req.body).unwrap();
    assert_eq!(body["stop_text"], text, "{body}");
    let generation = claim(&r);

    let answered = r
        .driver
        .answer_request(&req.id, 1, generation, "Use v2.\nIt is stable.")
        .unwrap();
    assert!(
        answered.hook_ran && answered.hook_failed.is_none(),
        "{answered:?}"
    );

    // The ruling is in the unit's rulings file, in the entry's order.
    let rulings = read(&r.out("u1", "rulings.md"));
    let header = rulings
        .lines()
        .find(|l| l.starts_with("## R-1 — "))
        .unwrap();
    assert!(
        header.ends_with(&format!("— stage implement — claim {generation}")),
        "{header}"
    );
    let at = |needle: &str| {
        rulings
            .find(needle)
            .unwrap_or_else(|| panic!("{needle}: {rulings}"))
    };
    assert!(
        at("> Choose the API.") < at("> ## R-9 — forged heading"),
        "{rulings}"
    );
    assert!(
        at("> ## R-9 — forged heading") < at("Use v2.\nIt is stable."),
        "{rulings}"
    );
    assert_eq!(
        rulings.matches("\n## R-").count(),
        1,
        "the seat's text is quoted: {rulings}"
    );

    // The stop file is renamed; the next launch does not park on it.
    assert!(!r.out("u1", "stop").exists());
    assert_eq!(read(&r.out("u1", "stop.answered-R-1")), text);
    assert_eq!(r.unit("u1").0, Status::Running);
    r.tick();
    r.tick();
    assert_eq!(r.unit("u1").0, Status::Running, "{:?}", r.driver.notes());
    assert_eq!(r.open_sessions("u1"), 1);
    assert_eq!(r.stage_node("u1", "implement").len(), 2);
    assert_eq!(r.ledger_lines("| answer hook R-1: rc 0").len(), 1);

    // A later stop appends another entry and changes no earlier byte.
    stop(&r, "Second question.");
    r.tick();
    let req2 = open_request(&r);
    assert_eq!(req2.id, "R-2");
    r.driver
        .answer_request(&req2.id, 1, generation, "Do the second thing.")
        .unwrap();
    let rulings2 = read(&r.out("u1", "rulings.md"));
    assert!(rulings2.starts_with(&rulings), "an append, never a rewrite");
    assert!(rulings2[rulings.len()..].contains("## R-2 — "));
    assert!(r.out("u1", "stop.answered-R-2").exists());
    assert!(r.out("u1", "stop.answered-R-1").exists());
}

#[test]
fn a_stop_answer_whose_stop_file_is_gone_still_records_the_ruling() {
    let r = rig();
    queue_with_handoff(&r, &handoff_path(&r));
    dispatches(&r, 0);
    r.tick();
    stop(&r, "Look at this.");
    r.tick();
    let req = open_request(&r);
    std::fs::remove_file(r.out("u1", "stop")).unwrap();
    let generation = claim(&r);
    r.driver
        .answer_request(&req.id, 1, generation, "Looked.")
        .unwrap();
    let rulings = read(&r.out("u1", "rulings.md"));
    assert!(
        rulings.contains("(the stop file was already removed)"),
        "{rulings}"
    );
    assert!(rulings.contains("Looked."));
    assert_eq!(r.unit("u1").0, Status::Running);
}

// ------------------------------------------------------------------ what show prints

#[test]
fn show_prints_the_gaps_and_the_stop_text_as_data() {
    let shown = |body: Value| {
        let v = json!({
            "id": "R-1", "revision": 1, "kind": "gaps", "status": "open", "run": "r1",
            "unit": "u1", "stage": "implement", "opened_at": "t", "reason": "gaps",
            "blocks": [], "body": body, "answer": null, "answer_generation": null,
            "verbs": ["answer", "clear"],
        });
        pulse_factory::controller::client::show_text(&v)
    };
    let text = shown(json!({
        "reason": "gaps",
        "gaps": [{"section": "§3", "question": "which\nbranch?\u{1b}[31m", "severity": "blocking"}],
        "gaps_error": null,
    }));
    assert!(
        text.contains("gap 1: [blocking] §3: which / branch??[31m\n"),
        "{text}"
    );
    assert!(
        !text.contains('\u{1b}'),
        "no escape character reaches the terminal"
    );
    let text = shown(json!({"reason": "stop", "stop_text": "line one\n\u{7}line two\n"}));
    assert!(
        text.contains("stop text:\n  | line one\n  | ?line two\n"),
        "{text}"
    );
    assert!(!text.contains('\u{7}'));
    let text = shown(json!({"reason": "gaps", "gaps_error": "the file does not exist"}));
    assert!(
        text.contains("gaps_error: the file does not exist\n"),
        "{text}"
    );
}

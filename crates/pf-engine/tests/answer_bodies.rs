//! AC-1: a `gaps` request holds the gap list its seat wrote, and a `stop` request holds the stop
//! file's text. The text is seat-written: it is read with a bound, cut, redacted and stored as
//! data. An answer whose hook failed leaves the request answered and parks the unit again.

mod common;

use std::path::Path;

use common::*;
use pf_engine::claim::{
    GAP_TEXT_MAX_CHARS, GAPS_FILE_MAX_BYTES, STOP_TEXT_MAX_BYTES, gaps_detail, sha256_hex,
    stop_detail,
};
use pf_engine::port::store::{RequestKind, RequestRow, RequestStatus, Store};
use pf_engine::{Command, CommandBody, EndStatus, EngineError, Status};
use serde_json::{Value, json};

fn item_env() -> Env {
    let env = env(item_workflow(&["implement", "verify"], true));
    env.engine
        .ingest(&mut FakeUnitSource {
            records: vec![unit("u1", "g1", 0)],
        })
        .unwrap();
    env
}

fn requests(env: &Env) -> Vec<RequestRow> {
    env.engine.requests().unwrap()
}

fn body(r: &RequestRow) -> Value {
    serde_json::from_str(&r.body).unwrap()
}

fn write(dir: &Path, name: &str, text: impl AsRef<[u8]>) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    path
}

/// Park `u1` the way the driver does for a seat's file: with the facts the file holds.
fn park_with(env: &Env, kind: RequestKind, reason: &str, detail: &Value) {
    env.engine.start_stage("u1").unwrap();
    env.engine
        .park_with("u1", kind, reason, Some(detail))
        .unwrap();
}

fn end_session(env: &Env) {
    env.engine
        .end("u1/implement#1", EndStatus::Failed, "ended")
        .unwrap();
}

fn gap(section: &str, question: &str, severity: &str) -> Value {
    json!({ "section": section, "question": question, "severity": severity })
}

// ------------------------------------------------------------------ the gap list

#[test]
fn a_gaps_file_in_the_implementer_shape_gives_a_body_with_each_gap() {
    let dir = tempfile::tempdir().unwrap();
    let gaps = vec![
        gap("§3 Pre-flight", "Which branch?", "blocking"),
        gap("AC-2", "Is the default 60 s?", "nice-to-have"),
    ];
    let file = write(
        dir.path(),
        "gaps.json",
        json!({
            "external_execution_gaps": {
                "work_item_id": "r0.s2.w2",
                "coordinator_verdict": "accepted",
                "implementer_return": { "mode": "gaps-surfaced", "gaps": gaps },
            }
        })
        .to_string(),
    );
    let detail = gaps_detail(&file);
    assert_eq!(detail, json!({ "gaps": gaps }));

    let env = item_env();
    park_with(&env, RequestKind::Gaps, "gaps surfaced for u1", &detail);
    let r = &requests(&env)[0];
    assert_eq!(r.kind, RequestKind::Gaps);
    assert_eq!(r.revision, 1, "the facts are in the first revision");
    assert_eq!(
        body(r),
        json!({ "reason": "gaps surfaced for u1", "gaps": gaps })
    );
}

#[test]
fn a_top_level_gaps_array_works_too() {
    let dir = tempfile::tempdir().unwrap();
    let gaps = vec![gap("AC-1", "What exit code?", "blocking")];
    let file = write(
        dir.path(),
        "gaps.json",
        json!({ "mode": "gaps-surfaced", "gaps": gaps }).to_string(),
    );
    assert_eq!(gaps_detail(&file), json!({ "gaps": gaps }));
    // An empty list is a list.
    let empty = write(dir.path(), "empty.json", r#"{"gaps": []}"#);
    assert_eq!(gaps_detail(&empty), json!({ "gaps": [] }));
}

#[test]
fn the_implementer_shape_wins_when_a_file_holds_both() {
    let dir = tempfile::tempdir().unwrap();
    let file = write(
        dir.path(),
        "gaps.json",
        json!({
            "gaps": [gap("top", "top question", "blocking")],
            "external_execution_gaps": {
                "implementer_return": { "gaps": [gap("deep", "deep question", "blocking")] }
            }
        })
        .to_string(),
    );
    assert_eq!(
        gaps_detail(&file)["gaps"][0]["section"],
        "deep",
        "the shape briefs/implement.md asks for comes first"
    );
}

fn error_of(detail: &Value) -> String {
    assert!(
        detail.get("gaps").is_none(),
        "no gaps with an error: {detail}"
    );
    detail["gaps_error"]
        .as_str()
        .unwrap_or_else(|| panic!("no gaps_error in {detail}"))
        .to_string()
}

#[test]
fn a_missing_oversized_or_malformed_file_gives_gaps_error_and_keeps_the_reason() {
    let dir = tempfile::tempdir().unwrap();
    let oversized = write(
        dir.path(),
        "big.json",
        vec![b' '; usize::try_from(GAPS_FILE_MAX_BYTES).unwrap() + 1],
    );
    let exactly_fits = write(
        dir.path(),
        "fits.json",
        format!(
            r#"{{"gaps": []}}{}"#,
            " ".repeat(usize::try_from(GAPS_FILE_MAX_BYTES).unwrap() - 12)
        ),
    );
    assert_eq!(
        std::fs::metadata(&exactly_fits).unwrap().len(),
        GAPS_FILE_MAX_BYTES
    );
    assert_eq!(gaps_detail(&exactly_fits), json!({ "gaps": [] }));

    let cases = [
        (dir.path().join("nope.json"), "does not exist"),
        (oversized, "over 1 MiB"),
        (
            write(dir.path(), "text.json", "gaps: none"),
            "not valid JSON",
        ),
        (write(dir.path(), "empty.json", ""), "not valid JSON"),
        (
            write(dir.path(), "obj.json", r#"{"mode":"x"}"#),
            "no `gaps` array",
        ),
        (
            write(dir.path(), "notarray.json", r#"{"gaps": "all of them"}"#),
            "not an array",
        ),
        (
            write(
                dir.path(),
                "noq.json",
                r#"{"gaps": [{"section":"a","severity":"b"}]}"#,
            ),
            "gap 1 has no string `question`",
        ),
        (
            write(
                dir.path(),
                "num.json",
                r#"{"gaps": [{"section":"a","question":5,"severity":"b"}]}"#,
            ),
            "no string `question`",
        ),
        (
            write(dir.path(), "scalar.json", r#"{"gaps": [7]}"#),
            "gap 1 has no string",
        ),
        (
            write(
                dir.path(),
                "deep.json",
                r#"{"external_execution_gaps": {"implementer_return": {"gaps": "x"}}}"#,
            ),
            "not an array",
        ),
    ];
    let env = item_env();
    let mut first = true;
    for (path, cause) in cases {
        let detail = gaps_detail(&path);
        let got = error_of(&detail);
        assert!(got.contains(cause), "{}: {got}", path.display());
        // The cause names no path, and no part of the file.
        assert!(!got.contains(dir.path().to_str().unwrap()), "{got}");
        if first {
            first = false;
            park_with(&env, RequestKind::Gaps, "the item has gaps", &detail);
            let r = &requests(&env)[0];
            assert_eq!(r.reason, "the item has gaps");
            assert_eq!(
                body(r),
                json!({ "reason": "the item has gaps", "gaps_error": got })
            );
        }
    }
}

#[test]
fn a_question_of_3000_characters_is_cut_to_2000() {
    let dir = tempfile::tempdir().unwrap();
    let long = "é".repeat(3_000);
    let file = write(
        dir.path(),
        "gaps.json",
        json!({ "gaps": [gap(&"s".repeat(2_500), &long, &"v".repeat(2_001))] }).to_string(),
    );
    let detail = gaps_detail(&file);
    let g = &detail["gaps"][0];
    assert_eq!(GAP_TEXT_MAX_CHARS, 2_000);
    assert_eq!(g["question"].as_str().unwrap().chars().count(), 2_000);
    assert_eq!(g["question"].as_str().unwrap(), "é".repeat(2_000));
    assert_eq!(g["section"].as_str().unwrap().chars().count(), 2_000);
    assert_eq!(g["severity"].as_str().unwrap().chars().count(), 2_000);
    // A question of exactly 2000 is not touched.
    let exact = write(
        dir.path(),
        "exact.json",
        json!({ "gaps": [gap("s", &"q".repeat(2_000), "blocking")] }).to_string(),
    );
    assert_eq!(
        gaps_detail(&exact)["gaps"][0]["question"]
            .as_str()
            .unwrap()
            .len(),
        2_000
    );
}

#[test]
fn a_secret_in_a_gap_is_redacted_before_it_is_stored() {
    let dir = tempfile::tempdir().unwrap();
    let token = "ghp_0123456789abcdefghijklmnopqrstuvwxyz";
    let file = write(
        dir.path(),
        "gaps.json",
        json!({ "gaps": [gap("s", &format!("use GH_TOKEN={token} to push?"), "blocking")] })
            .to_string(),
    );
    let stored = gaps_detail(&file).to_string();
    assert!(!stored.contains(token), "{stored}");
    assert!(stored.contains("to push?"), "{stored}");
}

// ------------------------------------------------------------------ the stop text

#[test]
fn a_stop_files_text_becomes_stop_text() {
    let dir = tempfile::tempdir().unwrap();
    let text = "Clear the stop: pick a branch.\nThe question:\n  which one?\n";
    let file = write(dir.path(), "stop", text);
    let detail = stop_detail(&file).expect("the file is readable");
    assert_eq!(detail, json!({ "stop_text": text }));

    let env = item_env();
    park_with(
        &env,
        RequestKind::Stop,
        "Clear the stop: pick a branch.",
        &detail,
    );
    let r = &requests(&env)[0];
    assert_eq!(r.kind, RequestKind::Stop);
    assert_eq!(
        body(r),
        json!({ "reason": "Clear the stop: pick a branch.", "stop_text": text })
    );
    // A file that cannot be read adds nothing: the request keeps its reason.
    assert!(stop_detail(&dir.path().join("no-stop")).is_none());
}

#[test]
fn a_stop_text_is_cut_to_64_kib_on_a_character_boundary() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(STOP_TEXT_MAX_BYTES, 64 * 1024);
    // 3-byte characters, so the 64 KiB cut falls inside one.
    let file = write(dir.path(), "stop", "€".repeat(30_000));
    let text = stop_detail(&file).unwrap()["stop_text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(text.len() <= 64 * 1024, "{} bytes", text.len());
    assert!(text.len() > 64 * 1024 - 4, "{} bytes", text.len());
    assert!(text.chars().all(|c| c == '€'), "no broken character");
    // A secret in the text is redacted.
    let token = "ghp_0123456789abcdefghijklmnopqrstuvwxyz";
    let secret = write(dir.path(), "stop2", format!("stop: GH_TOKEN={token}"));
    let stored = stop_detail(&secret).unwrap().to_string();
    assert!(!stored.contains(token), "{stored}");
}

#[test]
fn text_with_placeholders_and_command_substitution_is_stored_literally() {
    let dir = tempfile::tempdir().unwrap();
    let nasty = "use {param.repo} and $(rm -rf x) and `touch y` and {unit.id}";
    let stop = write(dir.path(), "stop", nasty);
    let gaps = write(
        dir.path(),
        "gaps.json",
        json!({ "gaps": [gap("{run.dir}", nasty, "$(id)")] }).to_string(),
    );
    assert_eq!(stop_detail(&stop).unwrap()["stop_text"], nasty);
    let detail = gaps_detail(&gaps);
    assert_eq!(detail["gaps"][0]["question"], nasty);
    assert_eq!(detail["gaps"][0]["section"], "{run.dir}");
    assert_eq!(detail["gaps"][0]["severity"], "$(id)");

    let env = item_env();
    park_with(&env, RequestKind::Gaps, "gaps", &detail);
    assert_eq!(body(&requests(&env)[0])["gaps"][0]["question"], nasty);
    assert!(!dir.path().join("y").exists(), "nothing ran the text");
}

// ------------------------------------------------------------------ the park command

#[test]
fn a_park_s_facts_cannot_set_the_keys_the_engine_owns() {
    let env = item_env();
    env.engine.start_stage("u1").unwrap();
    for key in ["reason", "stage", "park_generation"] {
        let why = match env.engine.park_with(
            "u1",
            RequestKind::Gaps,
            "gaps",
            Some(&json!({ key: "forged" })),
        ) {
            Err(EngineError::Rejected(why)) => why,
            other => panic!("expected a rejection, got {other:?}"),
        };
        assert!(why.contains(key), "{why}");
    }
    let why = match env.engine.park_with(
        "u1",
        RequestKind::Gaps,
        "gaps",
        Some(&json!(["not", "an", "object"])),
    ) {
        Err(EngineError::Rejected(why)) => why,
        other => panic!("expected a rejection, got {other:?}"),
    };
    assert!(why.contains("not a JSON object"), "{why}");
    assert_eq!(env.status_of("u1").0, Status::Running, "nothing was parked");
    assert!(requests(&env).is_empty());
}

#[test]
fn a_park_without_facts_keeps_its_stored_content() {
    // A command admitted before this change has no `detail` field. The same command still has
    // the same bytes, so its replay is a replay and not a conflict.
    let env = item_env();
    env.engine.start_stage("u1").unwrap();
    env.engine.park("u1", "stalled").unwrap();
    let row = env.store.admission("run-1", "park:u1:0").unwrap().unwrap();
    assert!(!row.content.contains("detail"), "{}", row.content);
    let replay = env
        .engine
        .submit(Command {
            id: "park:u1:0".to_string(),
            body: CommandBody::Park {
                unit_id: "u1".to_string(),
                reason: "stalled".to_string(),
                request_kind: RequestKind::Park,
                detail: None,
            },
        })
        .unwrap();
    assert!(replay.replayed);
}

// ------------------------------------------------------------------ an answer whose hook failed

fn claim(env: &Env) -> u64 {
    env.engine
        .claim("op@host", &sha256_hex(b"t1"))
        .unwrap()
        .generation
}

#[test]
fn a_failed_hook_leaves_the_request_answered_and_parks_the_unit_again() {
    let env = item_env();
    park_with(
        &env,
        RequestKind::Gaps,
        "gaps surfaced",
        &json!({ "gaps": [gap("s", "q", "blocking")] }),
    );
    end_session(&env);
    let generation = claim(&env);

    env.engine
        .answer_request_after_hook(
            "R-1",
            1,
            generation,
            "use the first branch",
            Some("on_answer command 1 (sh) exited with code 3: boom"),
        )
        .unwrap();

    // The request is answered, with the text.
    let all = requests(&env);
    assert_eq!(all.len(), 2, "{all:?}");
    assert_eq!(all[0].status, RequestStatus::Answered);
    assert_eq!(all[0].answer_generation, Some(generation));
    assert_eq!(
        serde_json::from_str::<Value>(all[0].answer.as_deref().unwrap()).unwrap(),
        json!({ "verb": "answer", "text": "use the first branch" })
    );
    // The unit parks again, with the hook's cause, and a new request is open.
    let (status, reason) = env.status_of("u1");
    assert_eq!(status, Status::Parked);
    let reason = reason.unwrap();
    assert_eq!(
        reason,
        "answer hook failed: on_answer command 1 (sh) exited with code 3: boom"
    );
    assert_eq!(all[1].id, "R-2");
    assert_eq!(all[1].status, RequestStatus::Open);
    assert_eq!(all[1].kind, RequestKind::Park);
    assert_eq!(all[1].stage, "implement");
    assert_eq!(all[1].reason, reason);
    assert_eq!(body(&all[1])["park_generation"], 1);
    // The unit has one open request, as the store requires.
    assert_eq!(
        requests(&env)
            .iter()
            .filter(|r| r.status == RequestStatus::Open)
            .count(),
        1
    );
    let ledger = env.ledger_text();
    assert!(
        ledger.contains("answer R-1@1 by claim 1: use the first branch (the answer hook failed"),
        "{ledger}"
    );

    // The second request is answered like any other: it resumes the unit.
    env.engine
        .answer_request("R-2", 1, generation, "fixed the hook")
        .unwrap();
    assert_eq!(env.status_of("u1").0, Status::Running);
}

#[test]
fn a_refused_answer_is_refused_before_a_hook_would_run() {
    let env = item_env();
    park_with(&env, RequestKind::Gaps, "gaps", &json!({ "gaps": [] }));
    let generation = claim(&env);
    let refused = |r: Result<(), EngineError>| match r {
        Err(EngineError::Rejected(why)) => why,
        other => panic!("expected a rejection, got {other:?}"),
    };
    assert!(refused(env.engine.check_answer("R-1", 9, generation, "x")).contains("revision 1"));
    assert!(refused(env.engine.check_answer("R-1", 1, generation + 1, "x")).contains("claim"));
    assert!(refused(env.engine.check_answer("R-1", 1, generation, "  ")).contains("needs text"));
    assert!(refused(env.engine.check_answer("R-9", 1, generation, "x")).contains("unknown"));
    // The request has a running session until it ends: the answer waits.
    assert!(
        refused(env.engine.check_answer("R-1", 1, generation, "x")).contains("running session")
    );
    end_session(&env);
    env.engine.check_answer("R-1", 1, generation, "x").unwrap();
    // The check records nothing.
    assert_eq!(requests(&env)[0].status, RequestStatus::Open);
    assert_eq!(env.status_of("u1").0, Status::Parked);
    assert!(!env.ledger_text().contains("answer R-1"));
}

#[test]
fn each_hook_run_writes_a_ledger_line_with_its_outcome() {
    let env = item_env();
    park_with(&env, RequestKind::Gaps, "gaps", &json!({ "gaps": [] }));
    env.engine
        .record_answer_hook("R-1", 1, Some(0), "")
        .unwrap();
    env.engine
        .record_answer_hook("R-1", 1, Some(3), "boom")
        .unwrap();
    env.engine
        .record_answer_hook("R-1", 1, None, "timed out after 5s and was killed")
        .unwrap();
    let lines: Vec<String> = env
        .ledger_text()
        .lines()
        .filter(|l| l.contains("answer hook"))
        .map(str::to_string)
        .collect();
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(
        lines[0].ends_with("| answer hook R-1: rc 0"),
        "{}",
        lines[0]
    );
    assert!(
        lines[1].ends_with("| answer hook R-1: rc 3: boom"),
        "{}",
        lines[1]
    );
    assert!(
        lines[2].ends_with("| answer hook R-1: rc none: timed out after 5s and was killed"),
        "{}",
        lines[2]
    );
    assert!(
        lines[0].contains(" u1 "),
        "the line names the unit: {}",
        lines[0]
    );
    // The hook of an unknown request records nothing.
    assert!(
        env.engine
            .record_answer_hook("R-8", 1, Some(0), "")
            .is_err()
    );
}

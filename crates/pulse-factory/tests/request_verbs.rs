//! AC-6: the read verbs. `requests` lists the open requests with every field, `--all` adds the
//! answered ones, `show` prints one request with its body and the verbs that answer it, and
//! `status` shows the request beside a parked unit and counts the open ones. None of them needs
//! the claim, and every JSON output holds `schema_version`. The CLI runs as a child process.

mod common;

use common::ctl::*;
use common::*;
use serde_json::Value;

fn json_of(live: &Live, args: &[&str]) -> Value {
    let mut full: Vec<&str> = args.to_vec();
    full.push("--json");
    let out = live.cli(&full);
    assert_eq!(code(&out), 0, "{args:?}: {}", stderr(&out));
    let v: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(v["schema_version"], 1, "{args:?}: {v}");
    v
}

/// Three units in a row, and u2 and u3 parked: u1 keeps running.
fn live_with_two_parks() -> Live {
    let live = Live::start();
    live.arm_with_units(&["u1", "u2", "u3"]);
    live.park_unit("u2", "the reviewer found a defect");
    live.park_unit("u3", "line one\nline two");
    live
}

fn ids(v: &Value) -> Vec<String> {
    v["requests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn requests_lists_the_open_requests_with_every_field_newest_last() {
    let live = live_with_two_parks();
    let v = json_of(&live, &["requests"]);
    assert_eq!(v["open"], 2);
    let list = v["requests"].as_array().unwrap();
    assert_eq!(list.len(), 2);
    let run = live.handle().info.run_id.clone();

    let first = &list[0];
    for key in [
        "id",
        "revision",
        "kind",
        "status",
        "run",
        "unit",
        "stage",
        "blocks",
        "opened_at",
        "answered_at",
        "reason",
    ] {
        assert!(
            first.get(key).is_some(),
            "requests --json lacks {key}: {first}"
        );
    }
    assert_eq!(first["revision"], 1);
    assert_eq!(first["kind"], "park");
    assert_eq!(first["status"], "open");
    assert_eq!(first["run"], run.as_str());
    assert_eq!(first["unit"], "u2");
    assert_eq!(first["stage"], "implement");
    assert_eq!(first["reason"], "the reviewer found a defect");
    assert!(!first["opened_at"].as_str().unwrap().is_empty());
    assert!(first["answered_at"].is_null());

    // Newest last, and the reason is one line.
    assert_eq!(list[1]["unit"], "u3");
    assert_eq!(list[1]["reason"], "line one / line two");
    let numbers: Vec<u64> = ids(&v)
        .iter()
        .map(|i| i.strip_prefix("R-").unwrap().parse().unwrap())
        .collect();
    assert!(numbers[0] < numbers[1], "{numbers:?}");

    // No claim was needed.
    assert!(live.handle().engine.view().unwrap().claims.is_empty());
}

#[test]
fn what_a_request_blocks_names_each_later_unit_the_merge_order_holds_back() {
    let live = live_with_two_parks();
    let v = json_of(&live, &["requests"]);
    let list = v["requests"].as_array().unwrap();
    // u2 holds back itself and u3, not u1 (earlier in the order).
    assert_eq!(list[0]["unit"], "u2");
    assert_eq!(list[0]["blocks"], serde_json::json!(["u2", "u3"]));
    assert_eq!(list[1]["unit"], "u3");
    assert_eq!(list[1]["blocks"], serde_json::json!(["u3"]));

    // The earliest unit parked holds back all three.
    live.park_unit("u1", "stop");
    let v = json_of(&live, &["requests"]);
    let u1 = v["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["unit"] == "u1")
        .unwrap()
        .clone();
    assert_eq!(u1["blocks"], serde_json::json!(["u1", "u2", "u3"]));

    // The text form names them too.
    let out = live.cli(&["requests"]);
    assert_eq!(code(&out), 0);
    let text = stdout(&out);
    assert!(text.contains("blocks u1 u2 u3"), "{text}");
    assert!(
        text.contains(&u1["id"].as_str().unwrap().to_string()),
        "{text}"
    );
    assert!(text.contains("rev 1 park"), "{text}");
}

#[test]
fn all_adds_the_answered_requests() {
    let live = live_with_two_parks();
    let before = json_of(&live, &["requests"]);
    let answered = ids(&before)[0].clone();

    let claim = live.cli(&["claim", "--holder", "ann@host"]);
    assert_eq!(code(&claim), 0, "{}", stderr(&claim));
    let out = live.cli(&["answer", &answered, "--rev", "1", "--text", "carry on"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));

    let open = json_of(&live, &["requests"]);
    assert_eq!(open["open"], 1);
    assert!(!ids(&open).contains(&answered));

    let all = json_of(&live, &["requests", "--all"]);
    assert_eq!(all["open"], 1, "the open count stays the open ones");
    assert!(ids(&all).contains(&answered), "{all}");
    let row = all["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == answered.as_str())
        .unwrap()
        .clone();
    assert_eq!(row["status"], "answered");
    assert!(!row["answered_at"].is_null());
    assert_eq!(row["blocks"], serde_json::json!([]));

    // The text form marks it.
    let text = stdout(&live.cli(&["requests", "--all"]));
    assert!(text.contains("[answered]"), "{text}");

    // --run narrows to one run, and an unknown run is refused.
    let run = live.handle().info.run_id.clone();
    assert_eq!(json_of(&live, &["requests", "--run", &run])["open"], 1);
    let out = live.cli(&["requests", "--run", "no-such-run"]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("no-such-run"), "{}", stderr(&out));
}

#[test]
fn requests_with_nothing_open_says_so() {
    let live = Live::start();
    live.arm();
    let out = live.cli(&["requests"]);
    assert_eq!(code(&out), 0);
    assert_eq!(stdout(&out), "no open requests\n");
    let v = json_of(&live, &["requests"]);
    assert_eq!(v["open"], 0);
    assert_eq!(v["requests"].as_array().unwrap().len(), 0);
}

#[test]
fn show_prints_the_body_and_the_verbs_that_answer_it() {
    let live = live_with_two_parks();
    let id = ids(&json_of(&live, &["requests"]))[0].clone();
    let v = json_of(&live, &["show", &id]);
    for key in [
        "id",
        "revision",
        "kind",
        "status",
        "run",
        "unit",
        "stage",
        "blocks",
        "opened_at",
        "answered_at",
        "reason",
        "body",
        "answer",
        "answer_generation",
        "verbs",
    ] {
        assert!(v.get(key).is_some(), "show --json lacks {key}: {v}");
    }
    assert_eq!(v["id"], id.as_str());
    assert_eq!(v["verbs"], serde_json::json!(["answer", "clear"]));
    assert!(v["body"].is_object(), "the body is JSON data: {v}");
    assert!(v["answer"].is_null());

    let out = live.cli(&["show", &id]);
    assert_eq!(code(&out), 0);
    let text = stdout(&out);
    assert!(
        text.contains(&format!("request: {id} (revision 1)")),
        "{text}"
    );
    assert!(text.contains("answered by: answer, clear"), "{text}");
    assert!(text.contains("pulse-factory answer"), "{text}");
    assert!(text.contains("pulse-factory clear"), "{text}");
    assert!(text.contains("the reviewer found a defect"), "{text}");

    // An unknown ID: exit 1, naming it.
    let out = live.cli(&["show", "R-999"]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("R-999"), "{}", stderr(&out));
    assert!(stdout(&out).is_empty());
}

#[test]
fn show_after_the_answer_holds_the_answer_and_no_verbs() {
    let live = live_with_two_parks();
    let id = ids(&json_of(&live, &["requests"]))[0].clone();
    assert_eq!(code(&live.cli(&["claim", "--holder", "ann@host"])), 0);
    let out = live.cli(&[
        "answer",
        &id,
        "--rev",
        "1",
        "--text",
        "use the second approach",
    ]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));

    let v = json_of(&live, &["show", &id]);
    assert_eq!(v["status"], "answered");
    assert_eq!(v["answer"]["verb"], "answer");
    assert_eq!(v["answer"]["text"], "use the second approach");
    assert_eq!(v["answer_generation"], 1);
    assert_eq!(v["verbs"], serde_json::json!([]));
    let text = stdout(&live.cli(&["show", &id]));
    assert!(
        text.contains("answer:") && text.contains("claim 1"),
        "{text}"
    );
    assert!(!text.contains("answered by:"), "{text}");
}

#[test]
fn status_shows_the_request_beside_a_parked_unit_and_the_open_count() {
    let live = live_with_two_parks();
    let listed = json_of(&live, &["requests"]);
    let u2 = listed["requests"][0]["id"].as_str().unwrap().to_string();
    let u3 = listed["requests"][1]["id"].as_str().unwrap().to_string();

    let v = json_of(&live, &["status"]);
    assert_eq!(v["open_requests"], 2);
    let unit = |id: &str| {
        v["units"]
            .as_array()
            .unwrap()
            .iter()
            .find(|u| u["id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(unit("u2")["status"], "parked");
    assert_eq!(unit("u2")["request"], u2.as_str());
    assert_eq!(unit("u3")["request"], u3.as_str());
    assert!(
        unit("u1")["request"].is_null(),
        "a running unit has no request"
    );

    let out = live.cli(&["status"]);
    assert_eq!(code(&out), 0);
    let text = stdout(&out);
    assert!(text.contains("open requests: 2"), "{text}");
    assert!(text.contains(&format!("[request {u2}]")), "{text}");
}

#[test]
fn every_json_output_of_the_verbs_holds_schema_version() {
    let live = live_with_two_parks();
    let id = ids(&json_of(&live, &["requests"]))[0].clone();
    let run = live.handle().info.run_id.clone();
    json_of(&live, &["requests"]);
    json_of(&live, &["requests", "--all"]);
    json_of(&live, &["show", &id]);
    json_of(&live, &["status"]);
    json_of(&live, &["claim", "--holder", "ann@host"]);
    json_of(&live, &["answer", &id, "--rev", "1", "--text", "go on"]);
    let other = ids(&json_of(&live, &["requests"]))[0].clone();
    assert_ne!(other, id);
    json_of(&live, &["clear", &run, "u3"]);
}

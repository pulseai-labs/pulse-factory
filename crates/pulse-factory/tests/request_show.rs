//! AC-5 of `r2.s11.w1`: `show` prints the decision block, and `requests --json` carries the cause
//! and the summary (`docs/contracts/cli.md`, `show` and `requests`). A request with no block prints
//! none of the new lines. A seat's text is tamed on its way to the terminal. A stored block of a
//! version this binary does not read gives no `decision` and no error. No line that `show` or
//! `requests` printed before this item changes.

mod common;
#[path = "common/needs_you_rig.rs"]
mod rig;

use std::sync::Arc;
use std::time::Duration;

use common::ctl::*;
use common::*;
use pf_api::types::{Decision, RequestFrame};
use pf_client::store::FileStore;
use pf_client::stream::{StreamEvent, StreamOptions, StreamTuning};
use pf_client::transport::Transport;
use pf_engine::port::store::{Cause, RequestKind};
use pf_engine::{Command, CommandBody};
use serde_json::{Value, json};

fn json_of(out: &std::process::Output) -> Value {
    serde_json::from_str(&stdout(out)).unwrap_or_else(|e| panic!("{e}: {}", stdout(out)))
}

/// Three parked units: `u1` with a block (a stall), `u2` with the old shape of a park (no cause),
/// and `u3` whose stop request holds a block with a control character in an option.
fn parked() -> Live {
    let live = Live::start();
    live.arm_with_units(&["u1", "u2", "u3"]);
    live.park_unit("u1", "stalled: the screen shows nothing");
    let engine = &live.handle().engine;
    engine
        .submit(Command {
            id: "park:u2:0".to_string(),
            body: CommandBody::Park {
                unit_id: "u2".to_string(),
                reason: "stalled: an old park".to_string(),
                request_kind: RequestKind::Stall,
                cause: None,
                detail: None,
            },
        })
        .unwrap();
    engine
        .park_with(
            "u3",
            RequestKind::Stop,
            Cause::StopFile,
            "Pick a branch.",
            Some(&json!({
                "stop_text": "Pick a branch.\nOptions:\n- a\n- b\n",
                "stop_options": [
                    { "text": "go\u{1b}[31m left", "recommended": false },
                    { "text": "go right", "recommended": true },
                ],
            })),
        )
        .unwrap();
    live.controller.tick_all();
    live
}

fn request_id(live: &Live, unit: &str) -> String {
    live.handle()
        .engine
        .view()
        .unwrap()
        .open_request_of(unit)
        .unwrap_or_else(|| panic!("no request for {unit}"))
        .id
        .clone()
}

fn show_text(live: &Live, id: &str) -> String {
    let out = live.cli(&["show", id]);
    assert!(out.status.success(), "{}", stderr(&out));
    stdout(&out)
}

#[test]
fn show_prints_the_block_after_every_line_it_printed_before() {
    let live = parked();
    let id = request_id(&live, "u1");
    let text = show_text(&live, &id);
    let lines: Vec<&str> = text.lines().collect();
    let at = |prefix: &str| {
        lines
            .iter()
            .position(|l| l.starts_with(prefix))
            .unwrap_or_else(|| panic!("no `{prefix}` line in:\n{text}"))
    };
    // The lines of before, in their order.
    let last_old = at("answered by:");
    assert!(at("request: ") < at("kind: "));
    assert!(at("kind: ") < at("body: "));
    assert!(lines.contains(&"kind: park"), "{text}");
    // The new lines, after them.
    assert!(at("cause: stalled") > last_old, "{text}");
    for prefix in [
        "summary: ",
        "asked by: controller",
        "attempt: 1",
        "takes text: no",
        "about: seat implementer (unit u1)",
        "option resume: Resume (resumes the unit; recommended by controller: ",
    ] {
        assert!(
            at(prefix) > last_old,
            "`{prefix}` comes before the old lines:\n{text}"
        );
    }
    assert!(
        !lines.iter().any(|l| l.starts_with("options error:")),
        "{text}"
    );
}

#[test]
fn an_option_text_with_an_escape_sequence_prints_with_the_character_replaced() {
    let live = parked();
    let text = show_text(&live, &request_id(&live, "u3"));
    assert!(!text.contains('\u{1b}'), "{text:?}");
    assert!(
        text.contains("option s1: go?[31m left (answers with the text"),
        "{text}"
    );
    assert!(
        text.contains(
            "option s2: go right (answers with the text \"go right\"; recommended by seat: "
        ),
        "{text}"
    );
    assert!(
        text.contains("asked by: seat implementer (unit u3)"),
        "{text}"
    );
    // `about` names the seat a halt concerns when the controller asked; here the seat did.
    assert!(!text.lines().any(|l| l.starts_with("about:")), "{text}");
}

#[test]
fn the_options_error_line_prints_when_a_stops_options_could_not_be_read() {
    let live = Live::start();
    live.arm_with_units(&["u1"]);
    live.handle()
        .engine
        .park_with(
            "u1",
            RequestKind::Stop,
            Cause::StopFile,
            "Stuck.",
            Some(&json!({
                "stop_text": "Stuck.\nOptions:\n- only one\n",
                "stop_options_error": "the options block holds 1 options, not 2 to 4",
            })),
        )
        .unwrap();
    live.controller.tick_all();
    let text = show_text(&live, &request_id(&live, "u1"));
    assert!(
        text.contains("options error: the options block holds 1 options, not 2 to 4"),
        "{text}"
    );
    assert!(!text.lines().any(|l| l.starts_with("option ")), "{text}");
}

#[test]
fn a_request_with_no_block_prints_none_of_the_new_lines() {
    let live = parked();
    let id = request_id(&live, "u2");
    let text = show_text(&live, &id);
    for prefix in [
        "cause:",
        "summary:",
        "asked by:",
        "attempt:",
        "takes text:",
        "about:",
        "options error:",
        "option ",
    ] {
        assert!(
            !text.lines().any(|l| l.starts_with(prefix)),
            "`{prefix}` printed for a request with no block:\n{text}"
        );
    }
    assert!(text.contains("kind: stall"), "{text}");
    let shown = json_of(&live.cli(&["show", &id, "--json"]));
    assert!(shown.get("decision").is_none(), "{shown}");
    assert!(
        shown.get("cause").is_none() && shown.get("summary").is_none(),
        "{shown}"
    );
}

#[test]
fn show_json_holds_a_decision_that_decodes_as_the_wire_type() {
    let live = parked();
    let shown = json_of(&live.cli(&["show", &request_id(&live, "u1"), "--json"]));
    let decision: Decision = serde_json::from_value(shown["decision"].clone())
        .unwrap_or_else(|e| panic!("{e}: {shown}"));
    assert_eq!(decision.v, 1);
    assert_eq!(decision.cause, "stalled");
    assert_eq!(decision.attempt, 1);
    assert_eq!(decision.options.len(), 1);
    assert_eq!(decision.options[0].id, "resume");
    assert_eq!(decision.recommended.as_ref().unwrap().by, "controller");
    assert_eq!(decision.refs.unit.as_deref(), Some("u1"));
    // The summary and the cause ride at the top too, beside the request's other fields.
    assert_eq!(shown["cause"], "stalled");
    assert_eq!(shown["summary"], json!(decision.summary));
    // The stop request decodes with its seat and its options.
    let stop = json_of(&live.cli(&["show", &request_id(&live, "u3"), "--json"]));
    let decision: Decision = serde_json::from_value(stop["decision"].clone()).unwrap();
    assert_eq!(decision.options.len(), 2);
    assert!(matches!(
        decision.asked_by,
        pf_api::types::AskedBy::Seat { ref role, .. } if role == "implementer"
    ));
}

#[test]
fn requests_json_holds_the_cause_and_the_summary_and_the_text_form_is_as_before() {
    let live = parked();
    let list = json_of(&live.cli(&["requests", "--json"]));
    let rows = list["requests"].as_array().unwrap();
    let row = |unit: &str| {
        rows.iter()
            .find(|r| r["unit"] == unit)
            .unwrap_or_else(|| panic!("no row for {unit}: {list}"))
    };
    assert_eq!(row("u1")["cause"], "stalled");
    assert!(!row("u1")["summary"].as_str().unwrap().is_empty());
    assert_eq!(row("u3")["cause"], "stop_file");
    assert!(row("u2").get("cause").is_none() && row("u2").get("summary").is_none());

    // The text form prints the line it printed before, and nothing of the block.
    let out = live.cli(&["requests"]);
    let text = stdout(&out);
    let line = text
        .lines()
        .find(|l| l.contains("unit u1"))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(
        line.contains("park") && line.contains("stalled: the screen shows nothing"),
        "{line}"
    );
    assert!(
        !text.contains("cause:") && !text.contains("summary:"),
        "{text}"
    );
}

#[test]
fn a_stored_block_above_version_1_gives_no_decision_and_no_error() {
    let live = Live::start();
    live.arm_with_units(&["u1"]);
    // A request as a later engine would have written it: a block of version 2 with a field this
    // binary does not know.
    live.handle()
        .engine
        .open_request(
            Some("u1"),
            RequestKind::Park,
            "implement",
            "a later engine parked it",
            &json!({
                "reason": "a later engine parked it",
                "decision": {
                    "v": 2,
                    "cause": "hook_failed",
                    "summary": "A later summary.",
                    "future_field": "something this binary does not know",
                },
            }),
        )
        .unwrap();
    live.controller.tick_all();
    let id = request_id(&live, "u1");

    let out = live.cli(&["show", &id, "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let shown = json_of(&out);
    assert!(shown.get("decision").is_none(), "{shown}");
    assert!(
        shown.get("cause").is_none() && shown.get("summary").is_none(),
        "{shown}"
    );
    // The body is still the stored one: the block is data this binary leaves as it is.
    assert_eq!(shown["body"]["decision"]["v"], 2);
    let text = show_text(&live, &id);
    assert!(!text.lines().any(|l| l.starts_with("cause:")), "{text}");
    let list = json_of(&live.cli(&["requests", "--json"]));
    assert!(list["requests"][0].get("cause").is_none(), "{list}");
}

#[test]
fn a_request_frame_holds_the_cause_and_the_summary() {
    let api = rig::api();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FileStore::new(dir.path().join("device.json")));
    let transport = Transport::new(store).with_stream_tuning(StreamTuning {
        schedule: vec![Duration::from_millis(50), Duration::from_millis(100)],
        reset_after: Duration::from_secs(60),
        silence: Duration::from_secs(30),
    });
    rt.block_on(transport.pair(&api.pair_code(), "MacBook"))
        .unwrap();
    let mut stream = {
        let _enter = rt.enter();
        transport.open_stream(StreamOptions::default())
    };
    let id = api.open_stop("Which base branch?");
    // The frame of the open request: the snapshot or the single frame, whichever holds it.
    let summary = rt.block_on(async {
        let end = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            let left = end.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(left, stream.next()).await {
                Ok(Some(StreamEvent::RequestFrame { frame })) => match frame {
                    RequestFrame::Request { request } if request.id == id => break request,
                    RequestFrame::Requests { requests, .. } => {
                        if let Some(r) = requests.into_iter().find(|r| r.id == id) {
                            break r;
                        }
                    }
                    _ => {}
                },
                Ok(Some(StreamEvent::Frame { .. })) => {}
                Ok(other) => panic!("{other:?} while waiting for the request frame"),
                Err(_) => panic!("timed out waiting for the request frame"),
            }
        }
    });
    assert_eq!(summary.cause.as_deref(), Some("stop_file"), "{summary:?}");
    let text = summary.summary.expect("the frame holds a summary");
    assert!(
        !text.is_empty() && !text.contains("Which base branch?"),
        "{text}"
    );
}

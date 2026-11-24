//! AC-4 of `r2.s3.w1`: every request says which side of the line it is on, `below` or `above`
//! (ADR-0006, amended; `docs/contracts/requests.md`, "The line"). The engine computes the class from
//! the request's cause and its track's merge, and the controller passes the run's forge repo and
//! default branch. `requests --json`, `show` and the request frames carry it. This item stores
//! none, so a closed request reads its class at the read. The class reads the track's merge from the
//! run's pinned copy of the workflow, so a change to the source package after `arm` moves no class.
//! `answer`, `clear` and `ack` treat a `below` request as any other.
//!
//! The controller runs in the test process on the fake adapter. The CLI runs as a child process.

mod common;
#[path = "common/needs_you_rig.rs"]
mod rig;

use std::sync::Arc;
use std::time::Duration;

use common::chat::*;
use common::ctl::*;
use common::*;
use pf_api::types::{RequestFrame, RequestList, RequestSummary};
use pf_client::store::FileStore;
use pf_client::stream::{StreamEvent, StreamOptions, StreamTuning};
use pf_client::transport::Transport;
use pf_engine::port::store::{Cause, RequestKind};
use pf_engine::{Command, CommandBody};
use pf_ipc::{Handler, Request};
use pulse_factory::controller::{Config, Controller};
use serde_json::{Value, json};

/// A package whose stage `implement` takes a text answer on any park (a text answer to a unit's
/// request needs a hook).
fn noted() -> Options {
    Options {
        implement_extra: "        on_answer:\n          - [\"true\"]\n".to_string(),
        ..Options::default()
    }
}

/// The item track of the fixture merges by `green_checks_in_declared_order` into `{param.spine_branch}`
/// (`spine/s1`) of the run's forge repo. The run's default branch is `main`. So a red check or a
/// conflict on it is `below`, and these two edits each make it `above`.
const RULE: (&str, &str) = ("rule: green_checks_in_declared_order", "rule: claim_ack");
const BASE: (&str, &str) = (
    "      base: \"{param.spine_branch}\"",
    "      base: \"main\"",
);

/// Rewrite the fixture's workflow file in the source package.
fn edit_package(sb: &Sandbox, edits: &[(&str, &str)]) {
    let path = sb.package.join("workflow.yaml");
    let mut text = std::fs::read_to_string(&path).unwrap();
    for (from, to) in edits {
        assert!(text.contains(from), "the fixture has no `{from}`");
        text = text.replace(from, to);
    }
    std::fs::write(&path, text).unwrap();
}

fn json_of(out: &std::process::Output) -> Value {
    assert_eq!(code(out), 0, "{}", stderr(out));
    serde_json::from_str(&stdout(out)).unwrap_or_else(|e| panic!("{e}: {}", stdout(out)))
}

/// Four parked units: `u1` a stall, `u2` a stop, `u3` a red check and `u4` a conflict. `u5` holds the
/// old shape of a park, with no block.
fn parked(live: &Live) {
    live.arm_with_units(&["u1", "u2", "u3", "u4", "u5"]);
    live.park_unit("u1", "stalled: the screen shows nothing");
    let engine = &live.handle().engine;
    engine
        .park_with(
            "u2",
            RequestKind::Stop,
            Cause::StopFile,
            "Pick a branch.",
            Some(&json!({
                "stop_text": "Pick a branch.\nOptions:\n- a\n- b\n",
                "stop_options": [
                    { "text": "go left", "recommended": false },
                    { "text": "go right", "recommended": true },
                ],
            })),
        )
        .unwrap();
    engine
        .park_with(
            "u3",
            RequestKind::MergeFailed,
            Cause::MergeChecksRed,
            "the checks are red",
            None,
        )
        .unwrap();
    engine
        .park_with(
            "u4",
            RequestKind::MergeFailed,
            Cause::MergeConflict,
            "the PR conflicts with its base",
            None,
        )
        .unwrap();
    engine
        .submit(Command {
            id: "park:u5:0".to_string(),
            body: CommandBody::Park {
                unit_id: "u5".to_string(),
                reason: "stalled: an old park".to_string(),
                request_kind: RequestKind::Stall,
                cause: None,
                detail: None,
            },
        })
        .unwrap();
    live.controller.tick_all();
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

/// The class of each unit's request in `requests --all --json`, by unit.
fn lines_by_unit(list: &Value) -> Vec<(String, Option<String>)> {
    list["requests"]
        .as_array()
        .unwrap_or_else(|| panic!("no requests in {list}"))
        .iter()
        .map(|r| {
            (
                r["unit"].as_str().unwrap_or_default().to_string(),
                r["line"].as_str().map(str::to_string),
            )
        })
        .collect()
}

fn line_of(list: &Value, unit: &str) -> Option<String> {
    lines_by_unit(list)
        .into_iter()
        .find(|(u, _)| u == unit)
        .unwrap_or_else(|| panic!("no row for {unit}: {list}"))
        .1
}

/// The operator's claim: `answer` and `clear` act under it.
fn claim(live: &Live) {
    let out = live.cli(&["claim", "--holder", "ann@host"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
}

fn requests_all(live: &Live) -> Value {
    json_of(&live.cli(&["requests", "--all", "--json"]))
}

fn show_text(live: &Live, id: &str) -> String {
    let out = live.cli(&["show", id]);
    assert!(out.status.success(), "{}", stderr(&out));
    stdout(&out)
}

#[test]
fn a_stall_a_red_check_and_a_conflict_read_below_and_a_stop_and_an_old_park_read_above() {
    let live = Live::start_package(BIN.into(), &noted(), |_| {});
    parked(&live);

    let list = requests_all(&live);
    assert_eq!(line_of(&list, "u1").as_deref(), Some("below"), "{list}");
    assert_eq!(line_of(&list, "u2").as_deref(), Some("above"), "{list}");
    assert_eq!(line_of(&list, "u3").as_deref(), Some("below"), "{list}");
    assert_eq!(line_of(&list, "u4").as_deref(), Some("below"), "{list}");
    // A request with no block has no cause, and no cause is `above`.
    assert_eq!(line_of(&list, "u5").as_deref(), Some("above"), "{list}");

    // `show --json` carries the class through the flattened summary.
    for (unit, class) in [
        ("u1", "below"),
        ("u2", "above"),
        ("u3", "below"),
        ("u5", "above"),
    ] {
        let shown = json_of(&live.cli(&["show", &request_id(&live, unit), "--json"]));
        assert_eq!(shown["line"], class, "{unit}: {shown}");
    }

    // The class is never stored: no request body holds a `line` key, and no decision block does.
    let view = live.handle().engine.view().unwrap();
    for r in &view.requests {
        let body: Value = serde_json::from_str(&r.body).unwrap();
        assert!(body.get("line").is_none(), "{body}");
        assert!(
            body.get("decision").is_none_or(|d| d.get("line").is_none()),
            "{body}"
        );
    }
}

#[test]
fn a_red_check_reads_above_on_a_claim_ack_track_and_on_a_track_that_merges_into_the_default_branch()
{
    for (name, edits) in [("claim_ack", [RULE]), ("default branch", [BASE])] {
        let live = Live::start_package(BIN.into(), &Options::default(), |_| {});
        edit_package(&live.sb, &edits);
        live.arm_with_units(&["u1", "u2", "u3"]);
        let engine = &live.handle().engine;
        engine
            .park_with(
                "u1",
                RequestKind::MergeFailed,
                Cause::MergeChecksRed,
                "the checks are red",
                None,
            )
            .unwrap();
        engine
            .park_with(
                "u2",
                RequestKind::MergeFailed,
                Cause::MergeConflict,
                "the PR conflicts with its base",
                None,
            )
            .unwrap();
        live.park_unit("u3", "stalled: the screen shows nothing");
        let list = requests_all(&live);
        assert_eq!(
            line_of(&list, "u1").as_deref(),
            Some("above"),
            "{name}: {list}"
        );
        assert_eq!(
            line_of(&list, "u2").as_deref(),
            Some("above"),
            "{name}: {list}"
        );
        // A stall is below the line whatever the track merges into.
        assert_eq!(
            line_of(&list, "u3").as_deref(),
            Some("below"),
            "{name}: {list}"
        );
    }
}

#[test]
fn a_closed_request_still_carries_its_class() {
    let live = Live::start_package(BIN.into(), &noted(), |_| {});
    parked(&live);
    let stall = request_id(&live, "u1");
    let run = live.handle().info.run_id.clone();
    claim(&live);
    // `clear` closes the stall's request.
    let out = live.cli(&["clear", &run, "u1", "--json"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));

    let list = json_of(&live.cli(&["requests", "--all", "--json"]));
    let row = list["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == stall.as_str())
        .unwrap_or_else(|| panic!("the closed request is not listed: {list}"));
    assert_ne!(row["status"], "open", "{row}");
    assert_eq!(row["line"], "below", "{row}");
    let shown = json_of(&live.cli(&["show", &stall, "--json"]));
    assert_eq!(shown["line"], "below", "{shown}");
    // The open list no longer holds it, and the others keep their class.
    let open = json_of(&live.cli(&["requests", "--json"]));
    assert!(
        !lines_by_unit(&open).iter().any(|(u, _)| u == "u1"),
        "{open}"
    );
    assert_eq!(line_of(&open, "u3").as_deref(), Some("below"), "{open}");
}

#[test]
fn every_class_stays_when_the_source_package_changes_after_arm_and_after_a_restart() {
    let live = Live::start_package(BIN.into(), &noted(), |_| {});
    parked(&live);
    let before = lines_by_unit(&requests_all(&live));
    assert_eq!(before.len(), 5, "{before:?}");
    assert!(before.iter().all(|(_, l)| l.is_some()), "{before:?}");

    // The pinned copy is the run's own. The source package now gives the other class for a red
    // check and a conflict: `claim_ack` and a base that is the default branch.
    assert!(live.run_dir().join("package").is_dir());
    edit_package(&live.sb, &[RULE, BASE]);

    assert_eq!(lines_by_unit(&requests_all(&live)), before);
    for unit in ["u1", "u3"] {
        let shown = json_of(&live.cli(&["show", &request_id(&live, unit), "--json"]));
        let want = before.iter().find(|(u, _)| u == unit).unwrap().1.clone();
        assert_eq!(shown["line"].as_str().map(str::to_string), want, "{unit}");
    }

    // A controller that starts again on the same state directory loads the run from its pinned
    // copy, so it reads the same classes.
    let second = Controller::open(
        Config::new(live.sb.state.clone(), 3_600_000, BIN.into()),
        live.ports.clone(),
    )
    .unwrap();
    let resp = second.handle(Request::new("requests", json!({ "all": true })));
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(lines_by_unit(&resp.result), before);
    let id = request_id(&live, "u3");
    let resp = second.handle(Request::new("show", json!({ "id": id })));
    assert!(resp.ok, "{:?}", resp.error);
    assert_eq!(resp.result["line"], "below", "{}", resp.result);
}

#[test]
fn show_prints_the_line_after_the_summary_and_every_line_it_printed_before() {
    let live = Live::start_package(BIN.into(), &noted(), |_| {});
    parked(&live);

    let text = show_text(&live, &request_id(&live, "u1"));
    let lines: Vec<&str> = text.lines().collect();
    let at = |prefix: &str| {
        lines
            .iter()
            .position(|l| l.starts_with(prefix))
            .unwrap_or_else(|| panic!("no `{prefix}` line in:\n{text}"))
    };
    assert_eq!(
        lines.get(at("summary: ") + 1),
        Some(&"line: below"),
        "`line:` is not directly after `summary:`:\n{text}"
    );
    assert_eq!(lines.iter().filter(|l| l.starts_with("line:")).count(), 1);
    // The lines of before, in their order, and the lines of the block.
    let last_old = at("answered by:");
    assert!(at("request: ") < at("kind: "));
    assert!(at("kind: ") < at("body: "));
    assert!(lines.contains(&"kind: park"), "{text}");
    for prefix in [
        "cause: stalled",
        "summary: ",
        "asked by: controller",
        "attempt: 1",
        "takes text: ",
        "about: seat implementer (unit u1)",
        "option resume: Resume (by controller; resumes the unit; recommended by controller: ",
    ] {
        assert!(at(prefix) > last_old, "`{prefix}` is out of place:\n{text}");
    }
    // A stop reads `above`.
    let stop = show_text(&live, &request_id(&live, "u2"));
    let stop_lines: Vec<&str> = stop.lines().collect();
    let summary = stop_lines
        .iter()
        .position(|l| l.starts_with("summary: "))
        .unwrap_or_else(|| panic!("{stop}"));
    assert_eq!(stop_lines.get(summary + 1), Some(&"line: above"), "{stop}");

    // A request with no block prints `line:` after the last line it printed before, and none of
    // the block's lines.
    let old = show_text(&live, &request_id(&live, "u5"));
    let old_lines: Vec<&str> = old.lines().collect();
    assert_eq!(old_lines.last(), Some(&"line: above"), "{old}");
    assert_eq!(
        old_lines.iter().filter(|l| l.starts_with("line:")).count(),
        1
    );
    assert!(
        old_lines.iter().any(|l| l.starts_with("answered by:")),
        "{old}"
    );
    assert!(
        !old_lines.iter().any(|l| l.starts_with("summary:")),
        "{old}"
    );
}

#[test]
fn requests_json_holds_grants_after_grant_clear_and_no_grants_key_before() {
    let live = Live::start_package(BIN.into(), &noted(), |_| {});
    live.arm_with_units(&["u1"]);
    live.park_unit("u1", "stalled: the screen shows nothing");
    let project = add_project(&live, &repo(&live, "alpha"));

    let before = json_of(&live.cli(&["requests", "--json"]));
    assert!(before.get("grants").is_none(), "{before}");
    assert!(before["requests"][0].get("line").is_some(), "{before}");

    let out = live.cli(&["grant", "clear", "--project", &project]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let after = json_of(&live.cli(&["requests", "--json"]));
    let list: RequestList = serde_json::from_value(after.clone()).unwrap();
    assert_eq!(list.grants.len(), 1, "{after}");
    assert_eq!(list.grants[0].project_id, project);
    assert_eq!(list.grants[0].generation, 1);
    assert_eq!(list.grants[0].agent_id, "orchestrator");
    assert!(list.grants[0].live);

    // The grant ends, and the key goes with it.
    let out = live.cli(&["grant", "revoke", "--project", &project]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let ended = json_of(&live.cli(&["requests", "--json"]));
    assert!(ended.get("grants").is_none(), "{ended}");
}

#[test]
fn answer_clear_and_ack_treat_a_below_request_as_any_other() {
    let live = Live::start_package(BIN.into(), &noted(), |_| {});
    parked(&live);
    let run = live.handle().info.run_id.clone();
    claim(&live);
    let stall = request_id(&live, "u1");
    let red = request_id(&live, "u3");
    let stop = request_id(&live, "u2");

    // The verbs that answer a request do not depend on its class.
    let list = requests_all(&live);
    let row = |id: &str| {
        list["requests"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(row(&stall)["verbs"], json!(["answer", "clear"]));
    assert_eq!(row(&red)["verbs"], json!(["answer", "clear"]));
    assert_eq!(row(&stop)["verbs"], json!(["answer", "clear"]));

    // `ack` takes a `merge_ack` request alone: a `below` request is refused in the words that an
    // `above` one gets. The refusal names the request's kind, so the text after the kind is the
    // same for a `below` park (`u1`), a `below` red check (`u3`) and an `above` stop (`u2`).
    let refusal = |id: &str| {
        let out = live.cli(&["ack", id, "--rev", "1"]);
        assert_ne!(code(&out), 0, "ack took {id}");
        let text = stderr(&out);
        let (head, tail) = text
            .split_once(" request: ")
            .unwrap_or_else(|| panic!("an unexpected refusal: {text}"));
        assert!(
            head.starts_with(&format!("pulse-factory: ack refused: request {id} is a ")),
            "{text}"
        );
        tail.to_string()
    };
    let words = refusal(&stop);
    assert_eq!(refusal(&stall), words);
    assert_eq!(refusal(&red), words);

    // `answer` with a text and `clear` close a `below` request.
    let out = live.cli(&["answer", &stall, "--rev", "1", "--text", "go on", "--json"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let out = live.cli(&["clear", &run, "u3", "--json"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let open = json_of(&live.cli(&["requests", "--json"]));
    let units: Vec<String> = lines_by_unit(&open).into_iter().map(|(u, _)| u).collect();
    assert!(!units.contains(&"u1".to_string()), "{units:?}");
    assert!(!units.contains(&"u3".to_string()), "{units:?}");
    assert!(units.contains(&"u2".to_string()), "{units:?}");
}

// ---- the frames

/// A device that is paired once and opens a stream for each request it waits for.
struct Device {
    rt: tokio::runtime::Runtime,
    transport: Transport<FileStore>,
    _dir: tempfile::TempDir,
}

fn device(api: &rig::Api) -> Device {
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
    Device {
        rt,
        transport,
        _dir: dir,
    }
}

impl Device {
    /// The summary that the request frame of `id` holds: the snapshot or the single frame,
    /// whichever holds it.
    fn frame_of(&self, id: &str) -> RequestSummary {
        let mut stream = {
            let _enter = self.rt.enter();
            self.transport.open_stream(StreamOptions::default())
        };
        self.rt.block_on(async {
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
                    Ok(Some(StreamEvent::Frame { .. } | StreamEvent::NoticeFrame { .. })) => {}
                    Ok(other) => panic!("{other:?} while waiting for the request frame"),
                    Err(_) => panic!("timed out waiting for the request frame of {id}"),
                }
            }
        })
    }
}

#[test]
fn a_request_frame_holds_the_class_below_for_a_stall_and_above_for_a_stop() {
    let api = rig::api();
    let device = device(&api);

    // A stop is `above`.
    let stop = api.open_stop("Which base branch?");
    let frame = device.frame_of(&stop);
    assert_eq!(frame.line.as_deref(), Some("above"), "{frame:?}");

    // A stall on the second unit is `below`.
    let s = &api.s;
    s.item("u2", 2, 2);
    s.tick();
    s.live.park_unit("u2", "stalled: the screen shows nothing");
    let id = request_id(&s.live, "u2");
    let frame = device.frame_of(&id);
    assert_eq!(frame.cause.as_deref(), Some("stalled"), "{frame:?}");
    assert_eq!(frame.line.as_deref(), Some("below"), "{frame:?}");

    // The first request keeps its class in the snapshot of a new stream.
    let again = device.frame_of(&stop);
    assert_eq!(again.line.as_deref(), Some("above"), "{again:?}");
}

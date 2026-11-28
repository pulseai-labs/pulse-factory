//! AC-7 of `r2.s3.w2`: a settled request keeps what its settle recorded (`docs/contracts/requests.md`,
//! "Settle"; ADR-0006, amended; audit C12). The stored answer holds the class, the cause and, for a
//! red check, the rule, the rendered base, the repo and the default branch, as the class rendered and
//! compared them. A reader reads `line` of a settled request from that record, not from a new
//! computation, and `settled` names the act, the reason, the session, the project and the generation.
//!
//! How the computed class is made to differ: the test stops using the controller, writes the track's
//! rendered base into `default_branch` of the run's `run.json`, and opens a second controller on the
//! same state directory. A new red check of the same track then reads `above`, computed, and the
//! settled request still reads `below`, recorded.

mod common;
#[path = "common/settle_rig.rs"]
mod settle_rig;

use common::chat::*;
use common::*;
use pf_engine::port::store::{Cause, RequestKind, Store};
use pf_ipc::{Handler, Request};
use pulse_factory::controller::client::show_text;
use pulse_factory::controller::{Config, Controller};
use serde_json::{Value, json};
use settle_rig::*;

/// The summary of `id` in `requests --all`, from a controller.
fn summary(controller: &Controller, id: &str) -> Value {
    let resp = controller.handle(Request::new("requests", json!({ "all": true })));
    assert!(resp.ok, "{:?}", resp.error);
    resp.result["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id)
        .cloned()
        .unwrap_or_else(|| panic!("no request {id}"))
}

fn shown(controller: &Controller, id: &str) -> Value {
    let resp = controller.handle(Request::new("show", json!({ "id": id })));
    assert!(resp.ok, "{:?}", resp.error);
    resp.result
}

#[test]
fn a_settled_request_reads_its_recorded_line_when_the_computed_class_differs() {
    // The item track merges by `green_checks_in_declared_order` into `spine/s1`, and the run's default
    // branch is `main`, so a red check is `below`.
    let rig = SRig::new(&[], Some(20));
    rig.live.queue_item("u1", 1);
    rig.live.queue_item("u2", 2);
    rig.tick();
    rig.tick();
    let engine = rig.live.handle().engine.clone();
    engine
        .park_with(
            "u1",
            RequestKind::MergeFailed,
            Cause::MergeChecksRed,
            "the checks are red",
            None,
        )
        .unwrap();
    rig.tick();
    let (id, rev) = rig.request("u1");
    assert_eq!(summary(&rig.live.controller, &id)["line"], "below");

    // The Orchestrator settles it with `redispatch --to <the first stage>`.
    let resp = rig.settle(
        &id,
        rev,
        "redispatch",
        Some("implement"),
        "run the checks from the start",
    );
    assert_eq!(resp["ok"], true, "{resp}");
    let stored = rig
        .live
        .controller
        .store()
        .request(&id)
        .unwrap()
        .unwrap()
        .answer
        .unwrap();
    let stored: Value = serde_json::from_str(&stored).unwrap();
    // The record holds the class, the cause and the inputs the class compared.
    let line = &stored["line"];
    assert_eq!(line["class"], "below", "{stored}");
    assert_eq!(line["cause"], "merge_checks_red", "{stored}");
    assert_eq!(line["rule"], "green_checks_in_declared_order", "{stored}");
    assert_eq!(line["base"], "spine/s1", "{stored}");
    assert_eq!(line["default_branch"], "main", "{stored}");
    assert!(line["repo"].is_string(), "{stored}");

    // Stop using the controller. The track's rendered base becomes the run's default branch.
    let run_json = rig.live.run_dir().join("run.json");
    let mut run: Value =
        serde_json::from_str(&std::fs::read_to_string(&run_json).unwrap()).unwrap();
    run["default_branch"] = json!("spine/s1");
    std::fs::write(&run_json, serde_json::to_string_pretty(&run).unwrap()).unwrap();
    let rec = rig.rec.clone();
    let mut cfg = Config::new(rig.live.sb.state.clone(), 3_600_000, BIN.into());
    cfg.chat_runtimes.insert(RUNTIME.to_string(), rec);
    cfg.server_name = "test-server".to_string();
    let reopened = Controller::open(cfg, rig.live.ports.clone()).unwrap();

    // A new red check of the same track now reads `above`, computed.
    reopened
        .handles()
        .last()
        .unwrap()
        .engine
        .park_with(
            "u2",
            RequestKind::MergeFailed,
            Cause::MergeChecksRed,
            "the checks are red",
            None,
        )
        .unwrap();
    reopened.tick_all();
    let fresh = reopened
        .handles()
        .last()
        .unwrap()
        .engine
        .view()
        .unwrap()
        .open_request_of("u2")
        .unwrap()
        .id
        .clone();
    assert_eq!(summary(&reopened, &fresh)["line"], "above", "computed");

    // The settled request still reads `below`, from its record, in `requests`, `show` and its frame.
    let in_requests = summary(&reopened, &id);
    assert_eq!(in_requests["line"], "below");
    assert_eq!(in_requests["settled"]["act"], "redispatch");
    assert_eq!(in_requests["settled"]["to_stage"], "implement");
    assert_eq!(
        in_requests["settled"]["reason"],
        "run the checks from the start"
    );
    assert_eq!(in_requests["settled"]["session"], rig.session.as_str());
    assert_eq!(in_requests["settled"]["project"], rig.project.as_str());
    assert_eq!(in_requests["settled"]["generation"], 1);
    let detail = shown(&reopened, &id);
    assert_eq!(detail["line"], "below");
    assert_eq!(detail["settled"]["act"], "redispatch");
    // The request frame and the `requests` rows are built by one function (`request_summaries`), so
    // the frame carries the same two fields.

    // `show` prints the `settled:` line after the `line:` line, and every line it printed before.
    let text = show_text(&detail);
    let lines: Vec<&str> = text.lines().collect();
    let at_line = lines
        .iter()
        .position(|l| *l == "line: below")
        .expect("a line: line");
    assert_eq!(
        lines[at_line + 1],
        format!(
            "settled: by the Orchestrator (session {}, grant {}#1): act=redispatch --to implement reason=run the checks from the start",
            rig.session, rig.project
        ),
        "{text}"
    );
    for before in [
        "request: ",
        "kind: ",
        "status: answered",
        "run: ",
        "stage: ",
        "cause: merge_checks_red",
    ] {
        assert!(
            lines.iter().any(|l| l.starts_with(before)),
            "{before}: {text}"
        );
    }
}

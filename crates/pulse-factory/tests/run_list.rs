//! AC-4 of `r1.s4.w3`: `run_list` on the real controller, over the socket and over the API (a paired
//! device on pinned TLS), on the spine-close rig: a `FakeAdapter` and a `BareForge`. The call reads
//! and writes nothing but its one `api.ledger` line. `waiting` is compared with the note that
//! `status --json` shows, never with a string this test spells.

mod common;
#[path = "common/needs_you_rig.rs"]
mod rig;

use std::sync::Arc;

use common::chat::controller_ledger;
use common::*;
use pf_client::store::{DeviceStore, FileStore};
use pf_client::transport::Transport;
use pf_engine::port::store::Store;
use serde_json::{Value, json};

const KEYS: [&str; 8] = [
    "end_reason",
    "halted_reason",
    "id",
    "open_requests",
    "spine",
    "state",
    "waiting",
    "workflow",
];

/// The API's answer to `run_list`, through `pf-client` over pinned TLS, as JSON.
fn over_the_api(api: &rig::Api, transport: &Transport<FileStore>) -> Value {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _ = api;
    serde_json::to_value(
        rt.block_on(transport.run_list(pf_api::types::RunListArgs {}))
            .unwrap(),
    )
    .unwrap()
}

fn paired(api: &rig::Api) -> (Transport<FileStore>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FileStore::new(dir.path().join("device.json")));
    let transport = Transport::new(store.clone());
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(transport.pair(&api.pair_code(), "MacBook"))
        .unwrap();
    assert!(store.load().unwrap().is_some());
    (transport, dir)
}

fn over_the_socket(api: &rig::Api) -> Value {
    let r = pf_ipc::call(&api.s.live.sb.state, "run_list", json!({})).unwrap();
    assert!(r.ok, "{:?}", r.error);
    r.result
}

/// What the store holds of the run, as text: the run row, its nodes, requests and admissions.
fn store_text(api: &rig::Api) -> String {
    let store = api.s.live.controller.store();
    let id = api.s.live.handle().info.run_id.clone();
    format!(
        "{:?}\n{:?}\n{:?}\n{:?}",
        store.run(&id),
        store.nodes(&id),
        store.requests(&id),
        store.admissions(&id)
    )
}

#[test]
fn run_list_shows_the_run_waiting_then_ended_and_reads_nothing_but_its_ledger_line() {
    let api = rig::api();
    let s = &api.s;
    let (transport, _dir) = paired(&api);
    let id = s.live.handle().info.run_id.clone();

    // Before the merge: the run is active and notes no waiting text.
    let before = over_the_api(&api, &transport);
    assert_eq!(before["schema_version"], 1);
    let runs = before["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 1, "{before}");
    let run = &runs[0];
    assert_eq!(run["id"], id.as_str());
    assert_eq!(run["state"], "active");
    assert!(run["waiting"].is_null(), "{run}");
    assert!(run["halted_reason"].is_null() && run["end_reason"].is_null());
    assert_eq!(run["open_requests"], 0);
    assert_eq!(run["spine"], s.live.handle().info.spine.as_str());
    assert_eq!(run["workflow"], s.live.handle().info.workflow.as_str());

    // The socket and the API answer the same JSON, and each `RunSummary` has exactly its keys.
    assert_eq!(over_the_socket(&api), before);
    for r in runs {
        let mut keys: Vec<&str> = r.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, KEYS, "{r}");
    }

    // One `api.ledger` line for the call, and nothing else changes: no controller ledger line, no
    // admission, no change to the runs, nodes or requests of the store.
    let lines = api.api_ledger();
    let hits: Vec<_> = lines
        .iter()
        .filter(|l| l.contains("verb=run_list") && l.contains("device=D-1"))
        .collect();
    assert_eq!(hits.len(), 1, "{lines:?}");
    assert!(
        hits[0].contains("outcome=ok") && hits[0].contains("route=verb"),
        "{}",
        hits[0]
    );
    let controller_before = controller_ledger(&s.live);
    let store_before = store_text(&api);
    let engine_ledger_before = s.ledger();
    for _ in 0..3 {
        over_the_api(&api, &transport);
        over_the_socket(&api);
    }
    assert_eq!(controller_ledger(&s.live), controller_before);
    assert_eq!(store_text(&api), store_before);
    assert_eq!(s.ledger(), engine_ledger_before);

    // The spine PR opens a `merge_ack`: the run counts one open request.
    let (request, _head) = api.to_merge_ack();
    let during = over_the_api(&api, &transport);
    assert_eq!(during["runs"][0]["open_requests"], 1, "{during}");
    assert!(during["runs"][0]["waiting"].is_null());

    // The CLI's `ack` merges, and a tick runs: `waiting` is the note `status --json` shows.
    assert_eq!(code(&s.live.cli(&["claim", "--holder", "ann@host"])), 0);
    let ack = s.live.cli(&["ack", &request, "--rev", "1"]);
    assert_eq!(code(&ack), 0, "{}", stderr(&ack));
    api.wait_for_the_close();
    let status = s.live.sb.status_json("");
    let waiting = over_the_api(&api, &transport)["runs"][0]["waiting"].clone();
    let text = waiting.as_str().expect("the run waits for its close");
    assert!(
        status["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n == text),
        "waiting {text:?} is not in the notes {}",
        status["notes"]
    );
    assert_eq!(over_the_socket(&api)["runs"][0]["waiting"], waiting);
    assert_eq!(over_the_api(&api, &transport)["runs"][0]["state"], "active");
    assert_eq!(
        over_the_api(&api, &transport)["runs"][0]["open_requests"],
        0
    );

    // The end file and a tick: the run is ended, waits for nothing and gives the end reason.
    api.close();
    let status = s.live.sb.status_json("");
    assert_eq!(status["state"], "ended");
    let ended = over_the_api(&api, &transport);
    let run = &ended["runs"][0];
    assert_eq!(run["state"], "ended", "{ended}");
    assert!(run["waiting"].is_null(), "{run}");
    assert_eq!(run["end_reason"], status["end_reason"]);
    assert!(!run["end_reason"].is_null());
    // A tick after the end keeps it so.
    s.tick();
    assert!(over_the_api(&api, &transport)["runs"][0]["waiting"].is_null());
    assert_eq!(over_the_socket(&api), over_the_api(&api, &transport));

    // The call wrote no `controller.ledger` line of its own: every line names an act that is not it.
    for line in controller_ledger(&s.live) {
        assert!(!line.contains("run_list"), "{line}");
    }
}

#[test]
fn a_revoked_devices_run_list_is_401() {
    let api = rig::api();
    let (transport, _dir) = paired(&api);
    over_the_api(&api, &transport);
    let out = api.s.live.cli(&["device", "revoke", "D-1"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let e = rt
        .block_on(transport.run_list(pf_api::types::RunListArgs {}))
        .unwrap_err();
    assert_eq!(e.kind, pf_client::error::ErrorKind::Unauthorized);
    let lines = api.api_ledger();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("verb=run_list") && l.contains("status=401")),
        "{lines:?}"
    );
}

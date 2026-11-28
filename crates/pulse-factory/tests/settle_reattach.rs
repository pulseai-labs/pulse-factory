//! AC-10 (the settle half) of `r2.s3.w3`: `resume` on a `host_unreachable` request attaches to the
//! recorded host again before anything launches (`docs/contracts/requests.md`, "Settle"; ADR-0005,
//! ADR-0007, amended). The seat adapter's `rebind` and `is_alive` are scripted over the fake
//! adapter. A host that answers is rebound and its stage runs on with no launch; a host that is gone
//! is replaced by a relaunch under the bound; a host that does not answer parks the unit again.
//! The controller marks the unit on the host service for the call and clears the mark after.
//!
//! The controller runs in the test process on the fake adapter, with a recording chat.

mod common;
#[path = "common/settle_rig.rs"]
mod settle_rig;

use std::sync::Arc;

use common::ctl::RebindScript;
use pf_engine::Cause;
use pf_engine::claim::sha256_hex;
use pf_engine::port::store::{Effect, NewHost, NewLaunchRecord, RequestKind};
use serde_json::json;
use settle_rig::*;

/// A rig whose unit `u1` is parked `host_unreachable` after its `implement` seat started.
fn unreachable() -> SRig {
    let rig = SRig::new(&[], Some(10));
    rig.live.queue_item("u1", 1);
    rig.tick();
    rig.tick();
    rig.live
        .handle()
        .engine
        .park_with(
            "u1",
            RequestKind::SeatLost,
            Cause::HostUnreachable,
            "host unreachable: no answer",
            None,
        )
        .unwrap();
    rig.tick();
    assert_eq!(rig.status_of("u1"), "parked");
    rig
}

/// Write the registry rows of the seat host that `u1`'s `implement` seat ran in, as the host service
/// does when it starts one: the seat adapter rebuilds the seat's key entry from the launch record
/// when a rebind finds the host alive.
fn seed_seat_host(rig: &SRig) {
    let plan = rig
        .live
        .ports
        .plans
        .lock()
        .unwrap()
        .iter()
        .rfind(|p| p.unit_id == "u1")
        .cloned()
        .unwrap();
    let process = rig
        .live
        .handle()
        .engine
        .view()
        .unwrap()
        .sessions
        .iter()
        .rfind(|s| s.unit_id == "u1" && s.process_ref.is_some())
        .and_then(|s| s.process_ref.clone())
        .unwrap();
    let unit = process
        .split_once(':')
        .map_or(process.as_str(), |(_, inner)| inner)
        .to_string();
    let settings: serde_json::Map<String, serde_json::Value> = plan
        .settings
        .iter()
        .filter(|(k, _)| !k.starts_with("env."))
        .map(|(k, v)| (k.clone(), json!(v)))
        .collect();
    let body = json!({
        "argv": ["seat"],
        "cwd": "/work/alpha",
        "env": {"PF_SESSION_KEY": format!("sha256:{}", sha256_hex(b"a seat key"))},
        "secret_names": [],
        "transport": {"kind": "pipes"},
        "runtime": {"id": plan.runtime, "version": "1", "binary_path": "/bin/true",
                    "binary_sha256": "ab".repeat(32)},
        "settings": settings,
        "plugins": [],
        "skills": [],
        "rules_sha256": "cd".repeat(32),
        "run_binding": {"run_id": plan.run_id, "unit_id": plan.unit_id, "stage": plan.stage,
                        "role": plan.role, "model": plan.model, "attempt": plan.attempt},
        "resume": "fresh",
    })
    .to_string();
    let (_, hash) = NewLaunchRecord::seal(&body).unwrap();
    let session = format!("{}/u1", rig.run);
    commit(
        &rig.live,
        "launch_record_write",
        &format!("lr-{unit}"),
        json!({ "session": session }),
        vec![Effect::LaunchRecordWrite(NewLaunchRecord {
            session_kind: "seat".to_string(),
            session_id: session.clone(),
            incarnation: 1,
            revision: 1,
            launcher: "detached".to_string(),
            host_unit: unit.clone(),
            host_socket: format!("/tmp/{unit}/host.sock"),
            record: body,
            record_sha256: hash.clone(),
            created_at: "t1".to_string(),
        })],
    );
    commit(
        &rig.live,
        "host_started",
        &format!("hs-{unit}"),
        json!({ "unit": unit }),
        vec![Effect::HostStarted(NewHost {
            unit: unit.clone(),
            session_kind: "seat".to_string(),
            session_id: session,
            incarnation: 1,
            launched_revision: 1,
            launched_record_sha256: hash,
            launcher: "detached".to_string(),
            socket: format!("/tmp/{unit}/host.sock"),
            invocation_id: "inv-1".to_string(),
            host_binary: "/bin/true".to_string(),
            host_sha256: "aa".repeat(32),
            protocol: 1,
            started_at: "t2".to_string(),
        })],
    );
}

fn script(rig: &SRig, s: RebindScript) {
    *rig.live.ports.rebinds.script.lock().unwrap() = Some(s);
}

fn implement_nodes(rig: &SRig) -> Vec<String> {
    rig.stage_nodes("u1")
        .into_iter()
        .filter(|(s, _)| s == "implement")
        .map(|(_, status)| status)
        .collect()
}

#[test]
fn a_host_that_answers_is_rebound_and_the_stage_runs_on_with_no_launch() {
    let rig = unreachable();
    seed_seat_host(&rig);
    script(&rig, RebindScript::Survived);
    // What the host service holds while the adapter's `rebind` runs.
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    {
        let seen = seen.clone();
        let controller = rig.live.controller.clone();
        *rig.live.ports.rebinds.on_call.lock().unwrap() = Some(Arc::new(move || {
            seen.lock()
                .unwrap()
                .push(controller.host_service().reattach_marks());
        }));
    }
    let launches = rig.admissions("launch");
    let (id, rev) = rig.request("u1");
    done(&rig.settle(&id, rev, "resume", None, WHY));

    assert_eq!(rig.live.ports.rebinds.calls.lock().unwrap().len(), 1);
    // The unit was marked for the call, and the mark is gone after it.
    let marks = seen.lock().unwrap().clone();
    assert_eq!(marks.len(), 1, "{marks:?}");
    assert_eq!(marks[0].len(), 1, "{marks:?}");
    assert_eq!(marks[0][0].0, format!("{}/u1", rig.run));
    assert_eq!(marks[0][0].1, format!("{id}@{rev}"));
    assert!(
        rig.live
            .controller
            .host_service()
            .reattach_marks()
            .is_empty()
    );

    let results = rig.results();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].1["outcome"], "attached", "{:?}", results[0]);
    assert_eq!(rig.admissions("launch"), launches, "no launch is admitted");
    assert_eq!(rig.status_of("u1"), "running");
    // A new attempt of the parked stage runs on the same process reference.
    assert_eq!(
        implement_nodes(&rig),
        ["failed", "running"],
        "{:?}",
        rig.stage_nodes("u1")
    );
    rig.tick();
    assert_eq!(
        rig.admissions("launch"),
        launches,
        "no tick launches it either"
    );
    let line = rig.run_ledger("settle-result");
    assert!(
        line.len() == 1 && line[0].contains("resume attached host="),
        "{line:#?}"
    );
}

#[test]
fn a_host_that_is_gone_is_replaced_by_a_launch_under_the_bound() {
    let rig = unreachable();
    script(&rig, RebindScript::Lost("the unit is gone".to_string()));
    let launches = rig.admissions("launch");
    let (id, rev) = rig.request("u1");
    done(&rig.settle(&id, rev, "resume", None, WHY));
    assert_eq!(rig.results()[0].1["outcome"], "gone");
    assert_eq!(
        rig.admissions("launch"),
        launches,
        "no launch before the tick"
    );
    assert_eq!(rig.status_of("u1"), "running");
    rig.tick();
    assert_eq!(
        rig.admissions("launch"),
        launches + 1,
        "the stage launches again"
    );
    assert_eq!(implement_nodes(&rig).last().unwrap(), "running");
    let line = rig.run_ledger("settle-result");
    assert!(line[0].contains("resume gone host="), "{line:#?}");
}

#[test]
fn a_host_that_does_not_answer_parks_the_unit_again_with_one_line() {
    let rig = unreachable();
    script(
        &rig,
        RebindScript::Unreachable("no hello in 3000 ms".to_string()),
    );
    let launches = rig.admissions("launch");
    let (id, rev) = rig.request("u1");
    done(&rig.settle(&id, rev, "resume", None, WHY));
    assert_eq!(rig.results()[0].1["outcome"], "failed");
    assert_eq!(rig.status_of("u1"), "parked");
    let (id2, _) = rig.request("u1");
    assert_ne!(id2, id, "the park is a new request");
    let all = cli_json_requests(&rig);
    assert!(all.contains("host_unreachable"), "{all}");
    let line = rig.run_ledger("settle-result");
    assert_eq!(line.len(), 1, "{line:#?}");
    assert!(
        line[0].contains("resume failed") && line[0].contains("parked"),
        "{line:#?}"
    );
    assert_eq!(rig.admissions("launch"), launches);
    // A failed adapter call is a failure too.
    let rig = unreachable();
    script(&rig, RebindScript::Error("socket closed".to_string()));
    let (id, rev) = rig.request("u1");
    done(&rig.settle(&id, rev, "resume", None, WHY));
    assert_eq!(rig.results()[0].1["outcome"], "failed");
    assert_eq!(rig.status_of("u1"), "parked");
}

fn cli_json_requests(rig: &SRig) -> String {
    common::chat::cli_json(&rig.live, &["requests", "--all"]).to_string()
}

#[test]
fn an_adapter_with_no_host_follows_is_alive() {
    // `Lost` with the cause `no session host`: `is_alive` decides, as tier 1 does.
    let rig = unreachable();
    script(
        &rig,
        RebindScript::Lost(pf_engine::port::agent::NO_SESSION_HOST.to_string()),
    );
    *rig.live.ports.rebinds.alive.lock().unwrap() = Some(true);
    let (id, rev) = rig.request("u1");
    done(&rig.settle(&id, rev, "resume", None, WHY));
    assert_eq!(rig.results()[0].1["outcome"], "attached");

    let rig = unreachable();
    script(
        &rig,
        RebindScript::Lost(pf_engine::port::agent::NO_SESSION_HOST.to_string()),
    );
    *rig.live.ports.rebinds.alive.lock().unwrap() = Some(false);
    let (id, rev) = rig.request("u1");
    done(&rig.settle(&id, rev, "resume", None, WHY));
    assert_eq!(rig.results()[0].1["outcome"], "gone");
}

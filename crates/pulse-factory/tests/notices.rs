//! AC-5 of `r2.s11.w3`: the controller derives one notice from each request event, writes its text
//! from the request's decision block, redacted and cut to the channel's word cap, and gives it one
//! delivery per channel (ADR-0006's amendment of 2026-11-21, `docs/contracts/requests.md`,
//! "Notices"). The controller runs in the test process on the fake adapter. Nothing here starts a
//! seat, spends a token or reaches beyond `127.0.0.1`.

mod common;
#[path = "common/needs_you_rig.rs"]
mod needs;
#[path = "common/notice_rig.rs"]
mod rig;

use common::chat::*;
use common::ctl::Live;
use pf_engine::port::store::{Cause, RequestKind, RequestStatus, Store};
use rig::*;
use serde_json::{Value, json};

const APP: &str = "app";
const ORCH: &str = "orchestrator";

/// A controller with one project, a run of three units linked to it, and no unit parked.
fn project_live() -> (Live, String) {
    let live = start(Rec::new(strict_fake()));
    let project = add_project(&live, &repo(&live, "alpha"));
    live.arm_with_units(&["u1", "u2", "u3"]);
    let run = live.handle().info.run_id.clone();
    link(&live, &run, &project);
    (live, project)
}

/// A controller with a run that has no project.
fn bare_live() -> Live {
    let live = start(Rec::new(strict_fake()));
    live.arm_with_units(&["u1", "u2", "u3"]);
    live
}

fn summary_of(live: &Live, request: &str) -> String {
    requests_json(live)
        .into_iter()
        .find(|r| r["id"] == request)
        .unwrap_or_else(|| panic!("{request} is not open"))["summary"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn a_stop_a_park_and_a_merge_ack_each_get_one_opened_notice() {
    let (live, project) = project_live();
    let run = live.handle().info.run_id.clone();
    live.park_unit("u1", "the reviewer found a defect");
    live.handle()
        .engine
        .park_with(
            "u2",
            RequestKind::Stop,
            Cause::StopFile,
            "Pick a branch.",
            Some(&json!({ "stop_text": "Pick a branch.", "stop_options": [] })),
        )
        .unwrap();
    // The ticks add nothing more than one notice for each request event.
    ticks(&live, 4);
    let all = notices(&live);
    assert_eq!(all.len(), 2, "{all:?}");
    for (n, (unit, row)) in [("u1", &all[0]), ("u2", &all[1])].into_iter().enumerate() {
        let (id, revision) = request_of(&live, unit);
        assert_eq!(row.position, n as u64 + 1);
        assert_eq!(row.request_id, id);
        assert_eq!((row.revision, row.event.as_str()), (revision, "opened"));
        assert_eq!(row.run_id, run);
        assert_eq!(row.project_id.as_deref(), Some(project.as_str()));
        // The text is the block's summary: a sentence of the controller's own.
        assert_eq!(text_of(&live, row.position, APP), summary_of(&live, &id));
        assert_eq!(text_of(&live, row.position, ORCH), summary_of(&live, &id));
        assert_eq!(state_of(&live, row.position, APP), "waiting");
        assert_eq!(state_of(&live, row.position, ORCH), "waiting");
    }
    // One admission and one ledger line each, with the command ID and no text.
    let lines = ledger(&live, "notice");
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines[0].starts_with(&format!(
            "notice 1 request={}@1 event=opened run={run} project={project} app=waiting \
             orchestrator=waiting command=notice:{}:1:opened",
            all[0].request_id, all[0].request_id
        )),
        "{}",
        lines[0]
    );
    assert!(
        !lines[0].contains("stalled"),
        "no text on a line: {}",
        lines[0]
    );
}

#[test]
fn a_merge_ack_gets_an_opened_notice_and_a_revised_one_at_its_new_revision() {
    let api = needs::api();
    let (id, _head) = api.to_merge_ack();
    let live = &api.s.live;
    live.controller.tick_all();
    let first = notices(live);
    assert_eq!(first.len(), 1, "{first:?}");
    assert_eq!(
        (first[0].request_id.as_str(), first[0].revision),
        (id.as_str(), 1)
    );
    assert_eq!(first[0].event, "opened");
    assert_eq!(first[0].project_id, None);
    // The facts change: the request is revised, and the notice follows at the new revision.
    let row = live.controller.store().request(&id).unwrap().unwrap();
    let mut body: Value = serde_json::from_str(&row.body).unwrap();
    body["note"] = json!("the head moved");
    live.handle().engine.revise_request(&id, 1, &body).unwrap();
    ticks(live, 3);
    let all = notices(live);
    assert_eq!(all.len(), 2, "{all:?}");
    assert_eq!((all[1].revision, all[1].event.as_str()), (2, "revised"));
    assert!(
        text_of(live, 2, APP).starts_with("Changed: "),
        "{}",
        text_of(live, 2, APP)
    );
}

#[test]
fn an_answer_writes_closed_and_supersedes_what_still_waited() {
    let (live, _) = project_live();
    live.park_unit("u1", "the reviewer found a defect");
    let (id, revision) = request_of(&live, "u1");
    // A revision that the device never acknowledged: its delivery waits when the request closes.
    let row = live.controller.store().request(&id).unwrap().unwrap();
    let mut body: Value = serde_json::from_str(&row.body).unwrap();
    body["note"] = json!("more facts");
    live.handle()
        .engine
        .revise_request(&id, revision, &body)
        .unwrap();
    ticks(&live, 2);
    assert_eq!(notices(&live).len(), 2);
    let engine = &live.handle().engine;
    let granted = engine.claim("tester", &"a".repeat(64)).unwrap();
    engine
        .answer_request(&id, revision + 1, granted.generation, "go on")
        .unwrap();
    ticks(&live, 2);
    let all = notices(&live);
    assert_eq!(all.len(), 3, "{all:?}");
    assert_eq!(all[2].event, "closed");
    assert_eq!(
        text_of(&live, 3, APP),
        format!("Request {id} closed: answered.")
    );
    // A closed notice never waits.
    assert_eq!(state_of(&live, 3, APP), "superseded:request_closed");
    assert_eq!(state_of(&live, 3, ORCH), "superseded:request_closed");
    // What waited is superseded, one command each.
    for position in [1, 2] {
        for channel in [APP, ORCH] {
            assert_eq!(
                state_of(&live, position, channel),
                "superseded:request_closed",
                "{position} {channel}"
            );
        }
    }
    let lines = ledger(&live, "notice_superseded");
    assert_eq!(lines.len(), 4, "{lines:?}");
    for position in [1, 2] {
        for channel in [APP, ORCH] {
            let want = format!("notice_superseded {position} channel={channel} why=request_closed");
            assert!(lines.contains(&want), "{want} is not in {lines:?}");
        }
    }
    // Ticks after the close add nothing.
    ticks(&live, 3);
    assert_eq!(notices(&live).len(), 3);
    assert_eq!(ledger(&live, "notice_superseded").len(), 4);
}

#[test]
fn a_withdrawn_request_closes_with_the_word_withdrawn() {
    let (live, _) = project_live();
    live.park_unit("u1", "the reviewer found a defect");
    let (id, _) = request_of(&live, "u1");
    live.handle()
        .engine
        .withdraw_request(&id, "nothing will answer")
        .unwrap();
    ticks(&live, 2);
    let all = notices(&live);
    assert_eq!(all.len(), 2, "{all:?}");
    assert_eq!(
        text_of(&live, 2, ORCH),
        format!("Request {id} closed: withdrawn.")
    );
}

#[test]
fn a_request_closed_before_the_first_tick_gets_nothing_and_an_open_one_gets_opened() {
    let (live, _) = project_live();
    // Park both units through the engine, with no tick between: the controller has seen neither.
    let engine = &live.handle().engine;
    engine.park("u1", Cause::Stalled, "first").unwrap();
    engine.park("u2", Cause::Stalled, "second").unwrap();
    let (first, _) = request_of(&live, "u1");
    engine.withdraw_request(&first, "gone").unwrap();
    ticks(&live, 3);
    let all = notices(&live);
    assert_eq!(all.len(), 1, "{all:?}");
    let (second, _) = request_of(&live, "u2");
    assert_eq!(all[0].request_id, second);
    assert_eq!(all[0].event, "opened");
    let closed = live.controller.store().request(&first).unwrap().unwrap();
    assert_eq!(closed.status, RequestStatus::Withdrawn);
}

#[test]
fn no_marker_word_and_no_password_reaches_a_notice_or_a_ledger_line() {
    let (live, _) = project_live();
    live.handle()
        .engine
        .park_with(
            "u1",
            RequestKind::Stop,
            Cause::StopFile,
            "MARKERWORD in the reason",
            Some(&json!({
                "stop_text": "MARKERWORD asks which branch",
                "stop_options": [{ "text": "MARKERWORD go left", "recommended": true }],
            })),
        )
        .unwrap();
    // A cap park whose facts hold a URL with a password.
    live.handle()
        .engine
        .park_with(
            "u2",
            RequestKind::Park,
            Cause::OutcomeCap,
            "cap reached",
            Some(&json!({ "cap_to": "https://deploy:hunter2secret@example.invalid/x", "cap_max": 3 })),
        )
        .unwrap();
    ticks(&live, 3);
    let all = notices(&live);
    assert_eq!(all.len(), 2, "{all:?}");
    let mut seen = String::new();
    for row in &all {
        for channel in [APP, ORCH] {
            seen.push_str(&text_of(&live, row.position, channel));
            seen.push('\n');
        }
    }
    seen.push_str(&controller_ledger(&live).join("\n"));
    assert!(!seen.contains("MARKERWORD"), "{seen}");
    assert!(!seen.contains("hunter2secret"), "{seen}");
}

#[test]
fn the_text_is_cut_at_a_whole_word_under_the_channels_cap() {
    let (live, project) = project_live();
    set(&live, "notify.app.max_words", "5", &[]);
    set(
        &live,
        "notify.orchestrator.max_words",
        "10",
        &["--project", &project],
    );
    live.park_unit("u1", "the reviewer found a defect");
    let (id, _) = request_of(&live, "u1");
    let full = summary_of(&live, &id);
    assert!(full.split_whitespace().count() > 10, "{full}");
    let app = text_of(&live, 1, APP);
    let cut: Vec<&str> = full.split_whitespace().take(5).collect();
    assert_eq!(app, format!("{}…", cut.join(" ")));
    let orch = text_of(&live, 1, ORCH);
    let cut: Vec<&str> = full.split_whitespace().take(10).collect();
    assert_eq!(orch, format!("{}…", cut.join(" ")));
}

#[test]
fn a_channel_that_is_off_writes_superseded_with_channel_off() {
    let (live, _) = project_live();
    set(&live, "notify.app.on", "0", &[]);
    live.park_unit("u1", "the reviewer found a defect");
    assert_eq!(state_of(&live, 1, APP), "superseded:channel_off");
    assert_eq!(state_of(&live, 1, ORCH), "waiting");
    let line = &ledger(&live, "notice")[0];
    assert!(
        line.contains("app=superseded:channel_off orchestrator=waiting"),
        "{line}"
    );
}

#[test]
fn a_run_with_no_project_has_no_orchestrator_delivery_to_wait() {
    let live = bare_live();
    live.park_unit("u1", "the reviewer found a defect");
    assert_eq!(state_of(&live, 1, APP), "waiting");
    assert_eq!(state_of(&live, 1, ORCH), "superseded:no_project");
    let line = &ledger(&live, "notice")[0];
    assert!(line.contains("project=- "), "{line}");
}

#[test]
fn a_halted_run_still_gets_its_notices_and_launches_nothing() {
    let (live, _) = project_live();
    let out = live.cli(&["halt", "--reason", "hold"]);
    assert_eq!(common::code(&out), 0, "{}", common::stderr(&out));
    let launches = live.ports.plans.lock().unwrap().len();
    live.handle()
        .engine
        .park("u1", Cause::Stalled, "the build is red")
        .unwrap();
    ticks(&live, 3);
    assert_eq!(notices(&live).len(), 1);
    assert_eq!(live.ports.plans.lock().unwrap().len(), launches);
}

#[test]
fn a_stopped_controller_writes_no_notice_and_says_so_in_warnings() {
    // A controller whose commands are stopped writes no notice. The request is in the store.
    let live = start_with(Rec::new(strict_fake()), |cfg| {
        std::fs::create_dir(cfg.state_dir.join("controller.ledger")).unwrap();
    });
    live.arm_with_units(&["u1"]);
    live.park_unit("u1", "the reviewer found a defect");
    ticks(&live, 3);
    assert!(notices(&live).is_empty());
    let v = live.sb.status_json("");
    let warnings = v["warnings"].to_string();
    assert!(
        warnings.contains("notice") && warnings.contains("stopped"),
        "stopped controller commands show in warnings: {warnings}"
    );
}

#[test]
fn status_lists_each_waiting_notice_with_its_age() {
    let (live, _) = project_live();
    live.park_unit("u1", "the reviewer found a defect");
    let v = live.sb.status_json("");
    let waiting = v["notices"]["waiting"].as_array().unwrap();
    assert_eq!(waiting.len(), 1, "{v}");
    let (id, _) = request_of(&live, "u1");
    assert_eq!(waiting[0]["position"], 1);
    assert_eq!(waiting[0]["request"], id.as_str());
    assert_eq!(waiting[0]["revision"], 1);
    assert_eq!(waiting[0]["event"], "opened");
    assert_eq!(waiting[0]["channel"], ORCH);
    assert_eq!(
        waiting[0]["target"].as_str().unwrap(),
        "agent:P-1:orchestrator"
    );
    assert_eq!(waiting[0]["sends"], 0);
    assert!(waiting[0]["waiting_seconds"].is_u64(), "{v}");
    assert_eq!(v["notices"]["blocked"], json!([]));
}

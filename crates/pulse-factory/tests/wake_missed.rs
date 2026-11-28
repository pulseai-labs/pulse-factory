//! AC-3 of `r2.s3.w4`: a missed wake is sent when its conditions next hold (ruling D1; audit C9).
//! A wake that cannot run when its request opens waits in the wake set: a request that is open,
//! below the line and not yet woken. The check that finds it runs at three points: the end of a
//! notice step (every tick and after each request verb), the end of a turn of the project's
//! Orchestrator, and when `grant clear` has committed. Every waiting notice rides one send.
//!
//! The controller runs in the test process on a `FakeChat` behind a port that logs each send.
//! Nothing here starts `claude`, spends a token or reaches beyond `127.0.0.1`.

mod common;
#[path = "common/settle_rig.rs"]
mod settle_rig;
#[path = "common/wake_rig.rs"]
mod wake_rig;

use std::time::Duration;

use common::chat::*;
use common::{code, stderr};
use pf_engine::port::chat::TurnCause;
use pf_engine::port::store::Store;
use serde_json::{Value, json};
use wake_rig::*;

/// The IDs on the request lines of a block, in order.
fn listed(block: &str) -> Vec<String> {
    request_lines(block)
        .into_iter()
        .map(|l| pairs(l)[0].1.clone())
        .collect()
}

/// Close a request as the operator does, with a claim and an answer.
fn close(w: &W, request: &str, rev: u32) {
    let engine = &w.live.handle().engine;
    let granted = engine.claim("tester", &"a".repeat(64)).unwrap();
    engine
        .answer_request(request, rev, granted.generation, "go on")
        .unwrap();
}

/// Revise an open request: its facts change, its revision moves.
fn revise(w: &W, request: &str, rev: u32) {
    let row = w.live.controller.store().request(request).unwrap().unwrap();
    let mut body: Value = serde_json::from_str(&row.body).unwrap();
    body["note"] = json!(format!("more facts at {rev}"));
    w.live
        .handle()
        .engine
        .revise_request(request, rev, &body)
        .unwrap();
    w.tick(2);
}

#[test]
fn a_request_that_opens_during_a_woken_turn_is_woken_when_that_turn_ends_in_a_second_wake() {
    let w = world(&["u1", "u2"], Start::Running);
    w.port.swallow_wakes();
    w.grant();
    w.stall("u1");
    let (first, _) = w.request("u1");
    w.wait("the first wake's send", || w.port.wake_sends().len() == 1);
    w.begin(TurnCause::Notice);
    // The second request opens during the woken turn: no wake goes into an open turn.
    w.stall("u2");
    let (second, _) = w.request("u2");
    w.tick(6);
    assert_eq!(w.port.wake_sends().len(), 1);
    // The turn ends: the first request is marked, and the end's check sends the second wake.
    w.end(TurnCause::Notice);
    w.wait("the second wake's send", || w.port.wake_sends().len() == 2);
    assert!(w.is_woken(&first));
    let sends = w.port.wake_sends();
    assert_eq!(listed(&sends[0]), vec![first.clone()]);
    assert_eq!(listed(&sends[1]), vec![second.clone()]);
    assert_eq!(w.wake_items().len(), 2);
    w.begin(TurnCause::Notice);
    w.end(TurnCause::Notice);
    w.wait_woken(&second);
}

#[test]
fn a_request_that_opened_with_no_grant_is_woken_when_grant_clear_commits_before_the_next_tick() {
    let w = world(&["u1"], Start::NoGrant);
    let (id, _) = w.request("u1");
    w.tick(6);
    assert!(w.port.wake_sends().is_empty());
    // The grant commits; the wake is sent by the verb itself, with no tick in between.
    w.grant();
    let sends = w.port.wake_sends();
    assert_eq!(sends.len(), 1, "the grant did not wake the request");
    assert_eq!(listed(&sends[0]), vec![id.clone()]);
    w.wait_woken(&id);
}

#[test]
fn a_request_held_by_the_hour_is_woken_at_the_tick_after_the_hour_frees_a_slot() {
    let w = world(&["u1", "u2"], Start::Running);
    w.set("chat.self_turn_cap", "0");
    w.set("autopilot.wakes_per_hour", "1");
    w.grant();
    w.stall("u1");
    let (first, _) = w.request("u1");
    w.wait_woken(&first);
    w.stall("u2");
    let (second, _) = w.request("u2");
    w.tick(6);
    assert!(!w.is_woken(&second));
    assert_eq!(w.port.wake_sends().len(), 1);
    w.live
        .controller
        .advance_notice_clock(Duration::from_secs(3601));
    w.tick(1);
    w.wait_woken(&second);
    assert_eq!(listed(&w.port.wake_sends()[1]), vec![second]);
}

#[test]
fn three_requests_that_waited_ride_one_send_and_a_closed_one_and_an_above_one_are_not_in_it() {
    let w = world(&["u1", "u2", "u3", "u4", "u5"], Start::Running);
    for u in ["u1", "u2", "u3", "u4"] {
        w.stall(u);
    }
    w.stop_file("u5");
    let ids: Vec<(String, u32)> = ["u1", "u2", "u3", "u4", "u5"]
        .iter()
        .map(|u| w.request(u))
        .collect();
    w.tick(6);
    assert!(w.port.wake_sends().is_empty());
    // The fourth closes while the others wait.
    close(&w, &ids[3].0, ids[3].1);
    w.tick(2);
    assert_eq!(w.acked(), Some(0));

    w.grant();
    let sends = w.port.wake_sends();
    assert_eq!(sends.len(), 1, "{sends:?}");
    assert_eq!(
        listed(&sends[0]),
        vec![ids[0].0.clone(), ids[1].0.clone(), ids[2].0.clone()]
    );
    assert_eq!(data_lines(&sends[0]).len(), 3);
    assert_eq!(w.wake_items().len(), 1);
    w.wait_woken(&ids[0].0);
    w.wait_woken(&ids[1].0);
    w.wait_woken(&ids[2].0);
    // The above request is not woken, and the agent's position did not move for any wake.
    assert!(!w.is_woken(&ids[4].0));
    assert_eq!(w.acked(), Some(0));
    assert_eq!(w.turn_results("notice"), 1);
    assert_ne!(w.outcome().as_deref(), Some("carried"));
}

#[test]
fn a_woken_request_is_not_listed_again_and_a_halted_turn_leaves_its_requests_unmarked() {
    let w = world(&["u1", "u2"], Start::Running);
    w.grant();
    w.stall("u1");
    let (first, _) = w.request("u1");
    w.wait_woken(&first);
    // A second request after the first wake's turn ended gets a wake of its own: it lists it alone.
    w.stall("u2");
    let (second, _) = w.request("u2");
    w.wait_woken(&second);
    let sends = w.port.wake_sends();
    assert_eq!(sends.len(), 2);
    assert_eq!(listed(&sends[0]), vec![first]);
    assert_eq!(listed(&sends[1]), vec![second]);
    drop(w);

    // A wake turn that `halt` cut leaves its requests unmarked; with the run halted, none of them
    // is in a later wake.
    let w = world(&["u1"], Start::Running);
    w.port.swallow_wakes();
    w.grant();
    w.stall("u1");
    let (id, _) = w.request("u1");
    w.wait("the wake's send", || w.port.wake_sends().len() == 1);
    w.begin(TurnCause::Notice);
    let out = w.live.cli(&["halt", "--reason", "stop it"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(!w.is_woken(&id));
    w.tick(10);
    assert_eq!(w.port.wake_sends().len(), 1);
    assert_eq!(w.wake_items().len(), 1);
}

/// A woken turn whose stored result is an error is no proof that the block arrived: it marks no
/// request woken, and the end of that turn's check wakes them again. The control: a result that is
/// no error marks them.
#[test]
fn a_woken_turn_that_ends_in_an_error_marks_nothing_and_the_request_is_woken_again() {
    let w = world(&["u1"], Start::Running);
    w.port.swallow_wakes();
    w.grant();
    w.stall("u1");
    let (id, _) = w.request("u1");
    w.wait("the wake's send", || w.port.wake_sends().len() == 1);
    w.begin(TurnCause::Notice);
    w.end_with(TurnCause::Notice, true);
    assert!(!w.is_woken(&id), "an error result marked the request woken");
    // The turn's end runs the check: the request is in a second wake.
    w.wait("the second wake's send", || w.port.wake_sends().len() == 2);
    assert_eq!(listed(&w.port.wake_sends()[1]), vec![id.clone()]);
    assert_eq!(w.wake_items().len(), 2, "each wake item counts");
    // The control: a result that is no error marks the request.
    w.begin(TurnCause::Notice);
    w.end(TurnCause::Notice);
    assert!(w.is_woken(&id));
}

#[test]
fn a_session_held_at_the_cap_gets_no_wake_at_any_of_the_three_points() {
    let w = world(&["u1", "u2", "u3", "u4"], Start::Running);
    w.port.swallow_wakes();
    w.set("chat.self_turn_cap", "1");
    w.grant();
    w.stall("u1");
    let (first, _) = w.request("u1");
    w.wait("the wake's send", || w.port.wake_sends().len() == 1);
    w.begin(TurnCause::Notice);
    // A request opens during the woken turn; the turn's end meets the cap.
    w.stall("u2");
    w.end(TurnCause::Notice);
    w.wait_woken(&first);
    w.tick(2);
    assert_eq!(
        w.port.wake_sends().len(),
        1,
        "the turn's end woke past the cap"
    );
    assert!(!w.ledger("wake_held").is_empty());
    // The notice step finds the request each tick: still none.
    w.tick(6);
    assert_eq!(w.port.wake_sends().len(), 1);
    // `grant clear` finds it too: none. A new request waits beside it.
    w.revoke();
    w.stall("u3");
    w.grant();
    assert_eq!(w.port.wake_sends().len(), 1, "the grant woke past the cap");
    w.tick(4);
    assert_eq!(w.port.wake_sends().len(), 1);
    assert_eq!(w.wake_items().len(), 1);
}

#[test]
fn a_notice_that_rode_an_operators_message_is_not_in_the_wake_the_turns_end_sends() {
    let w = world(&["u1", "u2"], Start::Running);
    w.stall("u1");
    let (carried, _) = w.request("u1");
    // The operator's message carries the waiting notice. Its turn has not begun: the send is in the
    // process and the test feeds the turn's events itself.
    w.port.swallow_operator(true);
    w.say("what waits?");
    w.wait("the operator's send", || w.port.operator_sends().len() == 1);
    assert!(w.port.operator_sends()[0].contains(&format!("request {carried}")));
    // The grant is given and another request opens before the turn: no wake goes into it.
    w.grant();
    w.stall("u2");
    let (second, _) = w.request("u2");
    w.tick(4);
    assert!(
        w.port.wake_sends().is_empty(),
        "a wake went into an open turn"
    );
    w.begin(TurnCause::Operator);
    w.tick(2);
    assert!(
        w.port.wake_sends().is_empty(),
        "a wake went into an open turn"
    );
    // The turn's result acknowledges the carry first, so the end's check lists the new request alone.
    w.end(TurnCause::Operator);
    w.wait_acked(1);
    assert_eq!(w.outcome().as_deref(), Some("carried"));
    w.wait_woken(&second);
    let sends = w.port.wake_sends();
    assert_eq!(sends.len(), 1, "{sends:?}");
    assert_eq!(listed(&sends[0]), vec![second]);
    assert!(!w.is_woken(&carried));
}

#[test]
fn the_open_points_a_wake_turned_back_on_a_new_session_and_a_halted_run() {
    // `autopilot.wake` set back to `["below"]` wakes the waiting request at the next tick.
    let w = world(&["u1"], Start::Running);
    w.set("autopilot.wake", "[\"off\"]");
    w.grant();
    w.stall("u1");
    let (id, _) = w.request("u1");
    w.tick(6);
    assert!(w.port.wake_sends().is_empty());
    w.set("autopilot.wake", "[\"below\"]");
    w.tick(1);
    w.wait_woken(&id);
    assert_eq!(w.port.wake_sends().len(), 1);
    drop(w);

    // Ending the session and starting a new one wakes the request in the new session at the next
    // tick.
    let w = world(&["u1"], Start::Running);
    w.grant();
    cli_json(&w.live, &["session", "end", &w.session]);
    w.stall("u1");
    let (id, _) = w.request("u1");
    w.tick(6);
    assert!(w.port.wake_sends().is_empty());
    let started = cli_json(&w.live, &["session", "start", "--project", &w.project]);
    let fresh = started["session"]["id"].as_str().unwrap().to_string();
    assert_ne!(fresh, w.session);
    w.tick(2);
    w.wait_woken(&id);
    assert_eq!(w.wake_items_of(&fresh).len(), 1);
    assert!(w.wake_items_of(&w.session).is_empty());
    drop(w);

    // A halted run's waiting request never wakes, at any point.
    let w = world(&["u1"], Start::NoGrant);
    let out = w.live.cli(&["halt", "--reason", "stop it"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    w.grant();
    w.tick(4);
    w.set("autopilot.wake", "[\"off\"]");
    w.set("autopilot.wake", "[\"below\"]");
    w.tick(4);
    cli_json(&w.live, &["session", "end", &w.session]);
    let started = cli_json(&w.live, &["session", "start", "--project", &w.project]);
    assert!(started["session"]["id"].is_string());
    w.tick(6);
    assert!(w.port.wake_sends().is_empty());
    assert!(w.ledger("wake").is_empty());
}

#[test]
fn a_request_is_woken_at_most_once_and_one_revised_before_its_wake_is_listed_once_at_its_revision()
{
    // A woken request that is revised while open is in no later wake; it rides the carry.
    let w = world(&["u1"], Start::GrantThenStall);
    let (id, rev) = w.request("u1");
    w.wait_woken(&id);
    revise(&w, &id, rev);
    w.tick(8);
    assert_eq!(w.port.wake_sends().len(), 1);
    assert_eq!(w.wake_items().len(), 1);
    w.say("anything new?");
    wait_turns(&w.live, &w.session, 2);
    let carried = w.port.operator_sends();
    assert_eq!(carried.len(), 1);
    assert!(
        carried[0].contains(&format!("request {id}")),
        "{}",
        carried[0]
    );
    assert_eq!(w.port.wake_sends().len(), 1);
    drop(w);

    // A request that opened and was revised before any wake is listed once, at its revision.
    let w = world(&["u1"], Start::NoGrant);
    let (id, rev) = w.request("u1");
    revise(&w, &id, rev);
    let (_, now) = w.request("u1");
    assert_eq!(now, rev + 1);
    w.grant();
    let sends = w.port.wake_sends();
    assert_eq!(sends.len(), 1, "{sends:?}");
    let lines = request_lines(&sends[0]);
    assert_eq!(lines.len(), 1, "{}", sends[0]);
    let kv = pairs(lines[0]);
    assert_eq!(kv[0], ("request".to_string(), id.clone()));
    assert_eq!(kv[1], ("revision".to_string(), now.to_string()));
    w.wait_woken(&id);
    assert_eq!(
        w.ledger("woken"),
        vec![format!(
            "woken {id}@{now} session=S-1 wake=wake:S-1:{}",
            w.wake_items()[0].0
        )]
    );
}

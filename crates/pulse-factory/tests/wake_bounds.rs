//! AC-2 of `r2.s3.w4`: what bounds a wake (`docs/contracts/requests.md`, "The wake"; ADR-0015's
//! cap; ADR-0016's two `autopilot.*` keys). `chat.self_turn_cap` holds a wake, and its count is read
//! from the store, so a release of the chat's process and a restart reset nothing; only an operator
//! message does. `autopilot.wakes_per_hour` holds a wake by the hour. Each held wake writes one
//! `wake_held` line. A read of a bound that fails gives no wake.
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
use pf_engine::port::chat::TurnCause;
use pf_engine::port::store::{
    AdmissionOutcome, CONTROLLER_SCOPE, Effect, SettingRow, SettingScope, Store,
};
use serde_json::json;
use settle_rig::commit;
use wake_rig::*;

/// Park a unit on a stall and wait until its wake's turn has marked it woken.
fn wake_one(w: &W, unit: &str) -> String {
    w.stall(unit);
    let (id, _) = w.request(unit);
    w.wait_woken(&id);
    id
}

/// Release the chat's process, as the idle sweep does after its idle time.
fn release(w: &W) {
    std::thread::sleep(Duration::from_millis(650));
    w.live.controller.sweep_chats();
    assert!(
        w.live.controller.live_chat_sessions().is_empty(),
        "the process was not released"
    );
}

/// The `reason=` word of a `wake_held` line.
fn reason_of(line: &str) -> &str {
    let at = line.find("reason=").expect("a reason") + "reason=".len();
    line[at..].split(' ').next().unwrap()
}

/// Write a stored value that this binary cannot read, at the project, through the test's own
/// connection to the store.
fn store_unreadable(w: &W, key: &str, id: &str) {
    commit(
        &w.live,
        "setting_set",
        id,
        json!({ "key": key }),
        vec![Effect::SetSetting(SettingRow {
            scope: SettingScope::new(Some(&w.project), None),
            key: key.to_string(),
            value: "\"not a value\"".to_string(),
            set_by: "cli".to_string(),
            set_at: "2026-11-28T00:00:00.000Z".to_string(),
        })],
    );
}

const SLOW: Duration = Duration::from_millis(600);

fn world_idle(units: &[&str]) -> W {
    world_with(units, Start::Running, |cfg| cfg.chat_idle = SLOW)
}

#[test]
fn the_cap_holds_the_third_wake_and_one_held_line_says_why_until_a_new_request_moves_it() {
    let w = world(&["u1", "u2", "u3", "u4"], Start::Running);
    w.set("chat.self_turn_cap", "2");
    w.grant();
    wake_one(&w, "u1");
    wake_one(&w, "u2");
    assert_eq!(w.wake_items().len(), 2);
    assert_eq!(w.port.wake_sends().len(), 2);
    assert!(w.ledger("wake_held").is_empty());
    let acked = w.acked();

    // The third request gets no wake. Its notice waits, and one line says why.
    w.stall("u3");
    let (third, _) = w.request("u3");
    w.tick(2);
    assert_eq!(w.wake_items().len(), 2);
    assert_eq!(w.port.wake_sends().len(), 2);
    let held = w.ledger("wake_held");
    assert_eq!(held.len(), 1, "{held:?}");
    assert_eq!(reason_of(&held[0]), "self_turn_cap");
    assert!(
        held[0].contains(&format!("requests={third} ")),
        "{}",
        held[0]
    );
    assert!(held[0].contains("count=2/2"), "{}", held[0]);
    assert!(!w.is_woken(&third));
    assert_eq!(w.acked(), acked, "a held wake moves no position");

    // Twelve more ticks write no second line.
    w.tick(12);
    assert_eq!(w.ledger("wake_held").len(), 1);
    assert_eq!(w.port.wake_sends().len(), 2);

    // A new request moves the top position, so it writes one more line, and the line lists both.
    w.stall("u4");
    let (fourth, _) = w.request("u4");
    w.tick(2);
    let held = w.ledger("wake_held");
    assert_eq!(held.len(), 2, "{held:?}");
    assert_eq!(reason_of(&held[1]), "self_turn_cap");
    assert!(
        held[1].contains(&format!("requests={third},{fourth} ")),
        "{}",
        held[1]
    );
    w.tick(12);
    assert_eq!(w.ledger("wake_held").len(), 2);
    assert_eq!(w.wake_items().len(), 2);
}

#[test]
fn a_process_at_the_cap_from_background_turns_gets_no_wake_so_no_woken_turn_is_cut() {
    let w = world(&["u1"], Start::Running);
    w.set("chat.self_turn_cap", "2");
    w.grant();
    // Two turns that a background task started: the process is at the cap, the stored count is 0.
    for _ in 0..2 {
        w.begin(TurnCause::Task);
        w.end(TurnCause::Task);
    }
    w.stall("u1");
    w.tick(6);
    assert!(w.wake_items().is_empty());
    assert!(w.port.wake_sends().is_empty());
    let held = w.ledger("wake_held");
    assert_eq!(held.len(), 1, "{held:?}");
    assert_eq!(reason_of(&held[0]), "self_turn_cap");
    assert!(held[0].contains("count=0/2"), "{}", held[0]);
    // No turn was cut: no stop for the cap, and the process was not stopped at all.
    assert!(
        w.chat_lines()
            .iter()
            .all(|l| !l.contains("reason=self_turn_cap")),
        "{:?}",
        w.chat_lines()
    );
    assert_eq!(w.rec.stop_count(), 0);
}

/// Two wakes at cap 2, then the process goes. Returns the world, with a third request waiting.
fn at_cap_with_the_process_gone(restart: bool) -> W {
    let w = world_idle(&["u1", "u2", "u3", "u4"]);
    w.set("chat.self_turn_cap", "2");
    w.grant();
    wake_one(&w, "u1");
    wake_one(&w, "u2");
    release(&w);
    if restart {
        // What a restart forgets: the controller's memory of the notices, and the wake count's cache.
        w.live.controller.forget_notices();
    }
    w.stall("u3");
    w.tick(4);
    w
}

fn count_survives(restart: bool) {
    let w = at_cap_with_the_process_gone(restart);
    // The store holds two wakes after the last operator message, so the third request is held, and
    // no process was opened for it.
    assert_eq!(w.wake_items().len(), 2);
    assert_eq!(w.port.wake_sends().len(), 2);
    let held = w.ledger("wake_held");
    assert_eq!(held.len(), 1, "{held:?}");
    assert_eq!(reason_of(&held[0]), "self_turn_cap");
    assert!(w.live.controller.live_chat_sessions().is_empty());

    // An operator message resets the count (its item is newer than the wakes). Its turn carries the
    // waiting notice, and the next request is woken.
    w.say("I am here");
    wait_turns(&w.live, &w.session, 3);
    w.stall("u4");
    let (fourth, _) = w.request("u4");
    w.wait_woken(&fourth);
    assert_eq!(w.wake_items().len(), 3);
    let (_, block) = w.wake_items().pop().unwrap();
    assert!(block.contains(&format!("request={fourth} ")), "{block}");
}

#[test]
fn the_count_comes_from_the_store_so_a_release_of_the_process_resets_nothing() {
    count_survives(false);
}

#[test]
fn the_count_comes_from_the_store_so_a_restart_resets_nothing() {
    count_survives(true);
}

#[test]
fn a_wakes_item_has_the_code_wake_and_a_seq_below_its_turn_and_the_count_reads_it() {
    let w = world(&["u1"], Start::GrantThenStall);
    let (id, _) = w.request("u1");
    w.wait_woken(&id);
    let wakes = w.wake_items();
    assert_eq!(wakes.len(), 1);
    let seq = wakes[0].0;
    // The command ID names the item, and the item was stored before the turn it started.
    let command = format!("wake:S-1:{seq}");
    let row = w
        .live
        .controller
        .store()
        .admission(CONTROLLER_SCOPE, &command)
        .unwrap()
        .expect("the wake's admission");
    assert_eq!(row.outcome, AdmissionOutcome::Admitted);
    assert_eq!(row.kind, "wake");
    let result = items(&w.live, &w.session)
        .into_iter()
        .find(|i| i.kind == "turn_result" && i.payload.contains("\"notice\""))
        .expect("the woken turn's result");
    assert!(
        seq < result.seq,
        "the wake's item {seq} is after {}",
        result.seq
    );
}

#[test]
fn a_carry_stores_no_wake_item_so_it_never_counts_and_the_next_request_is_woken() {
    let w = world(&["u1", "u2", "u3", "u4"], Start::Running);
    w.set("chat.self_turn_cap", "2");
    // Three requests below the line, more than the cap, with no grant: they wait.
    for u in ["u1", "u2", "u3"] {
        w.stall(u);
    }
    w.say("what is waiting?");
    wait_turns(&w.live, &w.session, 1);
    // The carry rode the operator's message and stored no `wake` item.
    assert!(w.wake_items().is_empty());
    assert_eq!(w.outcome().as_deref(), Some("carried"));
    // The grant wakes nothing: the carry acknowledged those notices. A new request is woken, at
    // count 0 of 2.
    w.grant();
    w.stall("u4");
    let (fourth, _) = w.request("u4");
    w.wait_woken(&fourth);
    assert_eq!(w.wake_items().len(), 1);
    assert!(
        w.ledger("wake")[0].contains("count=1/2"),
        "{:?}",
        w.ledger("wake")
    );
    assert!(w.ledger("wake_held").is_empty());
}

#[test]
fn session_redact_of_a_wake_item_takes_it_out_of_the_count() {
    let w = world_idle(&["u1", "u2", "u3"]);
    w.set("chat.self_turn_cap", "2");
    w.grant();
    wake_one(&w, "u1");
    wake_one(&w, "u2");
    // The process is released, so only the stored count holds the third request (a live process
    // at the cap holds it too, by its own count).
    release(&w);
    w.stall("u3");
    let (third, _) = w.request("u3");
    w.tick(3);
    assert!(!w.is_woken(&third));
    assert_eq!(w.ledger("wake_held").len(), 1);

    // The operator redacts the first wake's item: the count reads the session again.
    let (seq, _) = w.wake_items()[0].clone();
    let out = w
        .live
        .cli(&["session", "redact", &w.session, &seq.to_string()]);
    assert_eq!(common::code(&out), 0, "{}", common::stderr(&out));
    w.tick(2);
    w.wait_woken(&third);
    assert_eq!(w.port.wake_sends().len(), 3);
}

#[test]
fn the_hour_holds_a_wake_by_the_items_time_and_a_clock_an_hour_on_frees_a_slot() {
    let w = world(&["u1", "u2", "u3"], Start::Running);
    w.set("chat.self_turn_cap", "0");
    w.set("autopilot.wakes_per_hour", "2");
    w.grant();
    wake_one(&w, "u1");
    wake_one(&w, "u2");
    w.stall("u3");
    let (third, _) = w.request("u3");
    w.tick(3);
    let held = w.ledger("wake_held");
    assert_eq!(held.len(), 1, "{held:?}");
    assert_eq!(reason_of(&held[0]), "wakes_per_hour");
    assert!(held[0].contains("hour=2/2"), "{}", held[0]);
    assert!(!w.is_woken(&third));
    w.tick(10);
    assert_eq!(w.ledger("wake_held").len(), 1);

    // The notice clock moves an hour on: the two wakes are older than the hour.
    w.live
        .controller
        .advance_notice_clock(Duration::from_secs(3601));
    w.tick(1);
    w.wait_woken(&third);
    assert_eq!(w.wake_items().len(), 3);
}

#[test]
fn with_the_cap_at_zero_wakes_run_up_to_the_hour_and_with_the_hour_at_zero_none_runs() {
    // Cap 0 is off: three wakes with no operator message, then the hour holds the fourth.
    let w = world(&["u1", "u2", "u3", "u4"], Start::Running);
    w.set("chat.self_turn_cap", "0");
    w.set("autopilot.wakes_per_hour", "3");
    w.grant();
    for u in ["u1", "u2", "u3"] {
        wake_one(&w, u);
    }
    w.stall("u4");
    w.tick(3);
    assert_eq!(w.wake_items().len(), 3);
    let held = w.ledger("wake_held");
    assert_eq!(held.len(), 1, "{held:?}");
    assert_eq!(reason_of(&held[0]), "wakes_per_hour");
    assert!(
        w.ledger("wake").iter().all(|l| l.contains("count=")),
        "{:?}",
        w.ledger("wake")
    );
    drop(w);

    // The hour at 0 is off, whatever the cap.
    let w = world(&["u1"], Start::Running);
    w.set("chat.self_turn_cap", "50");
    w.set("autopilot.wakes_per_hour", "0");
    w.grant();
    w.stall("u1");
    w.tick(6);
    assert!(w.wake_items().is_empty());
    assert!(w.port.wake_sends().is_empty());
    let held = w.ledger("wake_held");
    assert_eq!(held.len(), 1, "{held:?}");
    assert_eq!(reason_of(&held[0]), "wakes_per_hour");
}

/// A stored value that the binary cannot read gives no wake at all, and never the built-in. When
/// the value is written again, the waiting request is woken.
fn unreadable_gives_no_wake(key: &str) {
    let w = world(&["u1"], Start::Running);
    w.grant();
    store_unreadable(&w, key, &format!("setting-unreadable-{key}"));
    w.stall("u1");
    let (id, _) = w.request("u1");
    w.tick(8);
    assert!(
        w.wake_items().is_empty(),
        "{key}: a wake ran on a failed read"
    );
    assert!(w.port.wake_sends().is_empty());
    assert!(
        w.ledger("wake_held").is_empty(),
        "{key}: a failed read is not a held wake"
    );
    assert!(!w.is_woken(&id));

    // The value is written again, and the next tick wakes the request.
    let good = if key == "autopilot.wake" {
        "[\"below\"]"
    } else {
        "3"
    };
    w.set(key, good);
    w.tick(1);
    w.wait_woken(&id);
    assert_eq!(w.wake_items().len(), 1);
}

#[test]
fn a_failed_read_of_autopilot_wake_gives_no_wake() {
    unreadable_gives_no_wake("autopilot.wake");
}

#[test]
fn a_failed_read_of_autopilot_wakes_per_hour_gives_no_wake() {
    unreadable_gives_no_wake("autopilot.wakes_per_hour");
}

#[test]
fn a_failed_read_of_the_cap_gives_no_wake_where_a_chats_own_cap_keeps_its_built_in() {
    unreadable_gives_no_wake("chat.self_turn_cap");
}

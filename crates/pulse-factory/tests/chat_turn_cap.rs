//! AC-11 of `r1.s4.w4`: the cap on the turns a chat starts on its own. `Rec` over `FakeChat`; the
//! turns are fed with `apply_chat_event`, except the end-to-end ones, which run `fake:task` through
//! the real pump. No test here starts `claude`, spends a token or reaches beyond `127.0.0.1`.

mod common;

use std::sync::Arc;

use common::chat::*;
use common::ctl::Live;
use pf_adapter_fake::FakeChat;
use pf_engine::port::agent::Usage;
use pf_engine::port::chat::{ChatEvent, TurnCause, cap};
use pf_engine::port::store::{ChatSessionState, Store};
use serde_json::Value;

const CAP: &str = "chat.self_turn_cap";

/// A live controller with one started session `S-1` on project `P-1`.
fn session() -> (Live, Arc<Rec>, String) {
    let rec = Rec::new(strict_fake());
    let live = start(rec.clone());
    let (_, s) = started(&live, "alpha");
    assert_eq!(s, "S-1");
    (live, rec, s)
}

fn turn_results(live: &Live, s: &str) -> usize {
    count_kind(live, s, "turn_result")
}

/// One operator message, answered whole by the fake.
fn say(live: &Live, s: &str, text: &str) {
    let before = turn_results(live, s);
    cli_json(live, &["session", "send", s, text]);
    wait_turns(live, s, before + 1);
}

/// A turn the chat starts on its own begins. It counts.
fn begin(live: &Live, s: &str, cause: TurnCause) {
    live.controller
        .apply_chat_event(s, ChatEvent::TurnStarted { cause })
        .unwrap_or_else(|e| panic!("{s}: {e}"));
}

fn end(live: &Live, s: &str, cause: TurnCause) {
    live.controller
        .apply_chat_event(
            s,
            ChatEvent::TurnResult {
                cause,
                usage: Usage::default(),
                is_error: false,
                stop: "end_turn".into(),
                duration_ms: 1,
                cache_ttl_seconds: None,
            },
        )
        .unwrap_or_else(|e| panic!("{s}: {e}"));
}

/// One self-started turn, whole.
fn whole(live: &Live, s: &str, cause: TurnCause) {
    begin(live, s, cause);
    end(live, s, cause);
}

fn set_cap(live: &Live, value: &str, scope: &[&str]) {
    let mut args = vec!["settings", "set", CAP, value];
    args.extend_from_slice(scope);
    cli_json(live, &args);
}

fn cap_lines(live: &Live) -> Vec<String> {
    chat_ledger(live)
        .into_iter()
        .filter(|l| l.contains("reason=self_turn_cap"))
        .collect()
}

fn notices(live: &Live, s: &str, code: &str) -> Vec<Value> {
    items(live, s)
        .iter()
        .filter(|i| i.kind == "notice")
        .map(|i| serde_json::from_str::<Value>(&i.payload).unwrap())
        .filter(|v| v["code"] == code)
        .collect()
}

fn state(live: &Live, s: &str) -> ChatSessionState {
    live.controller
        .store()
        .chat_session(s)
        .unwrap()
        .unwrap()
        .state
}

fn is_live(live: &Live, s: &str) -> bool {
    live.controller.live_chat_sessions().iter().any(|x| x == s)
}

/// The process of `s` was cut: stopped once, interrupted once, held at `needs_you`.
fn assert_cut(live: &Live, rec: &Rec, s: &str) {
    assert_eq!(rec.interrupt_count(), 1, "one interrupt");
    assert_eq!(rec.stop_count(), 1, "one stop");
    assert!(!is_live(live, s), "the process left the table");
    assert_eq!(state(live, s), ChatSessionState::NeedsYou);
}

#[test]
fn the_default_cap_lets_three_run_whole_and_cuts_the_fourth() {
    let (live, rec, s) = session();
    say(&live, &s, "hello");
    for _ in 0..3 {
        whole(&live, &s, TurnCause::Task);
    }
    assert_eq!(rec.stop_count(), 0, "three turns are within the cap of 3");
    assert_eq!(rec.interrupt_count(), 0);
    assert!(is_live(&live, &s));
    assert!(cap_lines(&live).is_empty());

    begin(&live, &s, TurnCause::Task);

    assert_cut(&live, &rec, &s);
    let seen = notices(&live, &s, "self_turn_cap");
    assert_eq!(seen.len(), 1, "{seen:?}");
    let text = seen[0]["text"].as_str().unwrap_or_default().to_string();
    assert!(text.contains("3 turns on its own"), "{seen:?}");
    assert!(text.contains("your next message resumes it"), "{seen:?}");
    let lines = cap_lines(&live);
    assert_eq!(lines.len(), 1, "{lines:#?}");
    assert!(
        lines[0].contains("stop session=S-1 reason=self_turn_cap self_turns=4 cap=3 from=built_in"),
        "{}",
        lines[0]
    );
}

#[test]
fn a_notice_turn_counts_as_a_task_turn_does() {
    let (live, rec, s) = session();
    say(&live, &s, "hello");
    for _ in 0..3 {
        whole(&live, &s, TurnCause::Notice);
    }
    assert_eq!(rec.stop_count(), 0);
    begin(&live, &s, TurnCause::Notice);
    assert_cut(&live, &rec, &s);
    assert_eq!(cap_lines(&live).len(), 1);
}

#[test]
fn an_operator_message_or_turn_resets_the_count() {
    let (live, rec, s) = session();
    say(&live, &s, "one");
    for _ in 0..3 {
        whole(&live, &s, TurnCause::Task);
    }
    // A message resets the count: three more run whole.
    say(&live, &s, "two");
    for _ in 0..3 {
        whole(&live, &s, TurnCause::Task);
    }
    assert_eq!(rec.stop_count(), 0, "the message reset the count");

    // An operator turn start resets it too.
    whole(&live, &s, TurnCause::Operator);
    for _ in 0..3 {
        whole(&live, &s, TurnCause::Task);
    }
    assert_eq!(rec.stop_count(), 0, "an operator turn reset the count");
    assert!(cap_lines(&live).is_empty());
    assert!(is_live(&live, &s));
}

#[test]
fn a_cap_of_zero_is_off() {
    let (live, rec, s) = session();
    set_cap(&live, "0", &[]);
    say(&live, &s, "hello");
    for _ in 0..10 {
        whole(&live, &s, TurnCause::Task);
    }
    assert_eq!(rec.stop_count(), 0);
    assert_eq!(rec.interrupt_count(), 0);
    assert!(cap_lines(&live).is_empty());
    assert!(is_live(&live, &s));
}

#[test]
fn a_cap_of_one_lets_one_run_and_cuts_the_second() {
    let (live, rec, s) = session();
    set_cap(&live, "1", &[]);
    say(&live, &s, "hello");
    whole(&live, &s, TurnCause::Task);
    assert_eq!(rec.stop_count(), 0);
    begin(&live, &s, TurnCause::Task);
    assert_cut(&live, &rec, &s);
    let lines = cap_lines(&live);
    assert_eq!(lines.len(), 1, "{lines:#?}");
    assert!(
        lines[0].contains("self_turns=2 cap=1 from=server"),
        "{}",
        lines[0]
    );
}

#[test]
fn the_real_pump_runs_a_task_turn_whole_and_a_fed_second_one_is_cut() {
    let (live, rec, s) = session();
    set_cap(&live, "1", &[]);
    // The operator's turn, then the task finishes and the fake starts a `Task` turn by itself.
    cli_json(&live, &["session", "send", &s, "fake:task 50"]);
    wait_for(&live, &s, "the operator turn and the task turn", |i| {
        i.iter().filter(|x| x.kind == "turn_result").count() >= 2
    });
    let causes: Vec<String> = items(&live, &s)
        .iter()
        .filter(|i| i.kind == "turn_result")
        .map(|i| {
            serde_json::from_str::<Value>(&i.payload).unwrap()["cause"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(causes, ["operator", "task"]);
    assert_eq!(
        rec.stop_count(),
        0,
        "one self-started turn is within a cap of 1"
    );
    assert!(is_live(&live, &s));

    begin(&live, &s, TurnCause::Task);
    assert_cut(&live, &rec, &s);
    assert_eq!(cap_lines(&live).len(), 1);
}

#[test]
fn a_project_row_sets_the_cap_for_that_projects_chats_only() {
    let (live, rec, s1) = session();
    let beta = add_project(&live, &repo(&live, "beta"));
    let s2 = cli_json(&live, &["session", "start", "--project", &beta])["session"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    set_cap(&live, "2", &["--project", "P-1"]);
    say(&live, &s1, "hello");
    say(&live, &s2, "hello");

    // The other project's chat has the built-in 3: three run whole.
    for _ in 0..3 {
        whole(&live, &s2, TurnCause::Task);
    }
    assert!(is_live(&live, &s2));
    assert_eq!(rec.stop_count(), 0);

    // P-1's chat stops at its third.
    whole(&live, &s1, TurnCause::Task);
    whole(&live, &s1, TurnCause::Task);
    assert!(is_live(&live, &s1));
    begin(&live, &s1, TurnCause::Task);
    assert!(!is_live(&live, &s1));
    assert!(is_live(&live, &s2), "the other chat runs on");
    assert_eq!(rec.stop_count(), 1);
    let lines = cap_lines(&live);
    assert_eq!(lines.len(), 1, "{lines:#?}");
    assert!(
        lines[0].contains("session=S-1 reason=self_turn_cap self_turns=3 cap=2 from=project:P-1"),
        "{}",
        lines[0]
    );
}

#[test]
fn a_change_applies_at_the_next_self_started_turn_with_no_relaunch() {
    let (live, rec, s) = session();
    say(&live, &s, "hello");
    whole(&live, &s, TurnCause::Task);
    whole(&live, &s, TurnCause::Task);
    assert!(is_live(&live, &s));

    // The count is 2 and the cap 3. Lower the cap to 2: the next turn stops the live process.
    set_cap(&live, "2", &[]);
    assert!(is_live(&live, &s), "a settings change touches no process");
    assert_eq!(rec.open_count(), 1);
    begin(&live, &s, TurnCause::Task);
    assert_cut(&live, &rec, &s);
    assert_eq!(rec.open_count(), 1, "nothing was relaunched");
    assert!(cap_lines(&live)[0].contains("self_turns=3 cap=2 from=server"));
}

#[test]
fn a_pending_approval_is_closed_lost_and_the_state_stays_needs_you() {
    let (live, rec, s) = session();
    say(&live, &s, "hello");
    for _ in 0..3 {
        whole(&live, &s, TurnCause::Task);
    }
    live.controller
        .apply_chat_event(
            &s,
            ChatEvent::ApprovalRequest {
                request_id: "r1".into(),
                tool: "Bash".into(),
                input: cap("{}"),
                reason: None,
            },
        )
        .unwrap();
    begin(&live, &s, TurnCause::Task);

    assert_cut(&live, &rec, &s);
    assert!(
        live.controller
            .store()
            .pending_requests(&s)
            .unwrap()
            .is_empty()
    );
    let answer = items(&live, &s)
        .into_iter()
        .find(|i| i.kind == "approval_answer")
        .expect("a lost answer");
    let body: Value = serde_json::from_str(&answer.payload).unwrap();
    assert_eq!(body["outcome"], "lost");
    assert_eq!(body["reason"], "stopped at the self-started turn cap");
}

#[test]
fn the_interrupted_turns_late_result_is_stored_and_the_state_stays_needs_you() {
    let (live, rec, s) = session();
    set_cap(&live, "1", &[]);
    // The operator's turn ends at once; the fake's task finishes after 600 ms and starts a `Task`
    // turn through the real pump. A fed `Task` turn comes first, so the fake's is the second.
    cli_json(&live, &["session", "send", &s, "fake:task 600"]);
    wait_turns(&live, &s, 1);
    whole(&live, &s, TurnCause::Task);
    assert_eq!(rec.stop_count(), 0);

    wait_state(&live, &s, ChatSessionState::NeedsYou);
    wait_for(&live, &s, "the late turn result", |i| {
        i.iter().filter(|x| x.kind == "turn_result").count() >= 3
    });
    assert_cut(&live, &rec, &s);
    assert_eq!(
        state(&live, &s),
        ChatSessionState::NeedsYou,
        "the late result keeps it"
    );
    let turns: Vec<String> = chat_ledger(&live)
        .into_iter()
        .filter(|l| l.contains(" turn session=S-1 "))
        .collect();
    assert_eq!(turns.len(), 3, "{turns:#?}");
    assert_eq!(cap_lines(&live).len(), 1);
}

#[test]
fn the_next_message_resumes_the_session_and_starts_the_count_over() {
    let (live, rec, s) = session();
    set_cap(&live, "1", &[]);
    say(&live, &s, "hello");
    let stored = live
        .controller
        .store()
        .chat_session(&s)
        .unwrap()
        .unwrap()
        .runtime_session_ref
        .unwrap();
    whole(&live, &s, TurnCause::Task);
    begin(&live, &s, TurnCause::Task);
    assert_cut(&live, &rec, &s);

    let before = turn_results(&live, &s);
    cli_json(&live, &["session", "send", &s, "back again"]);
    wait_turns(&live, &s, before + 1);
    assert_eq!(rec.open_count(), 2, "the send opened a second process");
    assert_eq!(rec.last_open().resume.map(|r| r.0), Some(stored));
    assert!(is_live(&live, &s));
    assert_ne!(
        state(&live, &s),
        ChatSessionState::NeedsYou,
        "the hold is gone"
    );

    // The count started over: one self-started turn is within the cap of 1.
    whole(&live, &s, TurnCause::Task);
    assert!(is_live(&live, &s));
    assert_eq!(cap_lines(&live).len(), 1, "still one cap line");
}

#[test]
fn halt_stops_a_chat_mid_turn_and_leaves_a_held_chat_needs_you() {
    let (live, rec, s) = session();
    say(&live, &s, "hello");
    // `halt` during the first `Task` turn, cap 3.
    begin(&live, &s, TurnCause::Task);
    assert!(is_live(&live, &s));
    let v = cli_json(&live, &["halt", "--reason", "test"]);
    assert_eq!(v["chats_stopped"][0], "S-1", "{v}");
    assert_eq!(rec.stop_count(), 1);
    assert!(cap_lines(&live).is_empty(), "a halt is not a cap stop");
    assert!(
        chat_ledger(&live)
            .iter()
            .any(|l| l.contains("stop session=S-1 reason=halt")),
    );
    assert_eq!(state(&live, &s), ChatSessionState::Idle);

    // A held chat: reopen, run to the cap, then halt. The session stays `needs_you`.
    set_cap(&live, "1", &[]);
    say(&live, &s, "again");
    whole(&live, &s, TurnCause::Task);
    begin(&live, &s, TurnCause::Task);
    assert_eq!(state(&live, &s), ChatSessionState::NeedsYou);
    let v = cli_json(&live, &["halt", "--reason", "after the cap"]);
    assert!(v["chats_stopped"].as_array().unwrap().is_empty(), "{v}");
    assert_eq!(state(&live, &s), ChatSessionState::NeedsYou);
}

#[test]
fn no_item_line_or_answer_holds_the_text_of_a_message() {
    let rec = Rec::new(FakeChat::new());
    let live = start(rec.clone());
    let (_, s) = started(&live, "alpha");
    let secret = "the-private-message-text";
    say(&live, &s, secret);
    for _ in 0..4 {
        begin(&live, &s, TurnCause::Task);
        if !is_live(&live, &s) {
            break;
        }
        end(&live, &s, TurnCause::Task);
    }
    assert_eq!(cap_lines(&live).len(), 1);
    for note in notices(&live, &s, "self_turn_cap") {
        assert!(!note.to_string().contains(secret), "{note}");
    }
    for line in chat_ledger(&live)
        .iter()
        .chain(controller_ledger(&live).iter())
    {
        assert!(!line.contains(secret), "{line}");
    }
}

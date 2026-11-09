//! AC-7 of `r1.s3.w3`: the lifetime of a chat process. A `FakeChat`, a short idle window through
//! `Config`, and the sweep called by the test. No test starts `claude`.

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use common::chat::*;
use common::ctl::Live;
use common::*;
use pf_engine::port::store::{ChatSessionState, Store};
use pf_ipc::Handler;
use pulse_factory::controller::chats::parse_chat_env;
use pulse_factory::controller::{Config, Controller};
use serde_json::{Value, json};

/// A live controller whose idle window is zero: a process with no holder is released at a sweep.
fn short_idle(rec: &std::sync::Arc<Rec>) -> Live {
    start_with(rec.clone(), |cfg| cfg.chat_idle = Duration::ZERO)
}

fn notices(live: &Live, session: &str, code: &str) -> usize {
    items(live, session)
        .iter()
        .filter(|i| i.kind == "notice")
        .filter(|i| serde_json::from_str::<Value>(&i.payload).unwrap()["code"] == code)
        .count()
}

fn stops(live: &Live, reason: &str) -> usize {
    chat_ledger(live)
        .iter()
        .filter(|l| l.contains(" chat stop ") && l.contains(&format!("reason={reason}")))
        .count()
}

fn session_of(live: &Live, id: &str) -> Value {
    let v = cli_json(live, &["session", "list", "--all"]);
    v["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id)
        .unwrap_or_else(|| panic!("{id} in {v}"))
        .clone()
}

#[test]
fn an_idle_process_is_released_at_the_sweep() {
    let rec = Rec::new(strict_fake());
    let live = short_idle(&rec);
    let (_, s) = started(&live, "alpha");
    assert_eq!(live.controller.live_chat_sessions(), [s.as_str()]);
    assert_eq!(rec.stop_count(), 0);

    live.controller.sweep_chats();

    assert_eq!(rec.stop_count(), 1);
    assert!(live.controller.live_chat_sessions().is_empty());
    assert_eq!(notices(&live, &s, "idle_release"), 1);
    assert_eq!(stops(&live, "idle"), 1);
    let record = session_of(&live, &s);
    assert!(record["process"].is_null(), "{record}");
    assert_eq!(record["state"], "idle");
    // The session itself stays live.
    assert!(record["ended_at"].is_null());
}

#[test]
fn a_turn_a_background_task_and_a_pending_card_each_hold_the_process() {
    let rec = Rec::new(strict_fake());
    let live = short_idle(&rec);
    let (_, slow) = started(&live, "alpha");
    let beta = add_project(&live, &repo(&live, "beta"));
    let gamma = add_project(&live, &repo(&live, "gamma"));
    let delta = add_project(&live, &repo(&live, "delta"));
    let start = |p: &str| {
        cli_json(&live, &["session", "start", "--project", p])["session"]["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let task = start(&beta);
    let card = start(&gamma);
    let free = start(&delta);

    cli_json(&live, &["session", "send", &slow, "fake:slow 4000"]);
    wait_state(&live, &slow, ChatSessionState::Working);
    cli_json(&live, &["session", "send", &task, "fake:task 60000"]);
    wait_turns(&live, &task, 1);
    cli_json(&live, &["session", "send", &card, "fake:approval"]);
    wait_state(&live, &card, ChatSessionState::NeedsYou);

    live.controller.sweep_chats();

    // Only the process with no holder is released.
    assert_eq!(rec.stop_count(), 1);
    let mut left = live.controller.live_chat_sessions();
    left.sort();
    let mut want = vec![slow.clone(), task.clone(), card.clone()];
    want.sort();
    assert_eq!(left, want);
    assert_eq!(notices(&live, &free, "idle_release"), 1);
    for held in [&slow, &task, &card] {
        assert_eq!(notices(&live, held, "idle_release"), 0, "{held}");
    }
    // The list names each holder with its time.
    let record = session_of(&live, &slow);
    assert!(record["process"]["turn_since"].is_string(), "{record}");
    assert!(record["process"]["releases_at"].is_null());
    let record = session_of(&live, &task);
    assert_eq!(record["process"]["tasks"].as_array().unwrap().len(), 1);
    assert!(record["process"]["tasks"][0]["since"].is_string());
    let record = session_of(&live, &card);
    assert!(record["process"]["waiting_since"].is_string(), "{record}");
}

#[test]
fn nothing_but_the_next_send_reopens_a_released_process() {
    let rec = Rec::new(strict_fake());
    let live = short_idle(&rec);
    let (_, s) = started(&live, "alpha");
    let stored = live
        .controller
        .store()
        .chat_session(&s)
        .unwrap()
        .unwrap()
        .runtime_session_ref
        .unwrap();
    live.controller.sweep_chats();
    assert_eq!(rec.open_count(), 1);

    for _ in 0..5 {
        live.controller.sweep_chats();
        live.controller.tick_all();
    }
    assert_eq!(rec.open_count(), 1, "no sweep and no tick opens a process");
    assert!(live.controller.live_chat_sessions().is_empty());

    let v = cli_json(&live, &["session", "send", &s, "hello again"]);
    assert_eq!(v["session"]["id"], s.as_str());
    wait_turns(&live, &s, 1);
    assert_eq!(rec.open_count(), 2);
    let spec = rec.last_open();
    assert_eq!(spec.resume.map(|r| r.0), Some(stored));
    let texts: Vec<_> = items(&live, &s)
        .into_iter()
        .filter(|i| i.kind == "assistant_text")
        .collect();
    assert_eq!(texts.len(), 1);
    assert!(texts[0].payload.contains("fake reply: hello again"));
}

/// The cap is 1 and `S-1` holds its process by `prompt`. Answer the refusal of a start in another
/// project.
fn refusal_while(prompt: Option<&str>, ready: impl Fn(&Live, &str)) -> (Live, String, String) {
    let rec = Rec::new(strict_fake());
    let live = start_with(rec, |cfg| cfg.chat_max_live = 1);
    let (_, s) = started(&live, "alpha");
    if let Some(prompt) = prompt {
        cli_json(&live, &["session", "send", &s, prompt]);
        ready(&live, &s);
    }
    let beta = add_project(&live, &repo(&live, "beta"));
    let why = cli_refused(&live, &["session", "start", "--project", &beta]);
    (live, s, why)
}

#[test]
fn the_cap_refusal_names_the_holder_why_and_since_when() {
    // Idle: the process is held until the idle window passes.
    let (live, s, why) = refusal_while(None, |_, _| {});
    assert!(why.contains("the chat process cap (1)"), "{why}");
    assert!(why.contains(&format!("{s} is idle until")), "{why}");
    assert!(why.contains("session end"), "{why}");
    // The refused start made no session and no process.
    assert_eq!(live.controller.live_chat_sessions(), [s.as_str()]);
    let list = cli_json(&live, &["session", "list"]);
    assert_eq!(list["sessions"].as_array().unwrap().len(), 1);

    // A pending card counts against the cap (C2).
    let (live, s, why) = refusal_while(Some("fake:approval"), |live, s| {
        wait_state(live, s, ChatSessionState::NeedsYou)
    });
    assert!(
        why.contains(&format!("{s} waits for an answer since")),
        "{why}"
    );
    assert!(session_of(&live, &s)["process"]["waiting_since"].is_string());

    // A background task holds it (C7).
    let (live, s, why) = refusal_while(Some("fake:task 60000"), |live, s| wait_turns(live, s, 1));
    assert!(why.contains(&format!("{s} holds background task")), "{why}");
    assert!(why.contains("since"), "{why}");
    assert_eq!(
        session_of(&live, &s)["process"]["tasks"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // A running turn.
    let (_live, s, why) = refusal_while(Some("fake:slow 4000"), |live, s| {
        wait_state(live, s, ChatSessionState::Working)
    });
    assert!(why.contains(&format!("{s} runs a turn since")), "{why}");
}

#[test]
fn halt_with_no_run_stops_every_chat_and_the_next_send_reopens_one() {
    let rec = Rec::new(strict_fake());
    let live = start_with(rec.clone(), |_| {});
    let (_, s1) = started(&live, "alpha");
    let beta = add_project(&live, &repo(&live, "beta"));
    let s2 = cli_json(&live, &["session", "start", "--project", &beta])["session"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    cli_json(&live, &["session", "send", &s1, "fake:approval"]);
    wait_state(&live, &s1, ChatSessionState::NeedsYou);

    let v = cli_json(&live, &["halt", "--reason", "test"]);
    assert!(v["run"].is_null(), "{v}");
    let mut stopped: Vec<String> = v["chats_stopped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect();
    stopped.sort();
    assert_eq!(stopped, [s1.clone(), s2.clone()]);
    assert_eq!(rec.stop_count(), 2);
    assert_eq!(stops(&live, "halt"), 2);
    assert!(live.controller.live_chat_sessions().is_empty());

    // The card was closed as lost, and each session stays live and idle.
    assert!(
        live.controller
            .store()
            .pending_requests(&s1)
            .unwrap()
            .is_empty()
    );
    let answer = items(&live, &s1)
        .into_iter()
        .find(|i| i.kind == "approval_answer")
        .expect("a lost answer");
    let body: Value = serde_json::from_str(&answer.payload).unwrap();
    assert_eq!(body["outcome"], "lost");
    assert_eq!(body["reason"], "halted");
    for s in [&s1, &s2] {
        let row = live.controller.store().chat_session(s).unwrap().unwrap();
        assert!(row.ended_at.is_none(), "{s} is live");
        assert_eq!(row.state, ChatSessionState::Idle);
        assert_eq!(notices(&live, s, "halted"), 1, "{s}");
    }

    // The text form names the stopped chats.
    let out = live.cli(&["halt", "--reason", "again"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));

    // The next message reopens one, with a new confirm.
    let opens = rec.open_count();
    cli_json(&live, &["session", "send", &s2, "back"]);
    wait_turns(&live, &s2, 1);
    assert_eq!(rec.open_count(), opens + 1);
    assert_eq!(live.controller.live_chat_sessions(), [s2.as_str()]);
}

#[test]
fn a_fresh_controller_on_the_same_state_finds_the_session_idle_and_a_send_resumes_it() {
    let rec = Rec::new(strict_fake());
    let live = start_with(rec.clone(), |_| {});
    let (_, s) = started(&live, "alpha");
    cli_json(&live, &["session", "send", &s, "fake:approval"]);
    wait_state(&live, &s, ChatSessionState::NeedsYou);
    // A controller stop ends the processes first.
    live.controller.shutdown_chats();
    assert_eq!(stops(&live, "shutdown"), 1);

    let mut cfg = Config::new(live.sb.state.clone(), 3_600_000, BIN.into());
    cfg.chat_runtimes.insert(RUNTIME.to_string(), rec.clone());
    cfg.server_name = "test-server".to_string();
    let second = Controller::open(cfg, live.ports.clone()).unwrap();

    let row = second.store().chat_session(&s).unwrap().unwrap();
    assert_eq!(row.state, ChatSessionState::Idle);
    assert!(row.ended_at.is_none());
    assert!(second.live_chat_sessions().is_empty());
    assert!(second.store().pending_requests(&s).unwrap().is_empty());

    let opens = rec.open_count();
    let resp = second.handle(pf_ipc::Request::new(
        "session_send",
        json!({"session_id": s, "text": "after the restart", "command_id": "c-after"}),
    ));
    assert!(resp.ok, "{:?}", resp.error);
    let end = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let n = second
            .store()
            .chat_items_after(&s, 0, 1000)
            .unwrap()
            .iter()
            .filter(|i| i.kind == "turn_result")
            .count();
        if n >= 1 {
            break;
        }
        assert!(std::time::Instant::now() < end, "no turn after the restart");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(rec.open_count(), opens + 1);
    assert_eq!(
        rec.last_open().resume.map(|r| r.0),
        row.runtime_session_ref,
        "the resume names the stored ref"
    );
    assert_eq!(second.live_chat_sessions(), [s.as_str()]);
    second.shutdown_chats();
}

#[test]
fn a_ref_the_runtime_does_not_know_is_context_lost_and_never_a_fresh_open() {
    let rec = Rec::new(strict_fake());
    let live = short_idle(&rec);
    let (_, s) = started(&live, "alpha");
    live.controller.sweep_chats();
    live.controller
        .store()
        .set_chat_runtime_ref(&s, "unknown-ref", "2026-11-09T00:00:00Z")
        .unwrap();

    let why = cli_refused(&live, &["session", "send", &s, "hello"]);
    assert!(why.contains("context lost"), "{why}");
    assert_eq!(notices(&live, &s, "context_lost"), 1);
    let record = session_of(&live, &s);
    assert!(record["context_lost"].is_string(), "{record}");
    let attempts = rec.opens.lock().unwrap().len();
    assert_eq!(attempts, 2, "the start, and one failed resume");

    // A second send is refused at once, with no open.
    let why = cli_refused(&live, &["session", "send", &s, "again"]);
    assert!(why.contains("context lost"), "{why}");
    assert_eq!(rec.opens.lock().unwrap().len(), attempts);
    // Nothing ever opened a fresh context for a session that has a ref.
    let fresh = rec
        .opens
        .lock()
        .unwrap()
        .iter()
        .filter(|o| o.resume.is_none())
        .count();
    assert_eq!(fresh, 1, "only the first start opened with no resume");
    assert!(
        items(&live, &s)
            .iter()
            .all(|i| i.kind != "operator_message" || !i.payload.contains("again")),
        "a refused send leaves no operator message"
    );

    // The operator can end it.
    let out = live.cli(&["session", "end", &s]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let row = live.controller.store().chat_session(&s).unwrap().unwrap();
    assert!(row.ended_at.is_some());
}

#[test]
fn a_resume_in_a_deleted_worktree_is_refused_as_gone_and_marks_the_row() {
    let rec = Rec::new(strict_fake());
    let live = short_idle(&rec);
    let root = repo(&live, "alpha");
    let project = add_project(&live, &root);
    let s = cli_json(&live, &["session", "start", "--project", &project])["session"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let worktree = live
        .controller
        .store()
        .chat_session(&s)
        .unwrap()
        .unwrap()
        .worktree_id;
    live.controller.sweep_chats();
    std::fs::remove_dir_all(&root).unwrap();

    let why = cli_refused(&live, &["session", "send", &s, "hello"]);
    assert!(
        why.contains(&format!("the worktree {} is gone", root.display())),
        "{why}"
    );
    assert_eq!(rec.open_count(), 1, "no process opened");
    let row = live
        .controller
        .store()
        .worktree(&worktree)
        .unwrap()
        .unwrap();
    assert!(row.gone_at.is_some(), "the row is marked gone");
}

#[test]
fn a_bad_cap_setting_keeps_the_default_with_one_line() {
    let env = |v: &str| -> BTreeMap<String, String> {
        [("PF_CHAT_MAX_LIVE".to_string(), v.to_string())].into()
    };
    for bad in ["0", "65", "x"] {
        let (settings, warnings) = parse_chat_env(&env(bad), Some("host"));
        assert_eq!(settings.max_live, 4, "{bad}");
        assert_eq!(warnings.len(), 1, "{bad}: {warnings:?}");
        assert!(warnings[0].contains("PF_CHAT_MAX_LIVE"), "{warnings:?}");
    }
    for good in ["1", "64", "8"] {
        let (settings, warnings) = parse_chat_env(&env(good), Some("host"));
        assert_eq!(settings.max_live, good.parse::<usize>().unwrap());
        assert!(warnings.is_empty(), "{warnings:?}");
    }
    let (settings, warnings) = parse_chat_env(&BTreeMap::new(), Some("host"));
    assert_eq!(settings.max_live, 4);
    assert!(warnings.is_empty());
}

#[test]
fn a_withdrawn_request_restarts_the_idle_clock() {
    use pf_engine::port::chat::{ChatEvent, cap};

    let rec = Rec::new(strict_fake());
    let live = start_with(rec.clone(), |cfg| {
        cfg.chat_idle = Duration::from_millis(400)
    });
    let (_, s) = started(&live, "alpha");

    // The request holds the process. The idle clock set at the start is now older than the window.
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
    std::thread::sleep(Duration::from_millis(500));
    live.controller.sweep_chats();
    assert_eq!(rec.stop_count(), 0, "a pending request holds the process");

    // The runtime withdraws the request. The process is free from now, not from the start.
    live.controller
        .apply_chat_event(
            &s,
            ChatEvent::RequestClosed {
                request_id: "r1".into(),
            },
        )
        .unwrap();
    live.controller.sweep_chats();
    assert_eq!(
        rec.stop_count(),
        0,
        "the idle clock restarted at the withdrawal"
    );
    assert_eq!(notices(&live, &s, "idle_release"), 0);

    // The window then runs from the withdrawal, so the process is still released in time.
    std::thread::sleep(Duration::from_millis(500));
    live.controller.sweep_chats();
    assert_eq!(rec.stop_count(), 1);
    assert_eq!(notices(&live, &s, "idle_release"), 1);
}

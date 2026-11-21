//! AC-8 of `r2.s11.w3`: the orchestrator channel. A notice of a project's run is one `notice` item
//! in the project's newest Orchestrator session. The operator's next message carries the notices
//! that wait, as a marked block before the typed text, in one `Operator` send, when no turn is
//! open. The stored `OperatorMessage` holds the typed text alone. The agent acknowledges what a
//! message carried when the turn result of that message is stored, and a notice starts no turn
//! and has no timer (ADR-0015's amendment of 2026-11-21, `docs/contracts/requests.md`, "Notices").
//! The controller runs in the test process on a `FakeChat` behind a port that can fail a send or
//! swallow one. Nothing here starts `claude`, spends a token or reaches beyond `127.0.0.1`.

mod common;
#[path = "common/notice_rig.rs"]
mod rig;

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use common::chat::*;
use common::ctl::Live;
use pf_engine::port::chat::{
    BackgroundTask, ChatAnswer, ChatError, ChatId, ChatOpen, ChatOpened, ChatOption, ChatPort,
    ContextUsage, InputKind, MidTurn,
};
use pf_engine::port::store::{
    AdmissionOutcome, CONTROLLER_SCOPE, Commit, Committed, Effect, NewAdmission, NewDelivery,
    NewNotice, Store,
};
use rig::*;
use serde_json::{Value, json};

const MARK: &str = "[Pulse Factory notice. The controller wrote this, not the operator.]";
const END: &str = "[End of notice]";

/// The chat port of the tests: the recording `FakeChat`, and a send that can fail or be swallowed.
/// A swallowed send is taken and answered `Ok`, and no turn starts: the turn result never comes.
struct Flaky {
    inner: Arc<Rec>,
    /// 0 forwards, 1 fails, 2 swallows.
    mode: AtomicU8,
    /// Every text the port was asked to send, whatever became of it.
    log: Mutex<Vec<(InputKind, String)>>,
}

impl Flaky {
    fn set(&self, mode: u8) {
        self.mode.store(mode, Ordering::SeqCst);
    }

    fn sent(&self) -> Vec<String> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .map(|(_, t)| t.clone())
            .collect()
    }

    fn kinds(&self) -> Vec<InputKind> {
        self.log.lock().unwrap().iter().map(|(k, _)| *k).collect()
    }
}

impl ChatPort for Flaky {
    fn options(&self) -> Vec<ChatOption> {
        self.inner.options()
    }
    fn mid_turn(&self) -> MidTurn {
        self.inner.mid_turn()
    }
    fn open(&self, spec: &ChatOpen) -> Result<ChatOpened, ChatError> {
        self.inner.open(spec)
    }
    fn send(&self, chat: &ChatId, kind: InputKind, text: &str) -> Result<(), ChatError> {
        self.log.lock().unwrap().push((kind, text.to_string()));
        match self.mode.load(Ordering::SeqCst) {
            1 => Err(ChatError::Failed("the send failed for the test".into())),
            2 => Ok(()),
            _ => self.inner.send(chat, kind, text),
        }
    }
    fn answer(&self, chat: &ChatId, request_id: &str, a: &ChatAnswer) -> Result<(), ChatError> {
        self.inner.answer(chat, request_id, a)
    }
    fn interrupt(&self, chat: &ChatId) -> Result<(), ChatError> {
        self.inner.interrupt(chat)
    }
    fn set_option(&self, chat: &ChatId, id: &str, value: &str) -> Result<(), ChatError> {
        self.inner.set_option(chat, id, value)
    }
    fn context_usage(&self, chat: &ChatId) -> Result<ContextUsage, ChatError> {
        self.inner.context_usage(chat)
    }
    fn background_tasks(&self, chat: &ChatId) -> Result<Vec<BackgroundTask>, ChatError> {
        self.inner.background_tasks(chat)
    }
    fn is_alive(&self, chat: &ChatId) -> Result<bool, ChatError> {
        self.inner.is_alive(chat)
    }
    fn stop(&self, chat: &ChatId) -> Result<(), ChatError> {
        self.inner.stop(chat)
    }
}

struct World {
    live: Live,
    port: Arc<Flaky>,
    project: String,
    session: String,
    run: String,
}

/// A project with a live Orchestrator session, and an armed run of three units linked to it.
fn world() -> World {
    let rec = Rec::new(strict_fake());
    let port = Arc::new(Flaky {
        inner: rec.clone(),
        mode: AtomicU8::new(0),
        log: Mutex::new(Vec::new()),
    });
    let wrapped = port.clone();
    let live = start_with(rec, move |cfg| {
        cfg.chat_runtimes.insert(RUNTIME.to_string(), wrapped);
    });
    let (project, session) = started(&live, "alpha");
    live.arm_with_units(&["u1", "u2", "u3"]);
    let run = live.handle().info.run_id.clone();
    link(&live, &run, &project);
    World {
        live,
        port,
        project,
        session,
        run,
    }
}

/// The `notice` items of a session, as `(code, text)`.
fn notice_items(live: &Live, session: &str) -> Vec<(String, String)> {
    items(live, session)
        .into_iter()
        .filter(|i| i.kind == "notice")
        .filter_map(|i| {
            let v: Value = serde_json::from_str(&i.payload).ok()?;
            let code = v["code"].as_str()?.to_string();
            code.starts_with("request_")
                .then(|| (code, v["text"].as_str().unwrap_or("").to_string()))
        })
        .collect()
}

fn send(w: &World, text: &str) {
    cli_json(&w.live, &["session", "send", &w.session, text]);
}

/// The target's row, when it has one.
fn agent_row(w: &World) -> Option<pf_engine::port::store::NoticeTargetRow> {
    let target = format!("agent:{}:orchestrator", w.project);
    w.live
        .controller
        .store()
        .notice_targets()
        .unwrap()
        .into_iter()
        .find(|t| t.target == target)
}

/// A notice of a request that is no row of any run: the carry reads notices and deliveries, and
/// the test needs more open requests than a run has units to park.
fn raw_notice(w: &World, request: &str) -> u64 {
    let committed = w
        .live
        .controller
        .store()
        .commit(Commit {
            run_id: CONTROLLER_SCOPE.to_string(),
            command_id: format!("notice:{request}:1:opened"),
            content: json!({ "request": request, "revision": 1, "event": "opened" }).to_string(),
            admission: NewAdmission {
                kind: "notice".to_string(),
                outcome: AdmissionOutcome::Admitted,
                reason: None,
                unit_id: Some(pf_engine::port::store::NEW_ID.to_string()),
                role: None,
                model: None,
                attempt: None,
                admitted_at: "2026-11-21T00:00:00.000Z".to_string(),
            },
            effects: vec![Effect::NoticeAdd(NewNotice {
                request_id: request.to_string(),
                revision: 1,
                event: "opened".to_string(),
                run_id: w.run.clone(),
                project_id: Some(w.project.clone()),
                created_at: "2026-11-21T00:00:00.000Z".to_string(),
                deliveries: ["app", "orchestrator"]
                    .into_iter()
                    .map(|channel| NewDelivery {
                        channel: channel.to_string(),
                        text: format!("Request {request} waits."),
                        why: None,
                    })
                    .collect(),
            })],
            ledger: None,
        })
        .unwrap();
    match committed {
        Committed::Applied { admission, .. } => admission.unit_id.unwrap().parse().unwrap(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_notice_of_a_projects_run_is_one_item_in_its_newest_orchestrator_session_and_none_in_anothers()
{
    let w = world();
    let (_, other) = started(&w.live, "beta");
    w.live.park_unit("u1", "the reviewer found a defect");
    let (id, _) = request_of(&w.live, "u1");
    let got = notice_items(&w.live, &w.session);
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].0, "request_opened");
    let summary = requests_json(&w.live)
        .into_iter()
        .find(|r| r["id"] == id.as_str())
        .unwrap()["summary"]
        .as_str()
        .unwrap()
        .to_string();
    // The text of the channel, then the IDs that exist and the command.
    assert_eq!(
        got[0].1,
        format!(
            "{summary} (request {id}, run {}, unit u1, stage implement) Read it: pulse-factory show {id}",
            w.run
        )
    );
    assert!(notice_items(&w.live, &other).is_empty());
    // More ticks add no second item, and the send is recorded once.
    ticks(&w.live, 4);
    assert_eq!(notice_items(&w.live, &w.session).len(), 1);
    assert_eq!(
        ledger(&w.live, "notice_send"),
        vec!["notice_send 1 target=agent:P-1:orchestrator attempt=1"]
    );
    // The agent's row was made with the notice, just below it.
    let row = agent_row(&w).unwrap();
    assert_eq!((row.acked, row.outcome.as_str()), (0, "joined"));
    // A revision and the close each write their own item, with their own code.
    let engine = &w.live.handle().engine;
    let row = w.live.controller.store().request(&id).unwrap().unwrap();
    let mut body: Value = serde_json::from_str(&row.body).unwrap();
    body["note"] = json!("more");
    engine.revise_request(&id, 1, &body).unwrap();
    ticks(&w.live, 2);
    engine.withdraw_request(&id, "gone").unwrap();
    ticks(&w.live, 2);
    let codes: Vec<String> = notice_items(&w.live, &w.session)
        .into_iter()
        .map(|(c, _)| c)
        .collect();
    assert_eq!(
        codes,
        ["request_opened", "request_revised", "request_closed"]
    );
}

#[test]
fn the_next_operator_message_reaches_the_port_as_the_marked_block_a_blank_line_and_the_typed_text()
{
    let w = world();
    w.live.park_unit("u1", "the reviewer found a defect");
    let (id, _) = request_of(&w.live, "u1");
    send(&w, "what is going on?");
    common::chat::wait_turns(&w.live, &w.session, 1);
    let sent = w.port.sent();
    assert_eq!(sent.len(), 1, "{sent:?}");
    let lines: Vec<&str> = sent[0].lines().collect();
    assert_eq!(lines[0], MARK);
    assert!(
        lines[1].contains(&format!("(request {id}, run {}, unit u1", w.run)),
        "{}",
        lines[1]
    );
    assert!(
        lines[1].ends_with(&format!("Read it: pulse-factory show {id}")),
        "{}",
        lines[1]
    );
    assert_eq!(lines[2], END);
    assert_eq!(lines[3], "");
    assert_eq!(lines[4], "what is going on?");
    assert_eq!(lines.len(), 5);
    assert_eq!(w.port.kinds(), vec![InputKind::Operator]);
    // The stored message is the typed text alone.
    let stored: Vec<String> = items(&w.live, &w.session)
        .into_iter()
        .filter(|i| i.kind == "operator_message")
        .map(|i| {
            serde_json::from_str::<Value>(&i.payload).unwrap()["text"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(stored, ["what is going on?"]);
    // One more `notice_send` for the carry, and the acknowledgement after the turn result.
    assert_eq!(
        ledger(&w.live, "notice_send"),
        vec![
            "notice_send 1 target=agent:P-1:orchestrator attempt=1",
            "notice_send 1 target=agent:P-1:orchestrator attempt=2",
        ]
    );
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while agent_row(&w).unwrap().acked != 1 {
        assert!(
            std::time::Instant::now() < end,
            "the agent never acknowledged"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(agent_row(&w).unwrap().outcome, "carried");
    assert_eq!(
        ledger(&w.live, "notice_ack").last().unwrap(),
        "notice_ack agent:P-1:orchestrator position=1 outcome=carried"
    );
    // The next message carries nothing: the notice is acknowledged.
    send(&w, "and now?");
    common::chat::wait_turns(&w.live, &w.session, 2);
    assert_eq!(w.port.sent()[1], "and now?");
}

#[test]
fn the_block_lists_open_requests_newest_first_ten_at_most_then_the_count() {
    let w = world();
    // Twelve requests that wait, then one tick to write their items.
    let positions: Vec<u64> = (101..=112)
        .map(|n| raw_notice(&w, &format!("R-{n}")))
        .collect();
    assert_eq!(positions.len(), 12);
    ticks(&w.live, 2);
    send(&w, "status?");
    common::chat::wait_turns(&w.live, &w.session, 1);
    let sent = w.port.sent();
    let lines: Vec<&str> = sent[0].lines().collect();
    assert_eq!(lines[0], MARK);
    // Ten lines, newest first: R-112 down to R-103.
    for (i, line) in lines[1..11].iter().enumerate() {
        let want = format!("(request R-{}, run {}", 112 - i, w.run);
        assert!(line.contains(&want), "{line} should hold {want}");
    }
    assert_eq!(lines[11], "and 2 more: pulse-factory requests");
    assert_eq!(lines[12], END);
    assert_eq!(lines[13], "");
    assert_eq!(lines[14], "status?");
    // The acknowledgement covers all twelve.
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while agent_row(&w).unwrap().acked != *positions.last().unwrap() {
        assert!(
            std::time::Instant::now() < end,
            "the agent never acknowledged"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[test]
fn the_agent_is_acknowledged_only_after_the_turn_result_is_stored() {
    let w = world();
    w.live.park_unit("u1", "the reviewer found a defect");
    // The port takes the send and no turn starts: no turn result comes.
    w.port.set(2);
    send(&w, "first");
    assert!(w.port.sent()[0].starts_with(MARK));
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(
        agent_row(&w).unwrap().acked,
        0,
        "no turn result, no acknowledgement"
    );
    assert!(
        ledger(&w.live, "notice_ack")
            .iter()
            .all(|l| !l.contains("carried"))
    );
    // The next message carries the notice again, and its turn result acknowledges it.
    w.port.set(0);
    send(&w, "second");
    common::chat::wait_turns(&w.live, &w.session, 1);
    assert!(
        w.port.sent()[1].starts_with(MARK),
        "the notice still waited"
    );
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while agent_row(&w).unwrap().acked != 1 {
        assert!(
            std::time::Instant::now() < end,
            "the agent never acknowledged"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[test]
fn a_failed_send_and_an_ended_session_leave_the_notices_waiting_and_the_next_message_carries_them()
{
    let w = world();
    w.live.park_unit("u1", "the reviewer found a defect");
    // A failed send.
    w.port.set(1);
    let out = w.live.cli(&["session", "send", &w.session, "lost"]);
    let _ = out;
    assert_eq!(agent_row(&w).unwrap().acked, 0);
    // A send that is swallowed, and the session ends before any turn result.
    w.port.set(2);
    send(&w, "swallowed");
    cli_json(&w.live, &["session", "end", &w.session]);
    assert_eq!(
        agent_row(&w).unwrap().acked,
        0,
        "an ended session acknowledges nothing"
    );
    // A new session of the project: its first message carries the notice.
    w.port.set(0);
    let v = cli_json(&w.live, &["session", "start", "--project", &w.project]);
    let next = v["session"]["id"].as_str().unwrap().to_string();
    cli_json(&w.live, &["session", "send", &next, "hello again"]);
    common::chat::wait_turns(&w.live, &next, 1);
    let sent = w.port.sent();
    let last = sent.last().unwrap();
    assert!(
        last.starts_with(MARK) && last.ends_with("hello again"),
        "{last}"
    );
    // The notice was an item of the first session already: the new session holds the block in the
    // message, and no second item.
    assert_eq!(notice_items(&w.live, &w.session).len(), 1);
    assert!(notice_items(&w.live, &next).is_empty());
}

#[test]
fn a_message_during_an_open_turn_carries_nothing() {
    let w = world();
    // A slow turn: the typed text is the script, so no block may come before it.
    send(&w, "fake:slow 1500");
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while w.port.sent().is_empty() {
        assert!(std::time::Instant::now() < end);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    w.live.park_unit("u1", "the reviewer found a defect");
    send(&w, "second, in the turn");
    let sent = w.port.sent();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1], "second, in the turn");
    assert_eq!(agent_row(&w).unwrap().acked, 0);
}

#[test]
fn twelve_ticks_with_no_message_call_the_ports_send_zero_times() {
    let w = world();
    w.live.park_unit("u1", "the reviewer found a defect");
    ticks(&w.live, 12);
    assert!(w.port.sent().is_empty(), "{:?}", w.port.sent());
    // A notice starts no turn: no `turn_result`, and the item only.
    assert_eq!(count_kind(&w.live, &w.session, "turn_result"), 0);
    assert_eq!(notice_items(&w.live, &w.session).len(), 1);
}

#[test]
fn a_project_with_no_session_keeps_its_notices_and_the_first_message_of_its_next_session_carries_them()
 {
    let rec = Rec::new(strict_fake());
    let port = Arc::new(Flaky {
        inner: rec.clone(),
        mode: AtomicU8::new(0),
        log: Mutex::new(Vec::new()),
    });
    let wrapped = port.clone();
    let live = start_with(rec, move |cfg| {
        cfg.chat_runtimes.insert(RUNTIME.to_string(), wrapped);
    });
    let project = add_project(&live, &repo(&live, "alpha"));
    live.arm_with_units(&["u1", "u2"]);
    let run = live.handle().info.run_id.clone();
    link(&live, &run, &project);
    live.park_unit("u1", "the reviewer found a defect");
    live.park_unit("u2", "the build is red");
    ticks(&live, 3);
    // Two notices wait, with no item written anywhere.
    assert_eq!(notices(&live).len(), 2);
    assert!(ledger(&live, "notice_send").is_empty());
    let v = cli_json(&live, &["session", "start", "--project", &project]);
    let session = v["session"]["id"].as_str().unwrap().to_string();
    cli_json(&live, &["session", "send", &session, "hello"]);
    wait_turns(&live, &session, 1);
    let sent = port.sent();
    let lines: Vec<&str> = sent[0].lines().collect();
    assert_eq!(lines[0], MARK);
    assert_eq!(lines.len(), 6, "two lines, newest first: {lines:?}");
    assert!(
        lines[1].contains("unit u2") && lines[2].contains("unit u1"),
        "{lines:?}"
    );
    assert_eq!(notice_items(&live, &session).len(), 2);
}

#[test]
fn with_the_channel_off_the_message_reaches_the_port_as_typed() {
    let w = world();
    set(&w.live, "notify.orchestrator.on", "0", &[]);
    w.live.park_unit("u1", "the reviewer found a defect");
    assert!(
        notice_items(&w.live, &w.session).is_empty(),
        "an off channel writes no item"
    );
    send(&w, "plain");
    common::chat::wait_turns(&w.live, &w.session, 1);
    assert_eq!(w.port.sent(), vec!["plain".to_string()]);
    assert_eq!(
        state_of(&w.live, 1, "orchestrator"),
        "superseded:channel_off"
    );
}

#[test]
fn the_persona_holds_the_notice_paragraph() {
    let persona = pf_persona();
    assert!(persona.contains(MARK), "{persona}");
    assert!(persona.contains(END));
    assert!(persona.contains("You can answer none of them"), "{persona}");
}

fn pf_persona() -> String {
    pulse_factory::controller::agents::orchestrator_persona(
        "alpha",
        "/work/alpha",
        "test-server",
        "S-1",
        std::path::Path::new("/share"),
    )
}

//! AC-5 of `r1.s3.w3`: the chat verbs through the CLI. A live controller runs in the test process
//! with a strict `FakeChat` in the runtime table for the whole file (`effort` is not free: `low`,
//! `medium`, `high`, `xhigh`). Every repo is a temporary directory. No test starts `claude`.

mod common;

use common::chat::*;
use common::ctl::Live;
use common::*;
use pf_engine::port::store::{ChatSessionState, Store, WorktreeCreatedBy};
use pf_ipc::Handler;
use serde_json::{Value, json};

fn text_of(item: &pf_engine::port::store::ChatItemRow) -> Value {
    serde_json::from_str(&item.payload).unwrap()
}

#[test]
fn start_opens_the_orchestrator_in_the_projects_root() {
    let rec = Rec::new(strict_fake());
    let live = start_with(rec.clone(), |cfg| {
        cfg.chat_agent_env.insert(
            "ORCHESTRATOR".to_string(),
            [("EXAMPLE_ROLE".to_string(), "orchestrator".to_string())].into(),
        );
    });
    let root = repo(&live, "alpha");
    let project = add_project(&live, &root);

    let out = live.cli(&["session", "start", "--project", &project]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        stdout(&out).trim(),
        format!("session S-1 started in {} (worktree W-1)", root.display())
    );

    let store = live.controller.store();
    let row = store.chat_session("S-1").unwrap().unwrap();
    assert_eq!(row.worktree_id, "W-1");
    assert_eq!(
        store.worktree("W-1").unwrap().unwrap().created_by,
        WorktreeCreatedBy::Root
    );
    assert_eq!(ledger_kind(&live, "chat_session_start").len(), 1);
    assert_eq!(rec.open_count(), 1);
    // The runtime's own session ID is stored from the first moment.
    assert_eq!(row.runtime_session_ref.as_deref(), Some("fake-session-1"));
    let open_line = chat_ledger(&live)
        .into_iter()
        .find(|l| l.contains(" chat open "))
        .expect("a chat open line");
    assert!(
        open_line.contains("session=S-1") && open_line.contains("qualified=true"),
        "{open_line}"
    );
    assert!(open_line.contains("resume=no"), "{open_line}");

    // What the adapter was asked to open.
    let spec = rec.last_open();
    assert_eq!(spec.cwd, root.display().to_string());
    assert_eq!(spec.model, "claude-opus-5-5");
    assert_eq!(spec.effort, "high");
    assert_eq!(spec.permission_mode, "bypassPermissions");
    assert!(spec.resume.is_none());
    assert_eq!(
        spec.options,
        [("artifacts".to_string(), "on".to_string())].into()
    );
    assert_eq!(
        spec.env.get("EXAMPLE_ROLE").map(String::as_str),
        Some("orchestrator")
    );
    for word in ["alpha", &root.display().to_string(), "test-server", "S-1"] {
        assert!(spec.instructions.contains(word), "the persona names {word}");
    }
}

#[test]
fn a_send_is_one_admission_one_turn_and_one_cost() {
    let rec = Rec::new(strict_fake());
    let live = start(rec.clone());
    let (_, s) = started(&live, "alpha");

    let out = live.cli(&["session", "send", &s, "hello"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "sent to S-1");
    wait_turns(&live, &s, 1);

    // The ledger holds the byte count and not the text.
    let send_lines = ledger_kind(&live, "chat_send");
    assert_eq!(send_lines.len(), 1);
    assert!(send_lines[0].contains("bytes=5"), "{}", send_lines[0]);
    assert!(!send_lines[0].contains("hello"));
    assert!(!chat_ledger(&live).join("\n").contains("hello"));

    let log = items(&live, &s);
    let kinds: Vec<&str> = log.iter().map(|i| i.kind.as_str()).collect();
    assert_eq!(kinds, ["operator_message", "assistant_text", "turn_result"]);
    assert_eq!(text_of(&log[0])["text"], "hello");
    assert_eq!(text_of(&log[1])["text"], "fake reply: hello");
    // The session's cost is the turn's own: 3 x 5 + 15 x 17.
    let cost = |live: &Live| {
        live.controller
            .store()
            .chat_session("S-1")
            .unwrap()
            .unwrap()
            .cost_micros
    };
    assert_eq!(cost(&live), 3 * 5 + 15 * 17);

    cli_json(&live, &["session", "send", &s, "hi"]);
    wait_turns(&live, &s, 2);
    assert_eq!(cost(&live), (3 * 5 + 15 * 17) + (3 * 2 + 15 * 14));
    let turns: Vec<String> = chat_ledger(&live)
        .into_iter()
        .filter(|l| l.contains(" chat turn "))
        .collect();
    assert_eq!(turns.len(), 2);
    assert!(turns[0].contains("cost_micros=270"), "{}", turns[0]);
    assert!(turns[0].contains("cause=operator"), "{}", turns[0]);
}

#[test]
fn a_replay_changes_nothing_and_another_text_is_a_conflict() {
    let rec = Rec::new(strict_fake());
    let live = start(rec.clone());
    let (_, s) = started(&live, "alpha");

    let first = cli_json(
        &live,
        &["session", "send", &s, "hello", "--command-id", "c-1"],
    );
    assert_eq!(first["replayed"], false);
    wait_turns(&live, &s, 1);
    let (n_items, n_ledger, n_chat) = (
        items(&live, &s).len(),
        controller_ledger(&live).len(),
        chat_ledger(&live).len(),
    );

    let again = cli_json(
        &live,
        &["session", "send", &s, "hello", "--command-id", "c-1"],
    );
    assert_eq!(again["replayed"], true);
    assert_eq!(items(&live, &s).len(), n_items);
    assert_eq!(controller_ledger(&live).len(), n_ledger);
    assert_eq!(chat_ledger(&live).len(), n_chat);
    assert_eq!(rec.sent(0), ["hello"]);

    let why = cli_refused(
        &live,
        &["session", "send", &s, "other", "--command-id", "c-1"],
    );
    assert!(why.contains("c-1"), "{why}");
    assert_eq!(rec.sent(0), ["hello"]);
    assert_eq!(controller_ledger(&live).len(), n_ledger);
}

#[test]
fn approvals_and_questions_are_answered_by_seq() {
    let rec = Rec::new(strict_fake());
    let live = start(rec.clone());
    let (_, s) = started(&live, "alpha");

    cli_json(&live, &["session", "send", &s, "fake:approval"]);
    wait_for(&live, &s, "the approval request", |i| {
        i.iter().any(|x| x.kind == "approval_request")
    });
    wait_state(&live, &s, ChatSessionState::NeedsYou);
    let seq = seq_of(&live, &s, "approval_request");

    // A question's answer to an approval is refused, and the request stays pending.
    let why = cli_refused(
        &live,
        &[
            "session",
            "answer",
            &s,
            &seq.to_string(),
            "--answer",
            "Which one?=B",
        ],
    );
    assert!(why.contains("approval"), "{why}");

    let out = live.cli(&["session", "answer", &s, &seq.to_string(), "--deny", "no"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    wait_turns(&live, &s, 1);
    let log = items(&live, &s);
    let answer = log
        .iter()
        .find(|i| i.kind == "approval_answer")
        .expect("an answer item");
    assert_eq!(answer.answers_seq, Some(seq));
    let body = text_of(answer);
    assert_eq!(body["outcome"], "answered");
    assert_eq!(body["decision"], "deny");
    assert!(
        log.iter()
            .any(|i| i.kind == "assistant_text" && text_of(i)["text"] == "denied: no"),
        "the deny reached the turn"
    );
    wait_state(&live, &s, ChatSessionState::Idle);

    // A second answer to the same request is refused, with one refused line.
    let why = cli_refused(
        &live,
        &["session", "answer", &s, &seq.to_string(), "--allow"],
    );
    assert!(why.contains("already answered"), "{why}");
    let refused: Vec<String> = ledger_kind(&live, "chat_answer")
        .into_iter()
        .filter(|l| l.contains(" refused "))
        .collect();
    assert_eq!(
        refused.len(),
        2,
        "the wrong-kind and the repeated answer: {refused:?}"
    );

    cli_json(&live, &["session", "send", &s, "fake:question"]);
    wait_for(&live, &s, "the question request", |i| {
        i.iter().any(|x| x.kind == "question_request")
    });
    let qseq = seq_of(&live, &s, "question_request");
    cli_json(
        &live,
        &[
            "session",
            "answer",
            &s,
            &qseq.to_string(),
            "--answer",
            "Which one?=B",
        ],
    );
    wait_for(&live, &s, "the reply to the answer", |i| {
        i.iter()
            .any(|x| x.kind == "assistant_text" && text_of(x)["text"] == "answer: B")
    });
    let qa = items(&live, &s)
        .into_iter()
        .find(|i| i.kind == "question_answer")
        .unwrap();
    assert_eq!(text_of(&qa)["answers"]["Which one?"], "B");
}

#[test]
fn an_interrupt_ends_the_turn_and_with_no_turn_calls_nothing() {
    let rec = Rec::new(strict_fake());
    let live = start(rec.clone());
    let (_, s) = started(&live, "alpha");

    cli_json(&live, &["session", "send", &s, "fake:slow 2000"]);
    wait_state(&live, &s, ChatSessionState::Working);
    let out = live.cli(&["session", "interrupt", &s]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    wait_turns(&live, &s, 1);
    let turn = items(&live, &s)
        .into_iter()
        .find(|i| i.kind == "turn_result")
        .unwrap();
    assert_eq!(text_of(&turn)["stop"], "interrupted");

    // With no turn, it is admitted, adds no item and stops nothing.
    wait_state(&live, &s, ChatSessionState::Idle);
    let before = items(&live, &s).len();
    let out = live.cli(&["session", "interrupt", &s]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(items(&live, &s).len(), before);
    assert_eq!(rec.stop_count(), 0);
    assert_eq!(ledger_kind(&live, "chat_interrupt").len(), 2);
}

#[test]
fn a_set_changes_this_chat_only_and_the_next_start_takes_the_agents_defaults() {
    let rec = Rec::new(strict_fake());
    let live = start(rec.clone());
    let (project, s1) = started(&live, "alpha");
    let store = live.controller.store();
    let ref1 = store
        .chat_session("S-1")
        .unwrap()
        .unwrap()
        .runtime_session_ref;

    cli_json(&live, &["session", "set", &s1, "--effort", "medium"]);
    assert_eq!(store.chat_session("S-1").unwrap().unwrap().effort, "medium");
    assert!(
        rec.sets
            .lock()
            .unwrap()
            .iter()
            .any(|(_, id, v)| id == "effort" && v == "medium"),
        "the fake saw set_option effort medium"
    );
    assert_eq!(store.agent("orchestrator").unwrap().unwrap().effort, "high");

    // An undeclared effort and an undeclared option are refused with the declared values.
    let why = cli_refused(&live, &["session", "set", &s1, "--effort", "huge"]);
    assert!(why.contains("low, medium, high, xhigh"), "{why}");
    let why = cli_refused(&live, &["session", "set", &s1, "--option", "nope=on"]);
    assert!(why.contains("nope") && why.contains("artifacts"), "{why}");

    // After an end, a new start gets the agent's defaults and a fresh CLI context (open item B).
    cli_json(&live, &["session", "end", &s1]);
    let v = cli_json(&live, &["session", "start", "--project", &project]);
    let s2 = v["session"]["id"].as_str().unwrap().to_string();
    assert_eq!(s2, "S-2");
    let row2 = store.chat_session("S-2").unwrap().unwrap();
    assert_eq!(row2.effort, "high");
    assert!(rec.last_open().resume.is_none());
    assert_ne!(row2.runtime_session_ref, ref1);

    // A non-live option is stored on the row, pending while the process lives, and the next send
    // reopens the idle process with it. The session has had a message, so the reopen resumes.
    cli_json(&live, &["session", "send", &s2, "first"]);
    wait_turns(&live, &s2, 1);
    cli_json(&live, &["session", "set", &s2, "--option", "artifacts=off"]);
    let row2 = store.chat_session("S-2").unwrap().unwrap();
    assert_eq!(
        row2.runtime_options.get("artifacts").map(String::as_str),
        Some("off")
    );
    assert_eq!(
        store
            .agent("orchestrator")
            .unwrap()
            .unwrap()
            .runtime_options
            .get("artifacts")
            .map(String::as_str),
        Some("on")
    );
    let list = cli_json(&live, &["session", "list"]);
    let rec2 = &list["sessions"][0];
    assert_eq!(rec2["id"], "S-2");
    assert_eq!(rec2["pending_options"], json!({"artifacts": "off"}));
    let opens = rec.open_count();
    cli_json(&live, &["session", "send", &s2, "hello"]);
    wait_turns(&live, &s2, 2);
    assert_eq!(rec.open_count(), opens + 1);
    let spec = rec.last_open();
    assert_eq!(
        spec.options,
        [("artifacts".to_string(), "off".to_string())].into()
    );
    assert_eq!(
        spec.resume.as_ref().map(|r| r.0.clone()),
        row2.runtime_session_ref
    );
    assert!(
        chat_ledger(&live)
            .iter()
            .any(|l| l.contains("stop session=S-2 reason=option"))
    );

    // A fresh start after an end copies the agent's options again.
    cli_json(&live, &["session", "end", &s2]);
    cli_json(&live, &["session", "start", "--project", &project]);
    assert_eq!(
        rec.last_open().options,
        [("artifacts".to_string(), "on".to_string())].into()
    );
}

#[test]
fn an_end_stops_the_process_and_closes_the_requests() {
    let rec = Rec::new(strict_fake());
    let live = start(rec.clone());
    let (_, s) = started(&live, "alpha");

    cli_json(&live, &["session", "send", &s, "fake:approval"]);
    wait_state(&live, &s, ChatSessionState::NeedsYou);
    let seq = seq_of(&live, &s, "approval_request");
    cli_json(&live, &["session", "end", &s]);
    assert_eq!(rec.stop_count(), 1);
    let lost = items(&live, &s)
        .into_iter()
        .find(|i| i.kind == "approval_answer")
        .expect("a closing answer");
    assert_eq!(lost.answers_seq, Some(seq));
    assert_eq!(text_of(&lost)["outcome"], "lost");
    assert!(
        chat_ledger(&live)
            .iter()
            .any(|l| l.contains("stop session=S-1 reason=end"))
    );

    let why = cli_refused(&live, &["session", "send", &s, "again"]);
    assert!(why.contains("ended"), "{why}");

    // The list shows S-1 only with --all.
    assert_eq!(cli_json(&live, &["session", "list"])["sessions"], json!([]));
    let all = cli_json(&live, &["session", "list", "--all"]);
    assert_eq!(all["sessions"][0]["id"], "S-1");
    assert!(all["sessions"][0]["ended_at"].is_string());

    // A replay of the end stops nothing.
    let first = live.controller.store().admissions("@controller").unwrap();
    let end_id = first
        .iter()
        .find(|a| a.kind == "chat_session_end")
        .unwrap()
        .command_id
        .clone();
    cli_json(&live, &["session", "end", &s, "--command-id", &end_id]);
    assert_eq!(rec.stop_count(), 1);
}

#[test]
fn a_redaction_is_admitted_and_shows_in_the_items() {
    let rec = Rec::new(strict_fake());
    let live = start(rec.clone());
    let (_, s) = started(&live, "alpha");
    cli_json(&live, &["session", "send", &s, "a secret"]);
    wait_turns(&live, &s, 1);

    let out = live.cli(&["session", "redact", &s, "1"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(ledger_kind(&live, "chat_item_redact").len(), 1);
    assert!(!controller_ledger(&live).join("\n").contains("a secret"));

    let shown = cli_json(&live, &["session", "items", &s]);
    assert_eq!(shown["items"][0]["body"]["kind"], "redacted");
    assert_eq!(shown["items"][0]["body"]["item_kind"], "operator_message");
    // The reply echoes the text, and only item 1 is redacted: the redacted item holds no text.
    assert!(!shown["items"][0].to_string().contains("a secret"));
    let after = cli_json(&live, &["session", "items", &s, "--after", "1"]);
    assert_eq!(after["items"][0]["seq"], 2);
}

#[test]
fn the_list_holds_the_server_the_runtime_the_agent_and_each_sessions_facts() {
    let rec = Rec::new(strict_fake());
    let live = start(rec.clone());
    let (_, s) = started(&live, "alpha");
    cli_json(&live, &["session", "send", &s, "hello"]);
    wait_turns(&live, &s, 1);
    wait_state(&live, &s, ChatSessionState::Idle);
    // The context gauge is read after the turn, outside the pump's lock.
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let list = loop {
        let list = cli_json(&live, &["session", "list"]);
        if !list["sessions"][0]["context"].is_null() || std::time::Instant::now() > end {
            break list;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };

    assert!(list["server"]["id"].is_null(), "the API is off");
    assert_eq!(list["server"]["name"], "test-server");
    let runtime = &list["runtimes"][0];
    assert_eq!(runtime["id"], "claude-code");
    assert_eq!(runtime["mid_turn"], "steer");
    assert_eq!(runtime["options"].as_array().unwrap().len(), 4);
    assert_eq!(runtime["ready"], true);
    assert_eq!(list["agents"][0]["id"], "orchestrator");
    assert!(list["agents"][0].get("launch_env").is_none());

    let session = &list["sessions"][0];
    assert_eq!(session["branch"], "main");
    assert_eq!(session["detached"], false);
    assert_eq!(session["project_name"], "alpha");
    assert_eq!(session["usage"]["cost_micros"], 270);
    assert!(session["last_turn_at"].is_string());
    assert_eq!(session["cache_ttl_seconds"], 300);
    assert!(
        session["context"]["max_tokens"].as_u64().unwrap() > 0,
        "{session}"
    );
    assert_eq!(session["process"]["qualified"], true);
    assert!(session["process"]["releases_at"].is_string());
}

#[test]
fn the_text_forms_are_one_line_per_record() {
    let rec = Rec::new(strict_fake());
    let live = start(rec.clone());
    let (_, s) = started(&live, "alpha");
    cli_json(&live, &["session", "send", &s, "hello"]);
    wait_turns(&live, &s, 1);
    wait_state(&live, &s, ChatSessionState::Idle);

    let out = live.cli(&["session", "list"]);
    let text = stdout(&out);
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(
        text.starts_with("S-1  P-1 alpha  W-1 main  idle  claude-opus-5-5/high/bypassPermissions  $0.000270  process since "),
        "{text}"
    );
    let out = live.cli(&["session", "items", &s]);
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "{text}");
    assert_eq!(lines[0], "1  operator_message  hello");
    assert_eq!(lines[1], "2  assistant_text  fake reply: hello");
    assert!(
        lines[2].starts_with("3  turn_result  end_turn cost $0.000270"),
        "{}",
        lines[2]
    );
}

#[test]
fn under_a_seat_token_the_cli_refuses_each_state_change_and_runs_the_reads() {
    let rec = Rec::new(strict_fake());
    let live = start(rec.clone());
    let (_, s) = started(&live, "alpha");
    let seat = |args: &[&str]| {
        live.sb
            .cmd(args)
            .env("PF_SEAT_TOKEN", "x")
            .output()
            .unwrap()
    };
    for args in [
        vec!["session", "start", "--project", "P-1"],
        vec!["session", "send", &s, "hi"],
        vec!["session", "answer", &s, "1", "--allow"],
        vec!["session", "interrupt", &s],
        vec!["session", "set", &s, "--effort", "low"],
        vec!["session", "end", &s],
        vec!["session", "redact", &s, "1"],
    ] {
        let out = seat(&args);
        assert_eq!(code(&out), 1, "{args:?}");
        assert!(
            stderr(&out).contains("PF_SEAT_TOKEN"),
            "{args:?}: {}",
            stderr(&out)
        );
    }
    assert_eq!(code(&seat(&["session", "list"])), 0);
    assert_eq!(code(&seat(&["session", "items", &s])), 0);
    assert_eq!(rec.open_count(), 1);
    assert_eq!(ledger_kind(&live, "chat_send").len(), 0);
}

#[test]
fn the_controller_refuses_a_seat_and_a_chat_process_as_caller() {
    // A caller whose environment holds the seat token.
    let seat_proc = tempfile::tempdir().unwrap();
    fake_proc(seat_proc.path(), b"PF_SEAT_TOKEN=x\0", 1);
    let root = seat_proc.path().to_path_buf();
    let rec = Rec::new(strict_fake());
    let live = start_with(rec.clone(), move |cfg| cfg.proc_root = root);
    let (_, s) = started_via_controller(&live);
    let args = json!({"session_id": s, "text": "hi", "command_id": "c-seat"});
    let resp = pf_ipc::call(&live.sb.state, "session_send", args).unwrap();
    assert!(!resp.ok);
    assert!(resp.error.unwrap().contains("PF_SEAT_TOKEN"));
    assert_eq!(ledger_kind(&live, "chat_send").len(), 0);
    // The reads stay open to it.
    let resp = pf_ipc::call(&live.sb.state, "session_list", json!({})).unwrap();
    assert!(resp.ok, "{:?}", resp.error);

    // A caller that descends from the live chat process.
    let chat_proc = tempfile::tempdir().unwrap();
    fake_proc(chat_proc.path(), b"HOME=/x\0", 4242);
    let root = chat_proc.path().to_path_buf();
    let rec = Rec::new(strict_fake());
    let live = start_with(rec.clone(), move |cfg| cfg.proc_root = root);
    let (_, s) = started_via_controller(&live);
    rec.pids
        .lock()
        .unwrap()
        .insert(rec.chats.lock().unwrap()[0].clone(), 4242);
    let args = json!({"session_id": s, "text": "hi", "command_id": "c-chat"});
    let resp = pf_ipc::call(&live.sb.state, "session_send", args).unwrap();
    assert!(!resp.ok);
    let why = resp.error.unwrap();
    assert!(why.contains("chat process 4242"), "{why}");
    let resp = pf_ipc::call(
        &live.sb.state,
        "session_start",
        json!({"project_id": "P-1", "command_id": "c-chat-2"}),
    )
    .unwrap();
    assert!(!resp.ok);
    assert_eq!(ledger_kind(&live, "chat_send").len(), 0);
}

/// Start through the controller's own call, because the CLI child process would not pass a fake
/// `/proc` that names only the test process.
fn started_via_controller(live: &Live) -> (String, String) {
    let root = repo(live, "alpha");
    let project = add_project_via_controller(live, &root);
    let started = live.controller.handle(pf_ipc::Request::new(
        "session_start",
        json!({"project_id": project, "command_id": "start-1"}),
    ));
    assert!(started.ok, "{:?}", started.error);
    (
        project,
        started.result["session"]["id"]
            .as_str()
            .unwrap()
            .to_string(),
    )
}

fn add_project_via_controller(live: &Live, root: &std::path::Path) -> String {
    let added = live.controller.handle(pf_ipc::Request::new(
        "project_add",
        json!({"root": root.display().to_string(), "name": null, "command_id": "add-1"}),
    ));
    assert!(added.ok, "{:?}", added.error);
    added.result["project"]["id"].as_str().unwrap().to_string()
}

#[test]
fn an_agent_whose_runtime_is_not_on_this_server_is_refused_at_its_start() {
    // No runtime in the table at all.
    let live = Live::start();
    let root = repo(&live, "alpha");
    let project = add_project(&live, &root);
    let why = cli_refused(&live, &["session", "start", "--project", &project]);
    assert!(
        why.contains("no chat runtime claude-code on this server"),
        "{why}"
    );
    assert_eq!(ledger_kind(&live, "chat_session_start").len(), 1);
    assert!(
        live.controller
            .store()
            .chat_sessions(&project)
            .unwrap()
            .is_empty()
    );
}

/// PR #41, class C4: a text that starts with `-` is a value, not a flag.
#[test]
fn a_message_a_deny_text_and_an_option_value_may_start_with_a_hyphen() {
    let rec = Rec::new(strict_fake());
    let live = start(rec);
    let (_, s) = started(&live, "alpha");

    cli_json(&live, &["session", "send", &s, "- check the log"]);
    cli_json(&live, &["session", "send", &s, "-1 is the answer"]);
    wait_turns(&live, &s, 2);
    let sent: Vec<String> = items(&live, &s)
        .into_iter()
        .filter(|i| i.kind == "operator_message")
        .map(|i| text_of(&i)["text"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(sent, ["- check the log", "-1 is the answer"]);

    cli_json(&live, &["session", "send", &s, "fake:approval"]);
    wait_for(&live, &s, "the approval request", |i| {
        i.iter().any(|x| x.kind == "approval_request")
    });
    let seq = seq_of(&live, &s, "approval_request");
    cli_json(
        &live,
        &[
            "session",
            "answer",
            &s,
            &seq.to_string(),
            "--deny",
            "-x not now",
        ],
    );
    wait_for(&live, &s, "the deny text in the reply", |i| {
        i.iter()
            .any(|x| x.kind == "assistant_text" && text_of(x)["text"] == "denied: -x not now")
    });

    // The value reaches the controller, which refuses it as an option value and not as a flag.
    let why = cli_refused(&live, &["session", "set", &s, "--option", "artifacts=-x"]);
    assert!(
        !why.contains("unexpected argument") && why.contains("artifacts"),
        "{why}"
    );
}

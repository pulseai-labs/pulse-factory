//! AC-6 of `r1.s3.w3`: the pump's event function, fed hand-built sequences that pass
//! `check_order`, on a session of a live controller. No test starts `claude`.

mod common;

use std::collections::BTreeMap;

use common::chat::*;
use common::ctl::Live;
use pf_adapter_fake::FakeChat;
use pf_api::types::{ChatFrame, ChatItemBody};
use pf_engine::port::agent::{SessionRef, Usage};
use pf_engine::port::chat::{
    BackgroundTask, Capped, ChatEvent, TaskEnd, TurnCause, cap, check_order,
};
use pf_engine::port::store::{ChatSessionState, Store};
use serde_json::Value;
use tokio::sync::broadcast::Receiver;

/// A live controller with a started session `S-1`, and a reader of its frames.
fn session() -> (Live, Receiver<ChatFrame>, std::sync::Arc<Rec>) {
    let rec = Rec::new(FakeChat::new());
    let live = start(rec.clone());
    let (_project, id) = started(&live, "alpha");
    assert_eq!(id, "S-1");
    let frames = live.controller.subscribe_frames();
    (live, frames, rec)
}

/// Check the order of a sequence, then feed it event by event.
fn feed(live: &Live, events: Vec<ChatEvent>) {
    check_order(&events).unwrap_or_else(|e| panic!("the test's own sequence is bad: {e}"));
    for event in events {
        live.controller
            .apply_chat_event("S-1", event)
            .expect("a live process");
    }
}

fn drain(frames: &mut Receiver<ChatFrame>) -> Vec<ChatFrame> {
    let mut out = Vec::new();
    while let Ok(f) = frames.try_recv() {
        out.push(f);
    }
    out
}

fn payload(live: &Live, kind: &str) -> Vec<Value> {
    items(live, "S-1")
        .into_iter()
        .filter(|i| i.kind == kind)
        .map(|i| serde_json::from_str(&i.payload).unwrap())
        .collect()
}

fn turn(usage: Usage) -> ChatEvent {
    ChatEvent::TurnResult {
        cause: TurnCause::Operator,
        usage,
        is_error: false,
        stop: "end_turn".into(),
        duration_ms: 1042,
        cache_ttl_seconds: Some(300),
    }
}

fn usage(i: u64, o: u64, r: u64, w: u64, c: u64) -> Usage {
    Usage {
        input_tokens: i,
        output_tokens: o,
        cache_read_tokens: r,
        cache_write_tokens: w,
        cost_micros: c,
    }
}

fn started_turn() -> ChatEvent {
    ChatEvent::TurnStarted {
        cause: TurnCause::Operator,
    }
}

#[test]
fn deltas_give_frames_and_no_item_and_a_text_gives_one_item() {
    let (live, mut frames, _rec) = session();
    let before = items(&live, "S-1").len();
    feed(
        &live,
        vec![
            started_turn(),
            ChatEvent::TextDelta { text: "he".into() },
            ChatEvent::TextDelta { text: "llo".into() },
            ChatEvent::Text {
                text: "hello".into(),
            },
            turn(usage(1, 2, 0, 0, 10)),
        ],
    );
    let got = drain(&mut frames);
    let deltas: Vec<&str> = got
        .iter()
        .filter_map(|f| match f {
            ChatFrame::Delta { session_id, text } => {
                assert_eq!(session_id, "S-1");
                Some(text.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(deltas, ["he", "llo"]);
    // Only a Text and a TurnResult were stored: no item holds a delta.
    let kinds: Vec<String> = kinds(&live, "S-1").split_off(before);
    assert_eq!(kinds, ["assistant_text", "turn_result"]);
    assert_eq!(
        payload(&live, "assistant_text"),
        [serde_json::json!({"kind": "assistant_text", "text": "hello"})]
    );
}

#[test]
fn a_tool_pair_gives_one_tool_call_with_the_capped_texts_byte_for_byte() {
    let (live, mut frames, _rec) = session();
    let input = cap("{\"command\":\"ls\"}");
    let big = "o".repeat(20_000);
    let output = cap(&big);
    assert!(output.truncated && output.bytes == 20_000);
    feed(
        &live,
        vec![
            started_turn(),
            ChatEvent::ToolStarted {
                call_id: "c1".into(),
                tool: "Bash".into(),
                input: input.clone(),
            },
            ChatEvent::ToolFinished {
                call_id: "c1".into(),
                output: output.clone(),
                is_error: false,
            },
            turn(usage(1, 1, 0, 0, 1)),
        ],
    );
    let all = items(&live, "S-1");
    let calls: Vec<_> = all.iter().filter(|i| i.kind == "tool_call").collect();
    assert_eq!(calls.len(), 1, "one tool_call item for the pair");
    let row = calls[0];
    assert_eq!(row.tool_input.as_deref(), Some(input.text.as_str()));
    assert_eq!(row.tool_input_bytes, Some(input.bytes));
    assert!(!row.tool_input_truncated);
    assert_eq!(row.tool_output.as_deref(), Some(output.text.as_str()));
    assert_eq!(row.tool_output_bytes, Some(20_000));
    assert!(row.tool_output_truncated);
    assert!(
        row.tool_output
            .as_deref()
            .unwrap()
            .ends_with("…[truncated: 20000 bytes in all]"),
        "w1's mark"
    );
    let body: Value = serde_json::from_str(&row.payload).unwrap();
    assert_eq!(body["call_id"], "c1");
    assert_eq!(body["tool"], "Bash");
    assert_eq!(body["finished"], true);

    // The `tool_running` frame comes before the item's frame.
    let got = drain(&mut frames);
    let running = got
        .iter()
        .position(|f| matches!(f, ChatFrame::ToolRunning { call_id, tool, .. } if call_id == "c1" && tool == "Bash"))
        .expect("a tool_running frame");
    let item = got
        .iter()
        .position(|f| matches!(f, ChatFrame::Item { item } if item.seq == row.seq))
        .expect("the item frame");
    assert!(running < item);
    // The stream carries the same text.
    let ChatFrame::Item { item } = &got[item] else {
        unreachable!()
    };
    let ChatItemBody::ToolCall {
        input: i,
        output: o,
        ..
    } = &item.body
    else {
        panic!("a tool call: {:?}", item.body)
    };
    assert_eq!(o.as_ref().unwrap().text, output.text);
    assert_eq!(o.as_ref().unwrap().bytes, 20_000);
    assert!(o.as_ref().unwrap().truncated);
    assert_eq!(i.as_ref().unwrap().text, input.text);
}

#[test]
fn a_request_then_its_withdrawal_leaves_needs_you() {
    let (live, _frames, _rec) = session();
    feed(
        &live,
        vec![
            started_turn(),
            ChatEvent::ApprovalRequest {
                request_id: "r1".into(),
                tool: "Bash".into(),
                input: cap("{}"),
                reason: Some("it writes".into()),
            },
        ],
    );
    let store = live.controller.store();
    assert_eq!(store.pending_requests("S-1").unwrap().len(), 1);
    wait_state(&live, "S-1", ChatSessionState::NeedsYou);

    check_order(&[
        started_turn(),
        ChatEvent::ApprovalRequest {
            request_id: "r1".into(),
            tool: "Bash".into(),
            input: cap("{}"),
            reason: None,
        },
        ChatEvent::RequestClosed {
            request_id: "r1".into(),
        },
    ])
    .unwrap();
    live.controller
        .apply_chat_event(
            "S-1",
            ChatEvent::RequestClosed {
                request_id: "r1".into(),
            },
        )
        .unwrap();
    let all = items(&live, "S-1");
    let request = all
        .iter()
        .find(|i| i.kind == "approval_request")
        .expect("the request item");
    let answer = all
        .iter()
        .find(|i| i.kind == "approval_answer")
        .expect("an answer item");
    assert_eq!(answer.answers_seq, Some(request.seq));
    let body: Value = serde_json::from_str(&answer.payload).unwrap();
    assert_eq!(body["outcome"], "withdrawn");
    assert!(store.pending_requests("S-1").unwrap().is_empty());
    let state = store.chat_session("S-1").unwrap().unwrap().state;
    assert_ne!(state, ChatSessionState::NeedsYou, "the card left the set");
    assert_eq!(state, ChatSessionState::Working);
}

#[test]
fn a_session_moved_sets_the_runtime_ref() {
    let (live, _frames, _rec) = session();
    let store = live.controller.store();
    assert_eq!(
        store
            .chat_session("S-1")
            .unwrap()
            .unwrap()
            .runtime_session_ref
            .as_deref(),
        Some("fake-session-1")
    );
    feed(
        &live,
        vec![
            started_turn(),
            ChatEvent::SessionMoved {
                session: SessionRef("moved-2".into()),
            },
            turn(usage(0, 0, 0, 0, 0)),
        ],
    );
    assert_eq!(
        store
            .chat_session("S-1")
            .unwrap()
            .unwrap()
            .runtime_session_ref
            .as_deref(),
        Some("moved-2")
    );
}

#[test]
fn a_turn_result_adds_its_own_usage_once_and_writes_one_line_with_every_field() {
    let (live, _frames, _rec) = session();
    let own = usage(9, 12, 3, 13006, 3465);
    feed(
        &live,
        vec![
            started_turn(),
            ChatEvent::Text { text: "x".into() },
            turn(own),
        ],
    );
    let row = live
        .controller
        .store()
        .chat_session("S-1")
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            row.input_tokens,
            row.output_tokens,
            row.cache_read_tokens,
            row.cache_write_tokens,
            row.cost_micros
        ),
        (9, 12, 3, 13006, 3465),
        "the row holds the turn's usage once"
    );
    let results = payload(&live, "turn_result");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["usage"]["cost_micros"], 3465);
    assert_eq!(results[0]["usage"]["cache_write_tokens"], 13006);
    assert_eq!(results[0]["cause"], "operator");
    assert_eq!(results[0]["stop"], "end_turn");
    assert_eq!(results[0]["duration_ms"], 1042);
    assert_eq!(results[0]["cache_ttl_seconds"], 300);

    let turns: Vec<String> = chat_ledger(&live)
        .into_iter()
        .filter(|l| l.contains(" chat turn "))
        .collect();
    assert_eq!(turns.len(), 1, "{turns:?}");
    for part in [
        "session=S-1",
        "cause=operator",
        "input_tokens=9",
        "output_tokens=12",
        "cache_read_tokens=3",
        "cache_write_tokens=13006",
        "cost_micros=3465",
        "duration_ms=1042",
        "stop=end_turn",
        "is_error=false",
    ] {
        assert!(turns[0].contains(part), "{part} in {}", turns[0]);
    }
    wait_state(&live, "S-1", ChatSessionState::Idle);
}

#[test]
fn an_exit_the_controller_did_not_ask_for_loses_the_requests_and_ends_the_process() {
    let (live, mut frames, _rec) = session();
    feed(
        &live,
        vec![
            started_turn(),
            ChatEvent::ApprovalRequest {
                request_id: "r1".into(),
                tool: "Bash".into(),
                input: cap("{}"),
                reason: None,
            },
            ChatEvent::Question {
                request_id: "r2".into(),
                items: vec![],
            },
        ],
    );
    assert_eq!(live.controller.live_chat_sessions(), ["S-1"]);
    drain(&mut frames);
    live.controller
        .apply_chat_event(
            "S-1",
            ChatEvent::Exited {
                code: Some(137),
                reason: "killed".into(),
            },
        )
        .unwrap();

    let all = items(&live, "S-1");
    let notice = all.iter().find(|i| i.kind == "notice").expect("a notice");
    let body: Value = serde_json::from_str(&notice.payload).unwrap();
    assert_eq!(body["code"], "exited");
    let text = body["text"].as_str().unwrap();
    assert!(text.contains("137") && text.contains("killed"), "{text}");
    let lost: Vec<Value> = all
        .iter()
        .filter(|i| i.kind.ends_with("_answer"))
        .map(|i| serde_json::from_str(&i.payload).unwrap())
        .collect();
    assert_eq!(lost.len(), 2, "one answer per pending request");
    assert!(lost.iter().all(|b| b["outcome"] == "lost"), "{lost:?}");
    assert!(
        live.controller
            .store()
            .pending_requests("S-1")
            .unwrap()
            .is_empty()
    );
    assert!(
        chat_ledger(&live).iter().any(|l| l.contains(" chat stop ")
            && l.contains("session=S-1")
            && l.contains("reason=exited")
            && l.contains("code=137")),
        "{:?}",
        chat_ledger(&live)
    );
    wait_state(&live, "S-1", ChatSessionState::Idle);
    assert!(live.controller.live_chat_sessions().is_empty());
    // A session frame carries the new state, with no process.
    let got = drain(&mut frames);
    let last = got
        .iter()
        .rev()
        .find_map(|f| match f {
            ChatFrame::Session { session } => Some(session.clone()),
            _ => None,
        })
        .expect("a session frame");
    assert!(last.process.is_none());
}

#[test]
fn the_frames_carry_the_seqs_in_order_and_the_ledger_holds_no_text() {
    let (live, mut frames, _rec) = session();
    drain(&mut frames);
    let secret_msg = "SECRET-MESSAGE-TEXT";
    let secret_in = "SECRET-TOOL-INPUT";
    let secret_out = "SECRET-TOOL-OUTPUT";
    let events = vec![
        started_turn(),
        ChatEvent::TextDelta {
            text: secret_msg.into(),
        },
        ChatEvent::Text {
            text: secret_msg.into(),
        },
        ChatEvent::ToolStarted {
            call_id: "c1".into(),
            tool: "Bash".into(),
            input: cap(secret_in),
        },
        ChatEvent::ToolFinished {
            call_id: "c1".into(),
            output: cap(secret_out),
            is_error: false,
        },
        ChatEvent::TaskStarted {
            task: BackgroundTask {
                id: "t1".into(),
                kind: "bash".into(),
                description: secret_in.into(),
                ambient: false,
            },
        },
        ChatEvent::TaskFinished {
            task_id: "t1".into(),
            end: TaskEnd::Completed,
            summary: secret_out.into(),
        },
        ChatEvent::Text {
            text: secret_msg.into(),
        },
        turn(usage(1, 1, 0, 0, 7)),
    ];
    feed(&live, events);
    let seqs: Vec<u64> = drain(&mut frames)
        .into_iter()
        .filter_map(|f| match f {
            ChatFrame::Item { item } => Some(item.seq),
            _ => None,
        })
        .collect();
    let stored: Vec<u64> = items(&live, "S-1").iter().map(|i| i.seq).collect();
    assert_eq!(seqs, stored, "every stored item has one frame, in order");
    assert!(seqs.windows(2).all(|w| w[0] + 1 == w[1]), "{seqs:?}");
    assert!(seqs.len() >= 6, "{seqs:?}");

    let ledger = std::fs::read_to_string(live.sb.state.join("chat.ledger")).unwrap();
    for text in [secret_msg, secret_in, secret_out] {
        assert!(!ledger.contains(text), "chat.ledger holds {text}");
    }
    assert!(ledger.contains(" chat turn "));
}

#[test]
fn an_unfinished_tool_at_exit_is_not_stored_as_finished() {
    // `Capped` is public, so a hand-built event needs no helper.
    let (live, _frames, _rec) = session();
    let input = Capped {
        text: "{}".into(),
        bytes: 2,
        truncated: false,
    };
    let _ = BTreeMap::<String, String>::new();
    feed(
        &live,
        vec![
            started_turn(),
            ChatEvent::ToolStarted {
                call_id: "c9".into(),
                tool: "Bash".into(),
                input,
            },
        ],
    );
    live.controller
        .apply_chat_event(
            "S-1",
            ChatEvent::Exited {
                code: None,
                reason: "gone".into(),
            },
        )
        .unwrap();
    let calls = payload(&live, "tool_call");
    // The spec stores a tool still open at `Exited` with no output and `finished: false`.
    assert_eq!(calls.len(), 1, "{:?}", kinds(&live, "S-1"));
    assert_eq!(calls[0]["finished"], false);
    let all = items(&live, "S-1");
    let row = all.iter().find(|i| i.kind == "tool_call").unwrap();
    assert!(row.tool_output.is_none());
}

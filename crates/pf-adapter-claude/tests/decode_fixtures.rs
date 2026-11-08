//! The decoder over every recording: the events pass the grammar, and the recorded facts come out.
//! These are the facts ADR-0014 lists as verified for CLI 2.1.319. The adapter reads the same
//! recordings through the replay binary in `stream_replay.rs`; here the decoder runs alone.

mod common;

use common::*;
use pf_adapter_claude::decode::{Decoder, Out, decode_recording};
use pf_engine::port::chat::{CAP_BYTES, ChatEvent, InputKind, TaskEnd, TurnCause, check_order};
use serde_json::{Value, json};

fn events(name: &str) -> Vec<ChatEvent> {
    let f = load_fixture(name);
    decode_recording("2.1.319", "", &f.body).0
}

fn names(events: &[ChatEvent]) -> Vec<&'static str> {
    events.iter().map(ChatEvent::name).collect()
}

#[test]
fn every_recording_passes_the_event_grammar() {
    for f in all_fixtures() {
        let (events, _) = decode_recording("2.1.319", "", &f.body);
        check_order(&events).unwrap_or_else(|e| panic!("{}: {e}", f.name));
    }
}

#[test]
fn in_every_recording_the_deltas_of_a_block_join_to_its_text() {
    // A recorded fact, not a rule of `check_order`: a steered or multi-block turn would make a rule
    // fragile. If a future recording breaks it, the contract's statement changes, not the grammar.
    for f in all_fixtures() {
        let (events, _) = decode_recording("2.1.319", "", &f.body);
        let mut joined = String::new();
        for e in &events {
            match e {
                ChatEvent::TextDelta { text } => joined.push_str(text),
                ChatEvent::Text { text } => {
                    assert_eq!(
                        &joined, text,
                        "{}: the deltas before a Text join to it",
                        f.name
                    );
                    joined.clear();
                }
                _ => {}
            }
        }
        assert!(
            joined.is_empty(),
            "{}: deltas left over with no Text",
            f.name
        );
    }
}

#[test]
fn a_plain_turn_gives_deltas_one_text_and_one_result_per_turn() {
    let ev = events("plain_turn");
    assert_eq!(
        names(&ev).iter().filter(|n| **n == "TurnStarted").count(),
        3,
        "three turns in one process"
    );
    assert_eq!(names(&ev).iter().filter(|n| **n == "Text").count(), 3);
    assert!(ev.iter().all(|e| !matches!(e, ChatEvent::Exited { .. })));
}

#[test]
fn per_turn_usage_is_the_difference_of_the_cumulative_totals() {
    let f = load_fixture("plain_turn");
    let totals: Vec<f64> = f
        .body
        .iter()
        .filter(|(to, m)| to == "host" && m["type"] == "result")
        .map(|(_, m)| m["total_cost_usd"].as_f64().unwrap())
        .collect();
    assert_eq!(totals.len(), 3);
    let micros: Vec<u64> = totals.iter().map(|t| (t * 1e6).round() as u64).collect();
    let costs: Vec<u64> = events("plain_turn")
        .iter()
        .filter_map(|e| match e {
            ChatEvent::TurnResult { usage, .. } => Some(usage.cost_micros),
            _ => None,
        })
        .collect();
    assert_eq!(
        costs,
        [micros[0], micros[1] - micros[0], micros[2] - micros[1]]
    );
    assert_eq!(
        costs.iter().sum::<u64>(),
        micros[2],
        "the session total is the sum of its turns"
    );
}

#[test]
fn a_resumed_recording_starts_its_baseline_from_zero() {
    let f = load_fixture("resume");
    let total = f
        .body
        .iter()
        .find(|(to, m)| to == "host" && m["type"] == "result")
        .map(|(_, m)| (m["total_cost_usd"].as_f64().unwrap() * 1e6).round() as u64)
        .unwrap();
    let first = events("resume")
        .into_iter()
        .find_map(|e| match e {
            ChatEvent::TurnResult { usage, .. } => Some(usage.cost_micros),
            _ => None,
        })
        .unwrap();
    assert_eq!(first, total);
}

#[test]
fn the_large_output_recording_has_one_capped_tool_result_of_its_original_size() {
    let capped: Vec<_> = events("big_output")
        .into_iter()
        .filter_map(|e| match e {
            ChatEvent::ToolFinished { output, .. } if output.truncated => Some(output),
            _ => None,
        })
        .collect();
    assert_eq!(capped.len(), 1);
    assert_eq!(capped[0].bytes, 18892);
    assert!(capped[0].text.len() <= CAP_BYTES);
}

#[test]
fn the_mid_turn_recording_is_one_turn_and_the_background_recording_has_a_task_turn() {
    let ev = events("mid_turn");
    assert_eq!(
        names(&ev).iter().filter(|n| **n == "TurnStarted").count(),
        1,
        "steered: one turn"
    );
    assert_eq!(names(&ev).iter().filter(|n| **n == "TurnResult").count(), 1);

    let ev = events("background_task");
    let causes: Vec<TurnCause> = ev
        .iter()
        .filter_map(|e| match e {
            ChatEvent::TurnStarted { cause } => Some(*cause),
            _ => None,
        })
        .collect();
    assert_eq!(causes, [TurnCause::Operator, TurnCause::Task]);
    let finished = ev
        .iter()
        .position(|e| {
            matches!(
                e,
                ChatEvent::TaskFinished {
                    end: TaskEnd::Completed,
                    ..
                }
            )
        })
        .expect("the task finished");
    let task_turn = ev
        .iter()
        .rposition(|e| matches!(e, ChatEvent::TurnStarted { .. }))
        .unwrap();
    assert!(finished < task_turn, "the Task turn follows the finish");
}

#[test]
fn an_interrupted_turn_is_an_error_result_named_by_its_subtype() {
    let stops: Vec<(bool, String)> = events("interrupt")
        .into_iter()
        .filter_map(|e| match e {
            ChatEvent::TurnResult { is_error, stop, .. } => Some((is_error, stop)),
            _ => None,
        })
        .collect();
    assert!(
        stops.contains(&(true, "error_during_execution".to_string())),
        "the recorded interrupt: {stops:?}"
    );
}

#[test]
fn the_permission_recording_gives_approvals_with_a_reason_and_a_question() {
    let ev = events("permissions");
    let approvals = ev
        .iter()
        .filter(|e| matches!(e, ChatEvent::ApprovalRequest { .. }))
        .count();
    let questions = ev
        .iter()
        .filter(|e| matches!(e, ChatEvent::Question { .. }))
        .count();
    assert_eq!((approvals, questions), (2, 1));
    assert!(
        ev.iter().any(|e| matches!(
            e,
            ChatEvent::ApprovalRequest { reason: Some(r), .. } if !r.is_empty()
        )),
        "the request's description stands in for the missing decision reason"
    );
}

#[test]
fn a_recording_with_no_session_events_gives_none_and_a_refused_resume_gives_a_log() {
    assert!(events("models").is_empty(), "an initialize only");
    let f = load_fixture("resume_missing");
    let (ev, logs) = decode_recording("2.1.319", "", &f.body);
    assert!(ev.is_empty());
    assert!(
        logs.iter().any(|l| l.contains("outside a turn")),
        "{logs:?}"
    );
}

// ------------------------------------------------------------------ what no recording holds

fn feed(d: &mut Decoder, msg: Value) -> Vec<Out> {
    d.feed(&msg)
}

fn in_turn_decoder() -> Decoder {
    let mut d = Decoder::new("2.1.319", "s", false);
    d.note_send("u1", InputKind::Operator);
    let out = feed(
        &mut d,
        json!({"type": "system", "subtype": "init", "apiKeySource": "none", "tools": [],
            "claude_code_version": "2.1.319", "session_id": "s", "user_message_uuids": ["u1"]}),
    );
    assert!(matches!(out[0], Out::Event(ChatEvent::TurnStarted { .. })));
    d
}

#[test]
fn a_cancelled_request_closes_an_open_one_and_ignores_an_unknown_one() {
    let mut d = in_turn_decoder();
    let out = feed(
        &mut d,
        json!({"type": "control_request", "request_id": "r1",
            "request": {"subtype": "can_use_tool", "tool_name": "Bash", "input": {"command": "ls"}}}),
    );
    assert!(
        matches!(&out[0], Out::Event(ChatEvent::ApprovalRequest { request_id, .. }) if request_id == "r1")
    );
    let out = feed(
        &mut d,
        json!({"type": "control_cancel_request", "request_id": "r1"}),
    );
    assert_eq!(
        out,
        [Out::Event(ChatEvent::RequestClosed {
            request_id: "r1".into()
        })]
    );
    assert!(
        feed(
            &mut d,
            json!({"type": "control_cancel_request", "request_id": "r1"})
        )
        .is_empty()
    );
    assert!(
        feed(
            &mut d,
            json!({"type": "control_cancel_request", "request_id": "zz"})
        )
        .is_empty()
    );
}

#[test]
fn inbound_requests_the_adapter_does_not_serve_get_an_error_response() {
    let mut d = in_turn_decoder();
    for subtype in [
        "hook_callback",
        "mcp_message",
        "elicitation",
        "request_user_dialog",
        "frobnicate",
    ] {
        let out = feed(
            &mut d,
            json!({"type": "control_request", "request_id": format!("id-{subtype}"), "request": {"subtype": subtype}}),
        );
        assert!(
            out.iter()
                .any(|o| matches!(o, Out::Respond { request_id, error }
                if request_id == &format!("id-{subtype}") && error.contains(subtype))),
            "{subtype}: {out:?}"
        );
        assert!(
            out.iter().all(|o| !matches!(o, Out::Event(_))),
            "{subtype}: no event"
        );
    }
}

#[test]
fn a_cumulative_total_that_goes_down_counts_zero_logs_and_becomes_the_baseline() {
    let result = |cost: f64, input: u64| {
        json!({"type": "result", "subtype": "success", "is_error": false, "total_cost_usd": cost,
            "modelUsage": {"m": {"inputTokens": input, "outputTokens": 0, "cacheReadInputTokens": 0, "cacheCreationInputTokens": 0}}})
    };
    let mut d = in_turn_decoder();
    feed(&mut d, result(0.5, 100));
    d.note_send("u2", InputKind::Operator);
    feed(
        &mut d,
        json!({"type": "system", "subtype": "init", "apiKeySource": "none", "tools": [],
        "claude_code_version": "2.1.319", "session_id": "s", "user_message_uuids": ["u2"]}),
    );
    let out = feed(&mut d, result(0.2, 40));
    assert!(
        out.iter()
            .any(|o| matches!(o, Out::Log(l) if l.contains("went down")))
    );
    let Some(Out::Event(ChatEvent::TurnResult { usage, .. })) =
        out.iter().find(|o| matches!(o, Out::Event(_))).cloned()
    else {
        panic!("{out:?}")
    };
    assert_eq!((usage.cost_micros, usage.input_tokens), (0, 0));
    // The next turn counts from the lower baseline.
    d.note_send("u3", InputKind::Operator);
    feed(
        &mut d,
        json!({"type": "system", "subtype": "init", "apiKeySource": "none", "tools": [],
        "claude_code_version": "2.1.319", "session_id": "s", "user_message_uuids": ["u3"]}),
    );
    let out = feed(&mut d, result(0.3, 90));
    let Some(Out::Event(ChatEvent::TurnResult { usage, .. })) =
        out.iter().find(|o| matches!(o, Out::Event(_))).cloned()
    else {
        panic!("{out:?}")
    };
    assert_eq!((usage.cost_micros, usage.input_tokens), (100_000, 50));
}

#[test]
fn an_unknown_message_kind_gives_no_event_and_no_failure() {
    let mut d = in_turn_decoder();
    for msg in [
        json!({"type": "rate_limit_event"}),
        json!({"type": "something_new", "x": 1}),
        json!({"type": "system", "subtype": "never_heard_of_it"}),
        json!({}),
    ] {
        assert!(feed(&mut d, msg).is_empty());
    }
}

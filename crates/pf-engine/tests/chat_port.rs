//! AC-1: the chat port's data. Every event and its parts round-trip through JSON with a snake-case
//! `type` tag; `cap` keeps short text and cuts long text on a character boundary; `check_order` is
//! the event grammar, and it accepts the good sequences and rejects the bad ones, each with a reason;
//! `ChatError` names its kind.

use std::collections::BTreeMap;

use pf_engine::port::agent::{SessionRef, Usage};
use pf_engine::port::chat::{
    BackgroundTask, CAP_BYTES, Capped, ChatAnswer, ChatError, ChatEvent, ChatOption, ContextUsage,
    InputKind, MidTurn, QuestionChoice, QuestionItem, TaskEnd, TurnCause, cap, check_order,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(
    v: &T,
) -> serde_json::Value {
    let json = serde_json::to_value(v).unwrap();
    let back: T = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(&back, v, "{json}");
    json
}

fn usage() -> Usage {
    Usage {
        input_tokens: 10,
        output_tokens: 20,
        cache_read_tokens: 30,
        cache_write_tokens: 40,
        cost_micros: 500,
    }
}

fn task(id: &str) -> BackgroundTask {
    BackgroundTask {
        id: id.into(),
        kind: "local_bash".into(),
        description: "sleep".into(),
        ambient: false,
    }
}

fn capped(text: &str) -> Capped {
    cap(text)
}

// ------------------------------------------------------------------ events: the sequence builders

fn started(cause: TurnCause) -> ChatEvent {
    ChatEvent::TurnStarted { cause }
}

fn result(cause: TurnCause) -> ChatEvent {
    ChatEvent::TurnResult {
        cause,
        usage: usage(),
        is_error: false,
        stop: "end_turn".into(),
        duration_ms: 12,
        cache_ttl_seconds: Some(3600),
    }
}

fn delta(t: &str) -> ChatEvent {
    ChatEvent::TextDelta { text: t.into() }
}

fn text(t: &str) -> ChatEvent {
    ChatEvent::Text { text: t.into() }
}

fn tool_started(id: &str) -> ChatEvent {
    ChatEvent::ToolStarted {
        call_id: id.into(),
        tool: "Bash".into(),
        input: capped("{}"),
    }
}

fn tool_finished(id: &str) -> ChatEvent {
    ChatEvent::ToolFinished {
        call_id: id.into(),
        output: capped("ok"),
        is_error: false,
    }
}

fn approval(id: &str) -> ChatEvent {
    ChatEvent::ApprovalRequest {
        request_id: id.into(),
        tool: "Bash".into(),
        input: capped("{}"),
        reason: Some("why".into()),
    }
}

fn question(id: &str) -> ChatEvent {
    ChatEvent::Question {
        request_id: id.into(),
        items: vec![QuestionItem {
            text: "Which?".into(),
            header: "Pick".into(),
            options: vec![
                QuestionChoice {
                    label: "A".into(),
                    description: "first".into(),
                },
                QuestionChoice {
                    label: "B".into(),
                    description: "second".into(),
                },
            ],
            multi_select: false,
        }],
    }
}

fn exited() -> ChatEvent {
    ChatEvent::Exited {
        code: Some(0),
        reason: "stdin closed".into(),
    }
}

fn task_started(id: &str) -> ChatEvent {
    ChatEvent::TaskStarted { task: task(id) }
}

fn task_finished(id: &str) -> ChatEvent {
    ChatEvent::TaskFinished {
        task_id: id.into(),
        end: TaskEnd::Completed,
        summary: "done".into(),
    }
}

fn moved() -> ChatEvent {
    ChatEvent::SessionMoved {
        session: SessionRef("s2".into()),
    }
}

fn plain_turn() -> Vec<ChatEvent> {
    vec![
        started(TurnCause::Operator),
        delta("he"),
        delta("llo"),
        text("hello"),
        result(TurnCause::Operator),
    ]
}

// ------------------------------------------------------------------ tests

#[test]
fn every_event_round_trips_with_a_snake_case_type_tag() {
    let events: Vec<(ChatEvent, &str)> = vec![
        (started(TurnCause::Notice), "turn_started"),
        (delta("x"), "text_delta"),
        (text("x"), "text"),
        (tool_started("t1"), "tool_started"),
        (tool_finished("t1"), "tool_finished"),
        (approval("r1"), "approval_request"),
        (question("r2"), "question"),
        (
            ChatEvent::RequestClosed {
                request_id: "r1".into(),
            },
            "request_closed",
        ),
        (task_started("k1"), "task_started"),
        (task_finished("k1"), "task_finished"),
        (result(TurnCause::Task), "turn_result"),
        (moved(), "session_moved"),
        (exited(), "exited"),
    ];
    for (event, tag) in &events {
        let json = round_trip(event);
        assert_eq!(json["type"], *tag, "{json}");
    }
    // Each cause and end is a snake-case string.
    assert_eq!(
        serde_json::to_value(TurnCause::Operator).unwrap(),
        "operator"
    );
    assert_eq!(serde_json::to_value(TurnCause::Notice).unwrap(), "notice");
    assert_eq!(serde_json::to_value(TurnCause::Task).unwrap(), "task");
    assert_eq!(serde_json::to_value(TaskEnd::Stopped).unwrap(), "stopped");
    assert_eq!(serde_json::to_value(InputKind::Notice).unwrap(), "notice");
    assert_eq!(
        serde_json::to_value(MidTurn::NextTurn).unwrap(),
        "next_turn"
    );
}

#[test]
fn answers_options_and_the_small_types_round_trip() {
    let mut answers = BTreeMap::new();
    answers.insert("type".to_string(), "B".to_string());
    answers.insert("Which?".to_string(), "A".to_string());
    for (answer, tag) in [
        (ChatAnswer::Allow, "allow"),
        (
            ChatAnswer::Deny {
                message: "no".into(),
            },
            "deny",
        ),
        (ChatAnswer::Answers(answers), "answers"),
    ] {
        let json = round_trip(&answer);
        assert_eq!(json["type"], tag, "{json}");
    }
    round_trip(&ChatOption {
        id: "model".into(),
        label: "Model".into(),
        values: vec!["a".into(), "b".into()],
        default: "a".into(),
        free: true,
        live: true,
    });
    round_trip(&capped("hello"));
    round_trip(&ContextUsage {
        used_tokens: 28005,
        max_tokens: 1_000_000,
    });
    round_trip(&task("k1"));
}

#[test]
fn cap_keeps_short_text_whole_and_cuts_long_text_to_the_limit_with_a_mark() {
    let short = cap("hello");
    assert_eq!(
        short,
        Capped {
            text: "hello".into(),
            bytes: 5,
            truncated: false
        }
    );
    assert_eq!(cap("").text, "");

    // Text of exactly the limit stays whole, with no mark.
    let edge = "a".repeat(CAP_BYTES);
    let c = cap(&edge);
    assert!(!c.truncated);
    assert_eq!(c.text, edge);
    assert_eq!(c.bytes, CAP_BYTES as u64);

    // One byte over is cut: the content and the mark fit in the limit together.
    let over = "a".repeat(CAP_BYTES + 1);
    let c = cap(&over);
    assert!(c.truncated);
    assert_eq!(c.bytes, (CAP_BYTES + 1) as u64);
    assert!(c.text.len() <= CAP_BYTES, "{}", c.text.len());
    assert!(
        c.text
            .ends_with(&format!("…[truncated: {} bytes in all]", CAP_BYTES + 1))
    );

    // 20000 bytes, the spec's example.
    let big = "x".repeat(20000);
    let c = cap(&big);
    assert!(c.truncated);
    assert_eq!(c.bytes, 20000);
    assert_eq!(c.text.len(), CAP_BYTES, "ASCII fills the limit exactly");
    assert!(c.text.ends_with("…[truncated: 20000 bytes in all]"));
    assert!(c.text.starts_with("xxxx"));
}

#[test]
fn cap_never_splits_a_multi_byte_character() {
    // 3-byte, 2-byte and 4-byte characters, so every cut point lands inside one for some length.
    for unit in ["€", "é", "😀", "aé€😀"] {
        for extra in 0..6 {
            let original = unit.repeat(20000 / unit.len() + extra);
            let c = cap(&original);
            assert!(c.truncated);
            assert_eq!(c.bytes, original.len() as u64);
            assert!(c.text.len() <= CAP_BYTES, "{unit}: {}", c.text.len());
            let mark = format!("…[truncated: {} bytes in all]", original.len());
            let content = c.text.strip_suffix(&mark).expect("the mark ends the text");
            assert!(
                original.starts_with(content),
                "{unit}: the content is a prefix"
            );
            // The next character would not have fit.
            let next = original[content.len()..].chars().next().unwrap();
            assert!(
                content.len() + next.len_utf8() + mark.len() > CAP_BYTES,
                "{unit}: the cut left room for one more character"
            );
        }
    }
}

#[test]
fn check_order_accepts_the_good_sequences() {
    let good: Vec<(&str, Vec<ChatEvent>)> = vec![
        ("a plain turn", plain_turn()),
        (
            "a tool",
            vec![
                started(TurnCause::Operator),
                tool_started("t1"),
                tool_finished("t1"),
                delta("ok"),
                text("ok"),
                result(TurnCause::Operator),
            ],
        ),
        (
            "an approval",
            vec![
                started(TurnCause::Operator),
                approval("r1"),
                tool_started("t1"),
                tool_finished("t1"),
                result(TurnCause::Operator),
            ],
        ),
        (
            "a question",
            vec![
                started(TurnCause::Operator),
                question("r1"),
                text("B"),
                result(TurnCause::Operator),
            ],
        ),
        (
            "a closed request",
            vec![
                started(TurnCause::Operator),
                approval("r1"),
                ChatEvent::RequestClosed {
                    request_id: "r1".into(),
                },
                result(TurnCause::Operator),
            ],
        ),
        (
            "a task finished between turns, then a Task turn",
            vec![
                started(TurnCause::Operator),
                task_started("k1"),
                text("started"),
                result(TurnCause::Operator),
                task_finished("k1"),
                started(TurnCause::Task),
                text("finished"),
                result(TurnCause::Task),
            ],
        ),
        (
            "a task finished inside a turn, then a Task turn",
            vec![
                started(TurnCause::Operator),
                task_started("k1"),
                task_finished("k1"),
                result(TurnCause::Operator),
                started(TurnCause::Task),
                result(TurnCause::Task),
            ],
        ),
        (
            "two tasks finished in a turn, then one Task turn for each",
            vec![
                started(TurnCause::Operator),
                task_started("k1"),
                task_started("k2"),
                task_finished("k1"),
                task_finished("k2"),
                result(TurnCause::Operator),
                started(TurnCause::Task),
                result(TurnCause::Task),
                started(TurnCause::Task),
                result(TurnCause::Task),
            ],
        ),
        (
            "a session move between turns",
            vec![
                started(TurnCause::Operator),
                result(TurnCause::Operator),
                moved(),
                started(TurnCause::Operator),
                result(TurnCause::Operator),
            ],
        ),
        (
            "a session move right after a TurnStarted",
            vec![
                started(TurnCause::Operator),
                moved(),
                text("x"),
                result(TurnCause::Operator),
            ],
        ),
        (
            "an Exited inside an open turn",
            vec![started(TurnCause::Operator), delta("x"), exited()],
        ),
        ("an Exited with no turn", vec![exited()]),
        ("no events", vec![]),
        (
            "two turns",
            plain_turn().into_iter().chain(plain_turn()).collect(),
        ),
    ];
    for (name, events) in good {
        assert_eq!(check_order(&events), Ok(()), "{name}");
    }
}

#[test]
fn check_order_rejects_the_bad_sequences_each_with_a_reason() {
    let bad: Vec<(&str, Vec<ChatEvent>, &str)> = vec![
        ("a delta outside a turn", vec![delta("x")], "outside a turn"),
        (
            "a text outside a turn",
            vec![
                started(TurnCause::Operator),
                result(TurnCause::Operator),
                text("x"),
            ],
            "outside a turn",
        ),
        (
            "a ToolFinished with no ToolStarted",
            vec![started(TurnCause::Operator), tool_finished("t1")],
            "t1",
        ),
        (
            "a ToolFinished in another turn than its ToolStarted",
            vec![
                started(TurnCause::Operator),
                tool_started("t1"),
                result(TurnCause::Operator),
                started(TurnCause::Operator),
                tool_finished("t1"),
            ],
            "t1",
        ),
        (
            "a repeated request id",
            vec![started(TurnCause::Operator), approval("r1"), question("r1")],
            "r1",
        ),
        (
            "a RequestClosed of an unknown request",
            vec![
                started(TurnCause::Operator),
                ChatEvent::RequestClosed {
                    request_id: "r9".into(),
                },
            ],
            "r9",
        ),
        (
            "a RequestClosed twice",
            vec![
                started(TurnCause::Operator),
                approval("r1"),
                ChatEvent::RequestClosed {
                    request_id: "r1".into(),
                },
                ChatEvent::RequestClosed {
                    request_id: "r1".into(),
                },
            ],
            "r1",
        ),
        (
            "a TurnResult whose cause differs",
            vec![started(TurnCause::Operator), result(TurnCause::Notice)],
            "cause",
        ),
        (
            "a TurnResult with no turn",
            vec![result(TurnCause::Operator)],
            "outside a turn",
        ),
        (
            "a TurnStarted inside an open turn",
            vec![started(TurnCause::Operator), started(TurnCause::Operator)],
            "open turn",
        ),
        (
            "a Task turn with no finished task",
            vec![started(TurnCause::Task)],
            "task",
        ),
        (
            "a Task turn after a task that did not finish",
            vec![task_started("k1"), started(TurnCause::Task)],
            "task",
        ),
        (
            "a second Task turn for one finished task",
            vec![
                task_started("k1"),
                task_finished("k1"),
                started(TurnCause::Task),
                result(TurnCause::Task),
                started(TurnCause::Task),
            ],
            "task",
        ),
        (
            "a third Task turn for two finished tasks",
            vec![
                task_started("k1"),
                task_started("k2"),
                task_finished("k1"),
                task_finished("k2"),
                started(TurnCause::Task),
                result(TurnCause::Task),
                started(TurnCause::Task),
                result(TurnCause::Task),
                started(TurnCause::Task),
            ],
            "task",
        ),
        (
            "a TaskFinished of an unknown task",
            vec![task_finished("k9")],
            "k9",
        ),
        (
            "a TaskFinished twice",
            vec![task_started("k1"), task_finished("k1"), task_finished("k1")],
            "k1",
        ),
        (
            "a TaskStarted twice",
            vec![task_started("k1"), task_started("k1")],
            "k1",
        ),
        (
            "a SessionMoved in the middle of a turn",
            vec![started(TurnCause::Operator), text("x"), moved()],
            "session",
        ),
        (
            "an event after Exited",
            vec![exited(), started(TurnCause::Operator)],
            "after Exited",
        ),
        ("two Exited", vec![exited(), exited()], "Exited"),
    ];
    for (name, events, word) in bad {
        let err = check_order(&events).expect_err(name);
        assert!(!err.is_empty(), "{name}: the reason is empty");
        assert!(
            err.contains(word),
            "{name}: the reason {err:?} should name {word:?}"
        );
    }
}

#[test]
fn chat_error_display_names_its_kind() {
    let cases = [
        (
            ChatError::Refused("bad value".into()),
            "refused",
            "bad value",
        ),
        (ChatError::Failed("timed out".into()), "failed", "timed out"),
        (
            ChatError::ResumeFailed("no transcript".into()),
            "resume failed",
            "no transcript",
        ),
        (
            ChatError::RuntimeNotReady("install it".into()),
            "runtime not ready",
            "install it",
        ),
    ];
    for (error, kind, why) in cases {
        let shown = error.to_string();
        assert!(shown.contains(kind), "{shown}");
        assert!(shown.contains(why), "{shown}");
    }
    let _: &dyn std::error::Error = &ChatError::Failed("x".into());
}

// ------------------------------------------------------------------ the recordings

// The adapter's decoder imports only `pf_engine`, `serde_json` and `std`, so this test can include
// the file by path and decode the recordings here. `pf-engine` takes no dependency on the adapter
// crate, and the manifest check of `port_is_neutral.rs` would refuse one.
#[allow(dead_code)]
#[path = "../../pf-adapter-claude/src/decode.rs"]
mod decode;

#[test]
fn check_order_accepts_every_fixtures_event_sequence_decoded_from_the_recordings() {
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../pf-adapter-claude/fixtures");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    files.sort();
    assert!(
        files.len() >= 21,
        "the recordings are there: {}",
        files.len()
    );
    let mut total = 0;
    for path in &files {
        let text = std::fs::read_to_string(path).unwrap();
        let body: Vec<(String, serde_json::Value)> = text
            .lines()
            .skip(1) // the recorder's header
            .map(|l| {
                let v: serde_json::Value = serde_json::from_str(l).unwrap();
                (v["to"].as_str().unwrap().to_string(), v["msg"].clone())
            })
            .collect();
        let (events, _logs) = decode::decode_recording("2.1.319", "", &body);
        check_order(&events).unwrap_or_else(|e| {
            panic!("{}: check_order rejects the recording: {e}", path.display())
        });
        total += events.len();
    }
    assert!(total > 200, "the recordings give events: {total}");
}

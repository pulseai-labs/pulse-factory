//! AC-1: the chat port's data. Every event and its parts round-trip through JSON with a snake-case
//! `type` tag; `cap` keeps short text and cuts long text on a character boundary; `check_order` is
//! the event grammar, and it accepts the good sequences and rejects the bad ones, each with a reason;
//! `ChatError` names its kind.

use std::collections::BTreeMap;

use pf_engine::port::agent::{SessionRef, Usage};
use pf_engine::port::chat::{
    BackgroundTask, CAP_BYTES, Capped, ChatAnswer, ChatDelivery, ChatError, ChatEvent, ChatHost,
    ChatId, ChatOpen, ChatOpened, ChatOption, ChatPort, ChatRebind, ChatResumePoint, ContextUsage,
    InputKind, MidTurn, PendingKind, PendingRequest, QuestionChoice, QuestionItem, RecordedBinary,
    TaskEnd, TurnCause, cap, check_deny_command, check_deny_commands, check_order,
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

fn lost(from: u64, to: u64) -> ChatEvent {
    ChatEvent::OutputLost { from, to }
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
        (lost(0, 10), "output_lost"),
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

// ------------------------------------------------------------------ deny patterns (ADR-0016)

/// The fifteen built-in deny patterns (ruling 9 of `r1.s4.w4`).
const BUILT_IN: [&str; 15] = [
    "git push -f*",
    "git push * -f*",
    "git push --force*",
    "git push * --force*",
    "git push * +*",
    "git push --delete",
    "git push * --delete",
    "git push -d",
    "git push * -d",
    "git branch -D",
    "git branch * -D",
    "gh pr merge",
    "gh -R * pr merge",
    "gh --repo * pr merge",
    "gh --repo=* pr merge",
];

#[test]
fn check_deny_command_takes_the_fifteen_built_ins_and_a_plain_command() {
    for p in BUILT_IN.iter().chain(["npm publish"].iter()) {
        assert_eq!(check_deny_command(p), Ok(()), "{p}");
    }
    let hundred = "a".repeat(100);
    assert_eq!(check_deny_command(&hundred), Ok(()));
}

#[test]
fn check_deny_command_refuses_each_bad_shape_and_names_the_rule() {
    let hundred_and_one = "a".repeat(101);
    let cases: Vec<(&str, String, &str)> = vec![
        ("empty", String::new(), "empty"),
        ("101 characters", hundred_and_one, "101"),
        ("a leading space", " git push".into(), "start"),
        ("a leading dash", "-f".into(), "start"),
        ("a leading star", "* push".into(), "start"),
        ("a trailing space", "git push ".into(), "space"),
        ("two spaces", "git  push".into(), "two spaces"),
        ("a star inside a word", "gi*t push".into(), "inside"),
        ("a double star", "git push **".into(), "**"),
        ("the legacy prefix form", "git push * :*".into(), ":*"),
        ("a colon star elsewhere", "git push a:* b".into(), ":*"),
        ("an open parenthesis", "git (push".into(), "'('"),
        ("a close parenthesis", "git push)".into(), "')'"),
        ("a comma", "git push, force".into(), "','"),
        ("a double quote", "git push \"x\"".into(), "'\"'"),
        ("a single quote", "git push 'x'".into(), "'\\''"),
        ("a semicolon", "git push; ls".into(), "';'"),
        ("an ampersand", "git push && ls".into(), "'&'"),
        ("a pipe", "git push | ls".into(), "'|'"),
        ("a tab", "git\tpush".into(), "U+0009"),
        ("a newline", "git\npush".into(), "U+000A"),
    ];
    for (what, pattern, needle) in cases {
        let why = check_deny_command(&pattern).expect_err(what);
        assert!(why.contains(needle), "{what}: {why}");
    }
}

#[test]
fn check_deny_commands_refuses_33_entries_and_a_duplicate() {
    let ok: Vec<String> = (0..32).map(|i| format!("tool{i}")).collect();
    assert_eq!(check_deny_commands(&ok), Ok(()));
    let mut many = ok.clone();
    many.push("tool32".into());
    assert!(check_deny_commands(&many).unwrap_err().contains("33"));
    let dup = vec![
        "npm publish".to_string(),
        "cargo publish".into(),
        "npm publish".into(),
    ];
    assert!(check_deny_commands(&dup).unwrap_err().contains("twice"));
    let bad = vec!["npm publish".to_string(), "git push * :*".into()];
    assert!(check_deny_commands(&bad).unwrap_err().contains(":*"));
    assert_eq!(check_deny_commands(&[]), Ok(()));
}

fn open_spec() -> ChatOpen {
    ChatOpen {
        cwd: "/w".into(),
        resume: None,
        instructions: String::new(),
        model: "m".into(),
        effort: "high".into(),
        permission_mode: "bypass".into(),
        env: BTreeMap::new(),
        options: BTreeMap::new(),
        deny_commands: BUILT_IN.iter().map(ToString::to_string).collect(),
        chat_session: "S-1".into(),
        project: "P-1".into(),
        agent_id: "orchestrator".into(),
        worktree_id: "W-1".into(),
        deny_from: "built_in".into(),
        secret_env: BTreeMap::new(),
        recorded_binary: None,
    }
}

#[test]
fn a_chat_open_with_deny_commands_keeps_them_in_order() {
    let open = open_spec();
    let copy = open.clone();
    assert_eq!(copy, open);
    assert_eq!(copy.deny_commands.len(), 15);
    assert_eq!(copy.deny_commands[0], "git push -f*");
    assert_eq!(copy.deny_commands[14], "gh --repo=* pr merge");
}

// ------------------------------------------------------------------ the host calls (r2.s2.w5)

#[test]
fn check_order_accepts_output_lost_anywhere_before_exited_and_rejects_it_after() {
    // Between turns, before the first turn, inside a turn, and in a row.
    let mut between = plain_turn();
    between.push(lost(10, 20));
    between.extend(plain_turn());
    check_order(&between).unwrap();
    check_order(&[
        lost(0, 5),
        started(TurnCause::Operator),
        result(TurnCause::Operator),
    ])
    .unwrap();
    let mut inside = vec![
        started(TurnCause::Operator),
        delta("a"),
        lost(3, 9),
        text("a"),
    ];
    inside.push(result(TurnCause::Operator));
    check_order(&inside).unwrap();
    check_order(&[lost(0, 1), lost(1, 2)]).unwrap();
    // A lost stretch does not close a turn, open a request or end a task.
    check_order(&[
        started(TurnCause::Operator),
        approval("r1"),
        lost(1, 2),
        ChatEvent::RequestClosed {
            request_id: "r1".into(),
        },
        result(TurnCause::Operator),
    ])
    .unwrap();
    // An open turn at Exited with a lost stretch inside is allowed, like any open turn.
    check_order(&[started(TurnCause::Operator), lost(1, 2), exited()]).unwrap();
    // After Exited it is rejected, with the rule's reason.
    let err = check_order(&[exited(), lost(1, 2)]).unwrap_err();
    assert!(err.contains("after Exited"), "{err}");
    assert!(err.contains("OutputLost"), "{err}");
}

/// An adapter that implements only the required calls, so the new calls take their defaults.
struct Bare;

impl ChatPort for Bare {
    fn options(&self) -> Vec<ChatOption> {
        Vec::new()
    }
    fn mid_turn(&self) -> MidTurn {
        MidTurn::Steer
    }
    fn open(&self, _: &ChatOpen) -> Result<ChatOpened, pf_engine::port::chat::ChatError> {
        Err(ChatError::Failed("bare".into()))
    }
    fn send(&self, _: &ChatId, _: InputKind, _: &str) -> Result<(), ChatError> {
        Ok(())
    }
    fn answer(&self, _: &ChatId, _: &str, _: &ChatAnswer) -> Result<(), ChatError> {
        Ok(())
    }
    fn interrupt(&self, _: &ChatId) -> Result<(), ChatError> {
        Ok(())
    }
    fn set_option(&self, _: &ChatId, _: &str, _: &str) -> Result<(), ChatError> {
        Ok(())
    }
    fn context_usage(&self, _: &ChatId) -> Result<ContextUsage, ChatError> {
        Err(ChatError::Failed("bare".into()))
    }
    fn background_tasks(&self, _: &ChatId) -> Result<Vec<BackgroundTask>, ChatError> {
        Ok(Vec::new())
    }
    fn is_alive(&self, _: &ChatId) -> Result<bool, ChatError> {
        Ok(false)
    }
    fn stop(&self, _: &ChatId) -> Result<(), ChatError> {
        Ok(())
    }
}

#[test]
fn the_new_calls_default_so_an_adapter_with_no_host_keeps_working() {
    let bare = Bare;
    let chat = ChatId("c1".into());
    // `ack` and `detach` do nothing and succeed.
    assert_eq!(bare.ack(&chat, 42), Ok(()));
    assert_eq!(bare.detach(&chat), Ok(()));
    // `rebind` loses the chat, and says why.
    let point = ChatResumePoint {
        runtime_session: pf_engine::port::agent::SessionRef("s1".into()),
        acked: 0,
        committed: 0,
        checkpoint: None,
        pending: Vec::new(),
    };
    match bare.rebind("pf-host-x-1", &point) {
        ChatRebind::Lost { exit } => {
            let text = exit.to_string();
            assert!(text.contains("no session host"), "{text}");
        }
        other => panic!("the default rebind must be Lost, got {other:?}"),
    }
}

#[test]
fn the_host_types_hold_their_fields_and_a_delivery_carries_the_cursor() {
    // `at: None` is an adapter with no host; a checkpoint comes only with a cursor.
    let plain = ChatDelivery {
        event: text("x"),
        at: None,
        checkpoint: None,
    };
    let hosted = ChatDelivery {
        event: result(TurnCause::Operator),
        at: Some(120),
        checkpoint: Some("{\"totals\":1}".into()),
    };
    assert_ne!(plain, hosted);
    assert_eq!(hosted.clone(), hosted);
    // The resume point names each pending request: id, kind and the stored input when not cut.
    let point = ChatResumePoint {
        runtime_session: pf_engine::port::agent::SessionRef("s1".into()),
        acked: 10,
        committed: 20,
        checkpoint: Some("c".into()),
        pending: vec![
            PendingRequest {
                request_id: "r1".into(),
                kind: PendingKind::Approval,
                input: Some("{}".into()),
            },
            PendingRequest {
                request_id: "r2".into(),
                kind: PendingKind::Question,
                input: None,
            },
        ],
    };
    assert_eq!(point.clone(), point);
    // The open carries the session key, the project, the secrets apart from `env`, and the binary
    // the session was launched with.
    let mut open = open_spec();
    open.secret_env.insert("PF_AGENT_ENV_A".into(), "v".into());
    open.recorded_binary = Some(RecordedBinary {
        path: "/bin/x".into(),
        sha256: "ab".repeat(32),
        version: "2.1.319".into(),
    });
    assert_eq!(open.clone(), open);
    assert_eq!(open.chat_session, "S-1");
    // `ChatOpened` names the host it runs in.
    let (_tx, events) = tokio::sync::mpsc::unbounded_channel();
    let opened = ChatOpened {
        chat: ChatId("c1".into()),
        session: pf_engine::port::agent::SessionRef("s1".into()),
        runtime_version: "v".into(),
        qualified: true,
        events,
        host: Some(ChatHost {
            unit: "pf-host-x-1".into(),
            incarnation: 2,
        }),
    };
    assert_eq!(opened.host.as_ref().unwrap().incarnation, 2);
    // A rebind that cannot reach the host says why.
    match (ChatRebind::Unreachable {
        why: "no hello".into(),
    }) {
        ChatRebind::Unreachable { why } => assert_eq!(why, "no hello"),
        _ => unreachable!(),
    }
}

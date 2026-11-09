//! The display log of a chat on the store port, on the real SQLite store (ADR-0003, ADR-0015):
//! the `seq` of each item, the 16 KB cap on a tool call's input and output, the requests and their
//! answers, the usage of a turn, and the one admitted redaction. An error writes nothing, and no
//! append makes an admission or an outbox line.

mod common;

use common::*;
use pf_engine::port::agent::Usage;
use pf_engine::port::chat::cap;
use pf_engine::port::store::{
    ChatItemKind, ChatItemRow, NewChatItem, ProjectKind, Store, TOOL_TEXT_MAX_BYTES, ToolText,
};
use pf_store_sqlite::SqliteStore;

/// Two projects, each with a live Orchestrator session: `S-1` and `S-2`.
fn two_sessions() -> (tempfile::TempDir, SqliteStore) {
    let (d, store) = open();
    add_project(&store, "c1", "one", "/r/one", ProjectKind::Git, None);
    add_project(&store, "c2", "two", "/r/two", ProjectKind::Folder, None);
    applied(start(&store, "s1", "P-1", record("/r/one")));
    applied(start(&store, "s2", "P-2", record("/r/two")));
    (d, store)
}

fn text(n: usize) -> String {
    "x".repeat(n)
}

fn call(session: &str, input: Option<ToolText>, output: Option<ToolText>) -> NewChatItem {
    let mut i = item(session, ChatItemKind::ToolCall);
    i.payload = "{\"name\":\"Bash\"}".to_string();
    i.tool_input = input;
    i.tool_output = output;
    i
}

fn count(store: &SqliteStore, session: &str) -> usize {
    store.chat_items_after(session, 0, 1000).unwrap().len()
}

/// Run `f`, and check that it failed and wrote nothing: the items, the session row and the
/// admission state are as they were.
fn writes_nothing(
    store: &SqliteStore,
    session: &str,
    f: impl FnOnce() -> Result<ChatItemRow, pf_engine::port::store::StoreError>,
) -> String {
    let items = store.chat_items_after(session, 0, 1000).unwrap();
    let row = store.chat_session(session).unwrap();
    let state = admission_state(store);
    let err = f().expect_err("this append must fail");
    assert_eq!(store.chat_items_after(session, 0, 1000).unwrap(), items);
    assert_eq!(store.chat_session(session).unwrap(), row);
    assert_eq!(admission_state(store), state);
    err.to_string()
}

#[test]
fn items_get_seq_1_2_3_per_session_and_independently() {
    let (_d, store) = two_sessions();
    let a1 = store
        .append_chat_item(item("S-1", ChatItemKind::OperatorMessage))
        .unwrap();
    let b1 = store
        .append_chat_item(item("S-2", ChatItemKind::OperatorMessage))
        .unwrap();
    let a2 = store
        .append_chat_item(item("S-1", ChatItemKind::AssistantText))
        .unwrap();
    let a3 = store
        .append_chat_item(item("S-1", ChatItemKind::Notice))
        .unwrap();
    let b2 = store
        .append_chat_item(item("S-2", ChatItemKind::AssistantText))
        .unwrap();
    assert_eq!((a1.seq, a2.seq, a3.seq), (1, 2, 3));
    assert_eq!((b1.seq, b2.seq), (1, 2));
    assert_eq!(a1.session_id, "S-1");
    assert_eq!(a1.kind, "operator_message");
    assert_eq!(a1.payload, "{}");
    assert_eq!(a1.created_at, "t6");
    assert_eq!((a1.tool_input, a1.tool_output), (None, None));
    assert_eq!((a1.tool_input_bytes, a1.tool_output_bytes), (None, None));
    assert!(!a1.tool_input_truncated && !a1.tool_output_truncated);
    assert_eq!((a1.answers_seq, a1.redacted_at), (None, None));

    let after = store.chat_items_after("S-1", 1, 10).unwrap();
    assert_eq!(after.iter().map(|i| i.seq).collect::<Vec<_>>(), vec![2, 3]);
    assert_eq!(after[0], a2);
    assert_eq!(after[1], a3);
    assert_eq!(
        store.chat_items_after("S-1", 0, 2).unwrap().len(),
        2,
        "the limit holds"
    );
    assert_eq!(store.chat_items_after("S-1", 3, 10).unwrap(), vec![]);
    assert_eq!(store.chat_item("S-1", 2).unwrap(), Some(a2));
    assert_eq!(store.chat_item("S-1", 9).unwrap(), None);
    assert_eq!(store.chat_item("S-9", 1).unwrap(), None);
}

/// The cases of one tool text, for the input and for the output on its own.
fn each_side(
    check: impl Fn(
        &dyn Fn(ToolText) -> NewChatItem,
        &dyn Fn(&ChatItemRow) -> (Option<String>, Option<u64>, bool),
    ),
) {
    let input = |t: ToolText| call("S-1", Some(t), None);
    let output = |t: ToolText| call("S-1", None, Some(t));
    check(&input, &|r| {
        (
            r.tool_input.clone(),
            r.tool_input_bytes,
            r.tool_input_truncated,
        )
    });
    check(&output, &|r| {
        (
            r.tool_output.clone(),
            r.tool_output_bytes,
            r.tool_output_truncated,
        )
    });
}

#[test]
fn uncapped_text_of_at_most_16384_bytes_is_kept_whole_with_no_mark() {
    let (_d, store) = two_sessions();
    each_side(|make, read| {
        for n in [0, 1, 100, TOOL_TEXT_MAX_BYTES] {
            let row = store
                .append_chat_item(make(ToolText::uncapped(text(n))))
                .unwrap();
            assert_eq!(
                read(&row),
                (Some(text(n)), Some(n as u64), false),
                "{n} bytes"
            );
        }
    });
}

#[test]
fn uncapped_text_over_the_cap_is_cut_to_fit_with_its_mark_and_its_true_size() {
    let (_d, store) = two_sessions();
    each_side(|make, read| {
        for n in [TOOL_TEXT_MAX_BYTES + 1, 40 * 1024] {
            let original = format!("{}{}", "a".repeat(n - 1), "z");
            let row = store
                .append_chat_item(make(ToolText::uncapped(original.clone())))
                .unwrap();
            let (stored, bytes, truncated) = read(&row);
            let stored = stored.unwrap();
            assert!(truncated, "{n} bytes");
            assert_eq!(bytes, Some(n as u64), "the original size is kept");
            assert!(
                stored.len() <= TOOL_TEXT_MAX_BYTES,
                "content plus the mark fit the cap: {}",
                stored.len()
            );
            // The store's own cut is the adapter's cut: one function, one mark.
            let mark = format!("…[truncated: {n} bytes in all]");
            assert!(stored.ends_with(&mark), "{}", &stored[stored.len() - 40..]);
            assert_eq!(stored, cap(&original).text, "the store's cut is `cap`");
            let content = &stored[..stored.len() - mark.len()];
            assert!(original.starts_with(content), "the content is a prefix");
            assert_eq!(
                content.len(),
                TOOL_TEXT_MAX_BYTES - mark.len(),
                "an ASCII text is cut to the byte"
            );
        }
    });
}

#[test]
fn a_multibyte_character_across_the_cut_is_dropped_and_not_split() {
    let (_d, store) = two_sessions();
    // The mark names the original size. Both texts below are 16448 or 16449 bytes, five digits, so
    // the content room is the same for both.
    let mark_for = |size: usize| format!("…[truncated: {size} bytes in all]");
    let room = TOOL_TEXT_MAX_BYTES - mark_for(16_449).len();
    each_side(|make, read| {
        // Two-byte characters, so that the cut falls inside one of them.
        let original = format!("{}{}", "a".repeat(room - 1), "é".repeat(50));
        let row = store
            .append_chat_item(make(ToolText::uncapped(original.clone())))
            .unwrap();
        let (stored, bytes, truncated) = read(&row);
        let stored = stored.unwrap();
        assert!(truncated);
        assert_eq!(bytes, Some(original.len() as u64));
        assert!(stored.len() <= TOOL_TEXT_MAX_BYTES);
        let content = &stored[..stored.len() - mark_for(original.len()).len()];
        assert_eq!(
            content,
            "a".repeat(room - 1),
            "the straddling character is gone whole"
        );
        // The cut at a character boundary keeps the character that ends there.
        let original = format!("{}{}", "a".repeat(room - 2), "é".repeat(50));
        let size = original.len();
        let row = store
            .append_chat_item(make(ToolText::uncapped(original)))
            .unwrap();
        let stored = read(&row).0.unwrap();
        let content = &stored[..stored.len() - mark_for(size).len()];
        assert_eq!(content.len(), room);
        assert!(content.ends_with('é'));
    });
}

#[test]
fn text_that_the_adapter_capped_is_stored_byte_for_byte() {
    let (_d, store) = two_sessions();
    each_side(|make, read| {
        // As the adapter makes it: `cap` of a 90000-byte text.
        let capped = cap(&"b".repeat(90_000)).text;
        assert_eq!(capped.len(), TOOL_TEXT_MAX_BYTES);
        let row = store
            .append_chat_item(make(ToolText {
                text: capped.clone(),
                bytes: 90_000,
                truncated: true,
            }))
            .unwrap();
        assert_eq!(
            read(&row),
            (Some(capped), Some(90_000), true),
            "the size and the mark come from the event, and the store cuts nothing again"
        );
        // A shorter capped text, as the adapter makes for a small input.
        let row = store
            .append_chat_item(make(ToolText {
                text: "short…[truncated: 5000 bytes in all]".to_string(),
                bytes: 5_000,
                truncated: true,
            }))
            .unwrap();
        assert_eq!(
            read(&row),
            (
                Some("short…[truncated: 5000 bytes in all]".to_string()),
                Some(5_000),
                true
            )
        );
    });
}

#[test]
fn a_text_over_the_cap_that_claims_to_be_capped_is_an_error() {
    let (_d, store) = two_sessions();
    let msg = writes_nothing(&store, "S-1", || {
        store.append_chat_item(call(
            "S-1",
            Some(ToolText {
                text: text(TOOL_TEXT_MAX_BYTES + 1),
                bytes: 99_999,
                truncated: true,
            }),
            None,
        ))
    });
    assert!(msg.contains("tool input"), "{msg}");
}

#[test]
fn a_long_input_with_a_short_output_marks_only_the_input() {
    let (_d, store) = two_sessions();
    let row = store
        .append_chat_item(call(
            "S-1",
            Some(ToolText::uncapped(text(40 * 1024))),
            Some(ToolText::uncapped("ok")),
        ))
        .unwrap();
    assert!(row.tool_input_truncated);
    assert_eq!(row.tool_input_bytes, Some(40 * 1024));
    assert!(!row.tool_output_truncated);
    assert_eq!(row.tool_output.as_deref(), Some("ok"));
    assert_eq!(row.tool_output_bytes, Some(2));
    // And the other way round.
    let row = store
        .append_chat_item(call(
            "S-1",
            Some(ToolText::uncapped("ls")),
            Some(ToolText::uncapped(text(40 * 1024))),
        ))
        .unwrap();
    assert!(!row.tool_input_truncated);
    assert!(row.tool_output_truncated);
    assert_eq!(row.tool_output_bytes, Some(40 * 1024));
}

#[test]
fn an_answer_with_each_outcome_clears_its_request_and_any_other_outcome_is_refused() {
    let (_d, store) = two_sessions();
    for (n, outcome) in ["answered", "lost", "withdrawn"].into_iter().enumerate() {
        let request = store
            .append_chat_item(item("S-1", ChatItemKind::ApprovalRequest))
            .unwrap();
        assert!(
            store
                .pending_requests("S-1")
                .unwrap()
                .iter()
                .any(|r| r.seq == request.seq),
            "{outcome}: pending before the answer"
        );
        let a = store
            .append_chat_item(answer(
                "S-1",
                ChatItemKind::ApprovalAnswer,
                request.seq,
                outcome,
            ))
            .unwrap();
        assert_eq!(a.answers_seq, Some(request.seq));
        assert_eq!(a.kind, "approval_answer");
        assert_eq!(
            a.payload,
            format!("{{\"outcome\":\"{outcome}\"}}"),
            "case {n}"
        );
        assert!(
            store.pending_requests("S-1").unwrap().is_empty(),
            "{outcome}: cleared"
        );
    }
    // Any other outcome, a missing outcome and a payload that is no object are refused.
    let request = store
        .append_chat_item(item("S-1", ChatItemKind::QuestionRequest))
        .unwrap();
    for payload in [
        "{\"outcome\":\"approved\"}",
        "{\"outcome\":\"\"}",
        "{\"outcome\":1}",
        "{\"outcome\":null}",
        "{}",
        "[]",
        "\"answered\"",
    ] {
        let mut bad = answer("S-1", ChatItemKind::QuestionAnswer, request.seq, "answered");
        bad.payload = payload.to_string();
        let msg = writes_nothing(&store, "S-1", || store.append_chat_item(bad));
        assert!(msg.contains("outcome"), "{payload}: {msg}");
    }
    assert_eq!(store.pending_requests("S-1").unwrap().len(), 1);
}

#[test]
fn a_turn_result_with_usage_adds_to_the_five_totals_and_two_turns_sum() {
    let (_d, store) = two_sessions();
    let turn = |usage: Usage| {
        let mut i = item("S-1", ChatItemKind::TurnResult);
        i.usage = Some(usage);
        i
    };
    store
        .append_chat_item(turn(Usage {
            input_tokens: 10,
            output_tokens: 20,
            cache_read_tokens: 30,
            cache_write_tokens: 40,
            cost_micros: 500,
        }))
        .unwrap();
    let s = store.chat_session("S-1").unwrap().unwrap();
    assert_eq!(
        (
            s.input_tokens,
            s.output_tokens,
            s.cache_read_tokens,
            s.cache_write_tokens,
            s.cost_micros
        ),
        (10, 20, 30, 40, 500)
    );
    store
        .append_chat_item(turn(Usage {
            input_tokens: 1,
            output_tokens: 2,
            cache_read_tokens: 3,
            cache_write_tokens: 4,
            cost_micros: 5,
        }))
        .unwrap();
    let s = store.chat_session("S-1").unwrap().unwrap();
    assert_eq!(
        (
            s.input_tokens,
            s.output_tokens,
            s.cache_read_tokens,
            s.cache_write_tokens,
            s.cost_micros
        ),
        (11, 22, 33, 44, 505)
    );
    // A turn result without usage changes no total, and another session keeps its own.
    store
        .append_chat_item(item("S-1", ChatItemKind::TurnResult))
        .unwrap();
    assert_eq!(store.chat_session("S-1").unwrap().unwrap().cost_micros, 505);
    assert_eq!(store.chat_session("S-2").unwrap().unwrap().cost_micros, 0);
}

#[test]
fn usage_on_another_kind_an_unknown_session_or_a_payload_that_is_not_json_is_an_error() {
    let (_d, store) = two_sessions();
    let usage = Usage {
        input_tokens: 1,
        ..Usage::default()
    };
    let mut on_text = item("S-1", ChatItemKind::AssistantText);
    on_text.usage = Some(usage);
    let msg = writes_nothing(&store, "S-1", || store.append_chat_item(on_text));
    assert!(msg.contains("usage"), "{msg}");

    let before = admission_state(&store);
    let err = store
        .append_chat_item(item("S-9", ChatItemKind::Notice))
        .expect_err("an unknown session");
    assert!(err.to_string().contains("S-9"), "{err}");
    assert_eq!(admission_state(&store), before);

    for payload in ["not json", "{", "", "{\"a\":}"] {
        let mut bad = item("S-1", ChatItemKind::Notice);
        bad.payload = payload.to_string();
        let msg = writes_nothing(&store, "S-1", || store.append_chat_item(bad));
        assert!(msg.contains("JSON"), "{payload}: {msg}");
    }
    assert_eq!(count(&store, "S-1"), 0);
}

#[test]
fn an_item_appends_to_an_ended_session_and_no_append_makes_an_admission_or_a_line() {
    let (_d, store) = two_sessions();
    let before = admission_state(&store);
    store
        .append_chat_item(item("S-1", ChatItemKind::OperatorMessage))
        .unwrap();
    store
        .append_chat_item(item("S-1", ChatItemKind::ToolCall))
        .unwrap();
    assert_eq!(admission_state(&store), before, "an append is no command");

    applied(end(&store, "e1", "S-1"));
    let before = admission_state(&store);
    let late = store
        .append_chat_item(item("S-1", ChatItemKind::TurnResult))
        .unwrap();
    assert_eq!(late.seq, 3, "a turn's last items after an end are kept");
    assert_eq!(admission_state(&store), before);
    assert!(
        store
            .chat_session("S-1")
            .unwrap()
            .unwrap()
            .ended_at
            .is_some()
    );
}

#[test]
fn requests_are_stored_when_asked_and_pair_with_their_answers() {
    let (_d, store) = two_sessions();
    let approval = store
        .append_chat_item(item("S-1", ChatItemKind::ApprovalRequest))
        .unwrap();
    let question = store
        .append_chat_item(item("S-1", ChatItemKind::QuestionRequest))
        .unwrap();
    let pending = store.pending_requests("S-1").unwrap();
    assert_eq!(
        pending,
        vec![approval.clone(), question.clone()],
        "in seq order"
    );
    assert!(store.pending_requests("S-2").unwrap().is_empty());

    let a = store
        .append_chat_item(answer(
            "S-1",
            ChatItemKind::ApprovalAnswer,
            approval.seq,
            "answered",
        ))
        .unwrap();
    assert_eq!(
        store.pending_requests("S-1").unwrap(),
        vec![question.clone()]
    );
    let q = store
        .append_chat_item(answer(
            "S-1",
            ChatItemKind::QuestionAnswer,
            question.seq,
            "answered",
        ))
        .unwrap();
    assert!(store.pending_requests("S-1").unwrap().is_empty());

    // From 0, a reconnect gets each request and its answer, with the answer's `answers_seq`.
    let all = store.chat_items_after("S-1", 0, 100).unwrap();
    assert_eq!(
        all,
        vec![approval.clone(), question.clone(), a.clone(), q.clone()]
    );
    assert_eq!(a.answers_seq, Some(approval.seq));
    assert_eq!(q.answers_seq, Some(question.seq));
    assert!(all[..2].iter().all(|i| i.answers_seq.is_none()));
}

#[test]
fn a_bad_answer_is_an_error_that_writes_nothing() {
    let (_d, store) = two_sessions();
    let approval = store
        .append_chat_item(item("S-1", ChatItemKind::ApprovalRequest))
        .unwrap();
    store
        .append_chat_item(answer(
            "S-1",
            ChatItemKind::ApprovalAnswer,
            approval.seq,
            "answered",
        ))
        .unwrap();
    let question = store
        .append_chat_item(item("S-1", ChatItemKind::QuestionRequest))
        .unwrap();
    let text_item = store
        .append_chat_item(item("S-1", ChatItemKind::AssistantText))
        .unwrap();
    // Session 2 has a request at seq 2 only, so that seq 1 of it is no request.
    store
        .append_chat_item(item("S-2", ChatItemKind::AssistantText))
        .unwrap();
    store
        .append_chat_item(item("S-2", ChatItemKind::ApprovalRequest))
        .unwrap();

    // A second answer to one request.
    let again = answer(
        "S-1",
        ChatItemKind::ApprovalAnswer,
        approval.seq,
        "answered",
    );
    let msg = writes_nothing(&store, "S-1", || store.append_chat_item(again));
    assert!(msg.contains("answered already"), "{msg}");
    // An answer of the wrong kind: a question answer that names an approval, and the reverse.
    let wrong = answer(
        "S-1",
        ChatItemKind::QuestionAnswer,
        approval.seq,
        "answered",
    );
    writes_nothing(&store, "S-1", || store.append_chat_item(wrong));
    let wrong = answer(
        "S-1",
        ChatItemKind::ApprovalAnswer,
        question.seq,
        "answered",
    );
    writes_nothing(&store, "S-1", || store.append_chat_item(wrong));
    // An answer with no `answers_seq`.
    let mut none = answer(
        "S-1",
        ChatItemKind::QuestionAnswer,
        question.seq,
        "answered",
    );
    none.answers_seq = None;
    let msg = writes_nothing(&store, "S-1", || store.append_chat_item(none));
    assert!(msg.contains("names the request"), "{msg}");
    // An `answers_seq` on an assistant text, a request and an operator message.
    for kind in [
        ChatItemKind::AssistantText,
        ChatItemKind::ApprovalRequest,
        ChatItemKind::OperatorMessage,
    ] {
        let mut stray = item("S-1", kind);
        stray.answers_seq = Some(question.seq);
        writes_nothing(&store, "S-1", || store.append_chat_item(stray));
    }
    // An answer that names an item that is no request, one that does not exist yet, and itself.
    let n = store.chat_items_after("S-1", 0, 100).unwrap().len() as u64;
    for target in [text_item.seq, n + 1, n + 7] {
        let bad = answer("S-1", ChatItemKind::QuestionAnswer, target, "answered");
        writes_nothing(&store, "S-1", || store.append_chat_item(bad));
    }
    // An answer that names a request of another session.
    let other = answer(
        "S-2",
        ChatItemKind::ApprovalAnswer,
        approval.seq,
        "answered",
    );
    writes_nothing(&store, "S-2", || store.append_chat_item(other));
    let other = answer("S-1", ChatItemKind::ApprovalAnswer, 2, "answered");
    writes_nothing(&store, "S-1", || store.append_chat_item(other));
    // The open question is still pending, and the next good item gets the next seq.
    assert_eq!(store.pending_requests("S-1").unwrap(), vec![question]);
    let next = store
        .append_chat_item(item("S-1", ChatItemKind::Notice))
        .unwrap();
    assert_eq!(next.seq, n + 1, "no failed append used a seq");
}

#[test]
fn a_redaction_replaces_the_text_and_keeps_everything_else() {
    let (_d, store) = two_sessions();
    let original = store
        .append_chat_item(call(
            "S-1",
            Some(ToolText::uncapped("cat /etc/secret")),
            Some(ToolText::uncapped(text(40 * 1024))),
        ))
        .unwrap();
    assert!(original.tool_output_truncated);
    let before = admission_state(&store);

    let (admission, line) = applied(redact(&store, "red1", "S-1", original.seq));
    assert_eq!(admission.kind, "chat_item_redact");
    assert_eq!(admission.unit_id.as_deref(), Some("S-1"));
    let line = line.expect("one outbox line").body;
    assert_eq!(line, "chat_item_redact S-1 seq=1 admitted command=red1");
    assert!(!line.contains("secret") && !line.contains("xxx"), "{line}");
    assert_eq!(
        admission_state(&store),
        (before.0 + 1, before.1 + 1),
        "one admission, one line"
    );

    let now = store.chat_item("S-1", original.seq).unwrap().unwrap();
    assert_eq!(now.payload, "{\"redacted\":true}");
    assert_eq!(now.tool_input.as_deref(), Some("[redacted]"));
    assert_eq!(now.tool_output.as_deref(), Some("[redacted]"));
    assert_eq!(now.redacted_at.as_deref(), Some("t5"));
    assert_eq!(
        (
            now.kind.as_str(),
            now.seq,
            now.answers_seq,
            now.tool_input_bytes,
            now.tool_input_truncated,
            now.tool_output_bytes,
            now.tool_output_truncated,
            now.created_at.as_str()
        ),
        (
            original.kind.as_str(),
            original.seq,
            original.answers_seq,
            original.tool_input_bytes,
            original.tool_input_truncated,
            original.tool_output_bytes,
            original.tool_output_truncated,
            original.created_at.as_str()
        )
    );

    // A second redaction, and one of an unknown item, are rejected and write nothing.
    let state = admission_state(&store);
    let why = rejected(redact(&store, "red2", "S-1", original.seq));
    assert!(why.contains("redacted already"), "{why}");
    let why = rejected(redact(&store, "red3", "S-1", 77));
    assert!(why.contains("77"), "{why}");
    let why = rejected(redact(&store, "red4", "S-9", 1));
    assert!(why.contains("S-9"), "{why}");
    assert_eq!(admission_state(&store), state);
    assert_eq!(store.chat_item("S-1", original.seq).unwrap().unwrap(), now);
}

#[test]
fn an_item_without_tool_text_redacts_to_the_payload_alone() {
    let (_d, store) = two_sessions();
    let mut plain = item("S-1", ChatItemKind::AssistantText);
    plain.payload = "{\"text\":\"the password is hunter2\"}".to_string();
    store.append_chat_item(plain).unwrap();
    applied(redact(&store, "red1", "S-1", 1));
    let now = store.chat_item("S-1", 1).unwrap().unwrap();
    assert_eq!(now.payload, "{\"redacted\":true}");
    assert_eq!((now.tool_input, now.tool_output), (None, None));
    assert_eq!((now.tool_input_bytes, now.tool_output_bytes), (None, None));
}

#[test]
fn a_redacted_request_stays_a_request_and_still_pairs_with_its_answer() {
    let (_d, store) = two_sessions();
    let request = store
        .append_chat_item(item("S-1", ChatItemKind::ApprovalRequest))
        .unwrap();
    applied(redact(&store, "red1", "S-1", request.seq));
    let redacted = store.chat_item("S-1", request.seq).unwrap().unwrap();
    assert_eq!(redacted.kind, "approval_request");
    assert_eq!(
        store.pending_requests("S-1").unwrap(),
        vec![redacted.clone()],
        "it stays pending until it is answered"
    );
    store
        .append_chat_item(answer(
            "S-1",
            ChatItemKind::ApprovalAnswer,
            request.seq,
            "answered",
        ))
        .unwrap();
    assert!(store.pending_requests("S-1").unwrap().is_empty());
    // The answer is an item like any other, and a redaction works on an ended session too.
    applied(end(&store, "e1", "S-1"));
    applied(redact(&store, "red2", "S-1", 2));
    assert_eq!(
        store.chat_item("S-1", 2).unwrap().unwrap().payload,
        "{\"redacted\":true}"
    );
    assert_eq!(
        store.chat_item("S-1", 2).unwrap().unwrap().answers_seq,
        Some(1)
    );
}

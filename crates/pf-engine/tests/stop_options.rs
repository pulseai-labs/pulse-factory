//! AC-3 of `r2.s11.w1`: a stop file may end with a strict options block, and the stop request
//! keeps the options as data (`docs/contracts/requests.md`, "A stop's options").
//!
//! ```text
//! Options:
//! - <one line of text>
//! - [recommended] <one line of text>
//! ```
//!
//! The line `Options:` stands alone. Two to four lines follow, each starting with `- `, at most one
//! with `[recommended] `. Nothing but blank lines comes after. Each text is redacted first, then
//! holds 1 to 200 characters and no control character. A block that breaks a rule yields no option
//! and a `stop_options_error`; the stop itself stands, with its text as before.

mod common;

use common::*;
use pf_engine::Cause;
use pf_engine::claim::{KEY_STOP_OPTIONS, KEY_STOP_OPTIONS_ERROR, KEY_STOP_TEXT, stop_detail};
use pf_engine::port::store::RequestKind;
use serde_json::{Value, json};

const STEM: &str = "I cannot pick a branch.\nThe plan names two.\n\n";

/// Write `text` as a stop file and read its facts, as the controller does.
fn facts_of(text: &str) -> Value {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stop");
    std::fs::write(&path, text).unwrap();
    stop_detail(&path).expect("the file is readable")
}

fn options_of(facts: &Value) -> Vec<(String, bool)> {
    facts[KEY_STOP_OPTIONS]
        .as_array()
        .unwrap_or_else(|| panic!("no options in {facts}"))
        .iter()
        .map(|o| {
            (
                o["text"].as_str().unwrap().to_string(),
                o["recommended"].as_bool().unwrap(),
            )
        })
        .collect()
}

/// The facts of a file that must give no option and say why.
fn error_of(text: &str) -> String {
    let facts = facts_of(text);
    assert!(
        facts.get(KEY_STOP_OPTIONS).is_none(),
        "a broken block yields no option: {facts}"
    );
    // The stop stands: the whole file is still the stop text.
    assert_eq!(facts[KEY_STOP_TEXT], text, "{facts}");
    facts[KEY_STOP_OPTIONS_ERROR]
        .as_str()
        .unwrap_or_else(|| panic!("no error in {facts}"))
        .to_string()
}

#[test]
fn two_three_and_four_options_parse_in_file_order() {
    for n in 2..=4 {
        let lines: Vec<String> = (1..=n).map(|i| format!("- take branch {i}")).collect();
        let text = format!("{STEM}Options:\n{}\n", lines.join("\n"));
        let facts = facts_of(&text);
        let got = options_of(&facts);
        let want: Vec<(String, bool)> = (1..=n)
            .map(|i| (format!("take branch {i}"), false))
            .collect();
        assert_eq!(got, want, "{n} options");
        assert!(facts.get(KEY_STOP_OPTIONS_ERROR).is_none(), "{facts}");
        assert_eq!(facts[KEY_STOP_TEXT], text, "stop_text holds the whole file");
    }
}

#[test]
fn one_recommended_mark_is_kept_and_no_mark_is_false() {
    let facts = facts_of("Stuck.\nOptions:\n- go left\n- [recommended] go right\n- go back\n");
    assert_eq!(
        options_of(&facts),
        [
            ("go left".to_string(), false),
            ("go right".to_string(), true),
            ("go back".to_string(), false),
        ]
    );
    assert_eq!(
        facts[KEY_STOP_OPTIONS],
        json!([
            { "text": "go left", "recommended": false },
            { "text": "go right", "recommended": true },
            { "text": "go back", "recommended": false },
        ])
    );
}

#[test]
fn blank_lines_after_the_block_are_allowed() {
    let facts = facts_of("Stuck.\nOptions:\n- one\n- two\n\n\n  \n");
    assert_eq!(options_of(&facts).len(), 2);
}

#[test]
fn a_crlf_file_parses_like_an_lf_file() {
    let facts = facts_of("Stuck.\r\nOptions:\r\n- one\r\n- [recommended] two\r\n");
    assert_eq!(
        options_of(&facts),
        [("one".to_string(), false), ("two".to_string(), true)]
    );
}

#[test]
fn a_file_with_no_block_gives_no_options_and_no_error() {
    let facts = facts_of("The plan has no spec.\nNothing else.\n");
    assert!(facts.get(KEY_STOP_OPTIONS).is_none(), "{facts}");
    assert!(facts.get(KEY_STOP_OPTIONS_ERROR).is_none(), "{facts}");
    assert_eq!(
        facts,
        json!({ "stop_text": "The plan has no spec.\nNothing else.\n" })
    );
}

#[test]
fn one_option_is_too_few() {
    let why = error_of("Stuck.\nOptions:\n- only one\n");
    assert!(why.contains("2 to 4") && why.contains('1'), "{why}");
}

#[test]
fn five_options_are_too_many() {
    let why = error_of("Stuck.\nOptions:\n- a\n- b\n- c\n- d\n- e\n");
    assert!(why.contains("2 to 4") && why.contains('5'), "{why}");
}

#[test]
fn two_recommended_marks_are_refused() {
    let why = error_of("Stuck.\nOptions:\n- [recommended] a\n- [recommended] b\n");
    assert!(why.contains("recommended"), "{why}");
}

#[test]
fn a_text_of_201_characters_is_refused_and_one_of_200_passes() {
    let long = "x".repeat(201);
    let why = error_of(&format!("Stuck.\nOptions:\n- {long}\n- fine\n"));
    assert!(why.contains("200"), "{why}");
    // The bound is on characters: 200 two-byte characters pass.
    let edge = "é".repeat(200);
    let facts = facts_of(&format!(
        "Stuck.\nOptions:\n- {edge}\n- [recommended] fine\n"
    ));
    assert_eq!(options_of(&facts)[0].0, edge);
}

#[test]
fn an_empty_option_is_refused() {
    let why = error_of("Stuck.\nOptions:\n- \n- fine\n");
    assert!(why.contains("empty") || why.contains("1 to 200"), "{why}");
    let why = error_of("Stuck.\nOptions:\n- [recommended] \n- fine\n");
    assert!(why.contains("empty") || why.contains("1 to 200"), "{why}");
}

#[test]
fn a_control_character_is_refused() {
    let why = error_of("Stuck.\nOptions:\n- go\u{1b}[31m left\n- fine\n");
    assert!(why.contains("control"), "{why}");
    let why = error_of("Stuck.\nOptions:\n- go\tleft\n- fine\n");
    assert!(why.contains("control"), "{why}");
}

#[test]
fn text_after_the_block_is_refused() {
    let why = error_of("Stuck.\nOptions:\n- a\n- b\n\nThanks.\n");
    assert!(why.contains("after"), "{why}");
    let why = error_of("Stuck.\nOptions:\n- a\n- b\nThanks.\n");
    assert!(why.contains("after") || why.contains("- "), "{why}");
}

#[test]
fn an_options_line_that_is_not_alone_is_refused() {
    let why = error_of("Stuck.\nOptions: a, b or c\n- a\n- b\n");
    assert!(why.contains("alone"), "{why}");
    // Trailing blanks on the line itself do not make it a line with text.
    let facts = facts_of("Stuck.\nOptions:  \n- a\n- b\n");
    assert_eq!(options_of(&facts).len(), 2, "{facts}");
}

#[test]
fn a_line_of_the_block_that_is_not_an_option_is_refused() {
    let why = error_of("Stuck.\nOptions:\n- a\n* b\n");
    assert!(why.contains("- ") || why.contains("after"), "{why}");
}

#[test]
fn a_url_with_a_password_is_stored_redacted() {
    const SECRET: &str = "hunter2-SECRET-9f3a";
    let facts = facts_of(&format!(
        "Stuck.\nOptions:\n- clone https://deploy:{SECRET}@github.com/o/r and retry\n- stop\n"
    ));
    let got = options_of(&facts);
    assert!(!got[0].0.contains(SECRET), "{got:?}");
    assert!(
        got[0]
            .0
            .contains("https://deploy:[redacted]@github.com/o/r"),
        "{got:?}"
    );
    assert!(!facts.to_string().contains(SECRET), "{facts}");
}

#[test]
fn the_secret_is_redacted_before_the_length_is_counted() {
    // A password that makes the raw line longer than 200 characters passes once it is redacted.
    let padding = "p".repeat(190);
    let facts = facts_of(&format!(
        "Stuck.\nOptions:\n- use https://u:{padding}@host.example/r\n- stop\n"
    ));
    assert_eq!(options_of(&facts).len(), 2, "{facts}");
}

#[test]
fn a_block_that_fails_gives_the_requests_block_an_options_error() {
    let env = env(item_workflow(&["implement"], false));
    env.engine
        .ingest(&mut FakeUnitSource {
            records: vec![unit("u1", "g1", 0), unit("u2", "g1", 1)],
        })
        .unwrap();
    let broken = facts_of("Stuck.\nOptions:\n- only one\n");
    let fine = facts_of("Stuck.\nOptions:\n- a\n- [recommended] b\n");
    for (id, facts) in [("u1", &broken), ("u2", &fine)] {
        env.engine.start_stage(id).unwrap();
        env.engine
            .park_with(
                id,
                RequestKind::Stop,
                Cause::StopFile,
                "Stuck.",
                Some(facts),
            )
            .unwrap();
    }
    let requests = env.engine.requests().unwrap();
    let body = |i: usize| -> Value { serde_json::from_str(&requests[i].body).unwrap() };

    // The stop stands, with its text, and the block says why it has no option.
    let first = body(0);
    assert_eq!(first[KEY_STOP_TEXT], "Stuck.\nOptions:\n- only one\n");
    let block = &first["decision"];
    assert_eq!(block["cause"], "stop_file");
    assert!(block["options"].as_array().unwrap().is_empty(), "{block}");
    assert_eq!(block["options_error"], first[KEY_STOP_OPTIONS_ERROR]);
    assert!(block.get("recommended").is_none());

    // A block that parses gives options and no error.
    let second = body(1);
    let block = &second["decision"];
    assert_eq!(block["options"].as_array().unwrap().len(), 2);
    assert!(block.get("options_error").is_none(), "{block}");
    assert_eq!(block["recommended"]["option"], "s2");
    assert_eq!(block["recommended"]["by"], "seat");
}

#[test]
fn a_file_cut_at_the_bound_never_gives_a_shorter_block() {
    // The block sits past the first 64 KiB: the stop text is cut, so the block is refused and is
    // never read as the part of it that fits.
    let filler = "word ".repeat(14_000);
    let facts = facts_of(&format!("{filler}\nOptions:\n- a\n- b\n- c\n"));
    assert!(facts.get(KEY_STOP_OPTIONS).is_none(), "{facts}");
    // The cut text holds no block, so this file gives no error either.
    assert!(facts.get(KEY_STOP_OPTIONS_ERROR).is_none(), "{facts}");

    // A block that starts inside the bound and runs past it is an error, not a short block.
    let filler = "word ".repeat(13_000);
    let tail = "- a long option text\n".repeat(2_000);
    let facts = facts_of(&format!("{filler}\nOptions:\n{tail}"));
    assert!(facts.get(KEY_STOP_OPTIONS).is_none(), "{facts}");
    let why = facts[KEY_STOP_OPTIONS_ERROR].as_str().unwrap();
    assert!(why.contains("cut") || why.contains("4"), "{why}");
}

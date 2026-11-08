//! AC-10: the recordings leak nothing. Every fixture parses as JSON lines and holds no home path,
//! e-mail address, key, account block, operator setup list or artifact URL. The check reads each
//! line **and** each content block with its stream deltas joined, because a secret split across
//! text deltas shows in no single line (the page URL of `artifact_publish` once did).

mod common;

use std::collections::BTreeMap;

use common::*;
use serde_json::Value;

/// Every case of the qualification that was kept.
const CASES: [&str; 21] = [
    "plain_turn",
    "init",
    "env_allowlist",
    "hooks_quiet",
    "fresh_worktree",
    "models",
    "permissions",
    "bypass_dialogs",
    "interrupt",
    "mid_turn",
    "background_task",
    "live_changes",
    "context_usage",
    "big_output",
    "resume_first",
    "resume",
    "resume_missing",
    "stop_group",
    "artifacts_on",
    "artifacts_off",
    "artifact_publish",
];

/// The lists that name the operator's own setup. Each must be empty wherever it is a list.
const SETUP_LISTS: [&str; 6] = [
    "plugins",
    "skills",
    "slash_commands",
    "agents",
    "mcp_servers",
    "commands",
];

fn has_email(text: &str) -> bool {
    let b = text.as_bytes();
    let local = |c: u8| c.is_ascii_alphanumeric() || b"._%+-".contains(&c);
    let domain = |c: u8| c.is_ascii_alphanumeric() || c == b'.' || c == b'-';
    for (i, &c) in b.iter().enumerate() {
        if c != b'@' || i == 0 || !local(b[i - 1]) {
            continue;
        }
        let rest: Vec<u8> = b[i + 1..]
            .iter()
            .copied()
            .take_while(|c| domain(*c))
            .collect();
        if let Some(dot) = rest.iter().rposition(|c| *c == b'.') {
            let tld = &rest[dot + 1..];
            if tld.len() >= 2 && tld.iter().all(u8::is_ascii_alphabetic) && dot > 0 {
                return true;
            }
        }
    }
    false
}

/// Why `text` is not clean, if it is not.
fn leak(text: &str) -> Option<&'static str> {
    if text.contains("/home/") {
        return Some("a /home/ path");
    }
    if text.contains("sk-ant") {
        return Some("an sk-ant key");
    }
    if text.contains("claude.ai/artifact") || text.contains("claude.ai/code/artifact") {
        return Some("an artifact URL");
    }
    if has_email(text) {
        return Some("an e-mail address");
    }
    None
}

/// The text of each content block of the stream, deltas joined, in the order the blocks end.
fn joined_blocks(body: &[(String, Value)]) -> Vec<String> {
    let mut open: BTreeMap<(String, i64), String> = BTreeMap::new();
    let mut done = Vec::new();
    for (to, msg) in body {
        if to != "host" || msg["type"] != "stream_event" {
            continue;
        }
        let parent = msg["parent_tool_use_id"].as_str().unwrap_or("").to_string();
        let event = &msg["event"];
        let key = (parent, event["index"].as_i64().unwrap_or(-1));
        match event["type"].as_str() {
            Some("content_block_start") => {
                open.insert(key, String::new());
            }
            Some("content_block_delta") => {
                let d = &event["delta"];
                let piece = ["text", "partial_json", "thinking"]
                    .iter()
                    .find_map(|k| d[*k].as_str())
                    .unwrap_or("");
                open.entry(key).or_default().push_str(piece);
            }
            Some("content_block_stop") => {
                if let Some(text) = open.remove(&key) {
                    done.push(text);
                }
            }
            _ => {}
        }
    }
    // A block cut off by the end of the recording still counts.
    done.extend(open.into_values());
    done
}

fn walk(v: &Value, f: &mut impl FnMut(&str, &Value)) {
    match v {
        Value::Object(o) => {
            for (k, x) in o {
                f(k, x);
                walk(x, f);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| walk(x, f)),
        _ => {}
    }
}

#[test]
fn every_qualified_case_has_its_fixture_and_nothing_else_is_there() {
    let mut names: Vec<String> = all_fixtures().into_iter().map(|f| f.name).collect();
    names.sort();
    let mut want: Vec<String> = CASES.iter().map(|s| s.to_string()).collect();
    want.sort();
    assert_eq!(names, want);
}

#[test]
fn every_fixture_parses_as_json_lines() {
    for entry in std::fs::read_dir(fixtures_dir()).unwrap().flatten() {
        let path = entry.path();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.ends_with('\n'),
            "{}: ends with a newline",
            path.display()
        );
        for (i, line) in text.lines().enumerate() {
            serde_json::from_str::<Value>(line)
                .unwrap_or_else(|e| panic!("{} line {}: {e}", path.display(), i + 1));
        }
    }
}

#[test]
fn no_line_and_no_joined_block_leaks_a_path_an_address_a_key_or_an_artifact_url() {
    for f in all_fixtures() {
        let text =
            std::fs::read_to_string(fixtures_dir().join(format!("{}.jsonl", f.name))).unwrap();
        for (i, line) in text.lines().enumerate() {
            if let Some(why) = leak(line) {
                panic!("{} line {}: {why}", f.name, i + 1);
            }
        }
        for block in joined_blocks(&f.body) {
            if let Some(why) = leak(&block) {
                panic!(
                    "{}: a content block with its deltas joined holds {why}",
                    f.name
                );
            }
        }
    }
}

#[test]
fn the_joining_check_sees_a_secret_that_no_single_delta_holds() {
    // The blind spot this test guards: the URL is split across three text deltas.
    let delta = |i: i64, text: &str| {
        (
            "host".to_string(),
            serde_json::json!({"type": "stream_event", "parent_tool_use_id": null,
                "event": {"type": "content_block_delta", "index": i,
                          "delta": {"type": "text_delta", "text": text}}}),
        )
    };
    let edge = |kind: &str| {
        (
            "host".to_string(),
            serde_json::json!({"type": "stream_event", "parent_tool_use_id": null,
                "event": {"type": kind, "index": 0}}),
        )
    };
    let body = vec![
        edge("content_block_start"),
        delta(0, "see https://claude.ai/arti"),
        delta(0, "fact/abc"),
        edge("content_block_stop"),
    ];
    for (_, msg) in &body {
        assert_eq!(leak(&msg.to_string()), None, "no single line shows it");
    }
    let blocks = joined_blocks(&body);
    assert_eq!(blocks.len(), 1);
    assert_eq!(leak(&blocks[0]), Some("an artifact URL"));
    assert!(has_email("write to someone@example.com now"));
    assert!(!has_email(
        "the package @anthropic-ai/claude-agent-sdk and a@b"
    ));
}

#[test]
fn no_account_block_no_foreign_api_key_source_and_no_setup_list_is_left() {
    for f in all_fixtures() {
        for (_, msg) in &f.body {
            walk(msg, &mut |key, value| {
                assert_ne!(key, "account", "{}: an account block", f.name);
                if key == "apiKeySource" {
                    assert_eq!(value, "none", "{}: apiKeySource", f.name);
                }
                if SETUP_LISTS.contains(&key)
                    && let Some(list) = value.as_array()
                {
                    assert!(list.is_empty(), "{}: `{key}` is not empty", f.name);
                }
            });
        }
    }
}

#[test]
fn artifact_ids_and_urls_are_only_the_placeholders() {
    for f in all_fixtures() {
        for (_, msg) in &f.body {
            walk(msg, &mut |key, value| {
                if key == "artifact_id" || key == "artifactId" {
                    assert_eq!(value, "<ARTIFACT_ID>", "{}: {key}", f.name);
                }
                if (key == "url" || key == "artifact_url")
                    && let Some(u) = value.as_str()
                {
                    assert!(
                        !u.contains("claude.ai"),
                        "{}: a claude.ai URL in `{key}`: {u}",
                        f.name
                    );
                }
            });
        }
    }
}

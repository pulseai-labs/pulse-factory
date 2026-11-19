//! AC-8 of `r2.s2.w5`: a chat whose host is gone resumes at its next `send` from the newest revision
//! of its launch record (tier 2). A `current` key (`chat.deny_commands`) is read now; a `record` key
//! (`session.replay_bytes`) comes from the record; the CLI copy the chat was launched with must still
//! be in place; a session with no launch record resumes from its row.
//!
//! The real `pulse-factory controller` runs `ClaudeChat` over `pf-claude-replay` in a detached host.
//! Build `pf-host` and `pf-claude-replay` first (`cargo build -p pf-host -p pf-adapter-claude
//! --bins`). Nothing starts a model, spends a token or reaches a network.

mod common;

use std::path::Path;

use common::chat_restart::*;
use pf_host::launcher::Launcher;
use pf_host::sha256::sha256_hex;
use serde_json::Value;

const PLAIN: &str = "plain_turn";
const SECRET_ENV: &str = "PF_AGENT_ENV_ORCHESTRATOR";

fn value_after(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

/// A chat with one finished turn whose host was stopped while the controller was down: the
/// controller started again and set it `idle`. The transcript stand-in is in place.
fn lost_chat(env: &[(&str, &str)]) -> ChatRig {
    let mut rig = ChatRig::new(PLAIN, Some("resume"), &[], env);
    rig.send("hello");
    rig.wait_items("the first turn", |i| {
        i.iter().any(|x| x.kind == "turn_result")
    });
    let unit = rig.unit();
    rig.r.kill9();
    rig.r.detached().stop(&unit).unwrap();
    rig.r.start_controller();
    assert_eq!(rig.row().state, "idle");
    assert_eq!(rig.notices("host_lost").len(), 1);
    rig.write_transcript(&rig.row().runtime_ref.expect("the runtime ref"));
    rig
}

fn wait_second_turn(rig: &ChatRig) {
    rig.wait_items("the resumed turn", |i| {
        i.iter().filter(|x| x.kind == "turn_result").count() >= 2
    });
}

fn open_lines(rig: &ChatRig) -> Vec<String> {
    rig.chat_lines("open")
}

#[test]
fn a_lost_chat_resumes_from_the_newest_revision_and_the_open_line_names_the_incarnation() {
    let rig = lost_chat(&[]);
    let runtime_ref = rig.row().runtime_ref.unwrap();
    // A `set` made while no host lived: the session row holds it, and so does a new revision.
    rig.json(&[
        "session",
        "set",
        &rig.session.clone(),
        "--model",
        "claude-sonnet-5-5",
    ]);
    let records = rig.records();
    assert_eq!(records.len(), 2, "revision 2 of incarnation 1");
    assert_eq!(records[1].2["settings"]["model"], "claude-sonnet-5-5");

    rig.send("again");
    wait_second_turn(&rig);
    assert_eq!(rig.launches(), 2);
    let argv = rig.argv(1);
    assert_eq!(
        value_after(&argv, "--resume").as_deref(),
        Some(runtime_ref.as_str()),
        "{argv:?}"
    );
    assert_eq!(
        value_after(&argv, "--model").as_deref(),
        Some("claude-sonnet-5-5"),
        "{argv:?}"
    );

    let opens = open_lines(&rig);
    assert_eq!(opens.len(), 2, "{opens:?}");
    assert!(
        opens[0].contains("resume=no") && opens[0].contains("incarnation=1"),
        "{}",
        opens[0]
    );
    assert!(
        opens[1].contains("resume=yes") && opens[1].contains("incarnation=2"),
        "{}",
        opens[1]
    );
    let sha = |l: &str| {
        l.split_whitespace()
            .find_map(|w| w.strip_prefix("binary_sha256="))
            .map(str::to_string)
            .unwrap_or_default()
    };
    let wrapper_sha = sha256_hex(&std::fs::read(&rig.wrapper).unwrap());
    assert_eq!(sha(&opens[0]), wrapper_sha, "{}", opens[0]);
    assert_eq!(sha(&opens[1]), sha(&opens[0]), "the same binary: {opens:?}");
    // The new incarnation's record keeps the model the resume ran with.
    let records = rig.records();
    let last = records.last().unwrap();
    assert_eq!((last.0, last.1), (2, 1));
    assert_eq!(last.2["settings"]["model"], "claude-sonnet-5-5");
    assert_eq!(rig.row().model, "claude-sonnet-5-5");
}

#[test]
fn a_current_key_is_read_now_and_a_record_key_comes_from_the_record() {
    let rig = lost_chat(&[]);
    let first = rig.records()[0].2.clone();
    let replay_bytes = first["settings"]["session.replay_bytes"]["value"]
        .as_u64()
        .unwrap();
    // While no host lives: a project-scope deny list (`current`) and another replay size (`record`).
    rig.json(&[
        "settings",
        "set",
        "chat.deny_commands",
        r#"["terraform apply"]"#,
        "--project",
        &rig.project.clone(),
    ]);
    rig.json(&["settings", "set", "session.replay_bytes", "100000"]);
    rig.send("again");
    wait_second_turn(&rig);
    let argv = rig.argv(1);
    let disallowed = value_after(&argv, "--disallowedTools").expect("a deny list");
    assert!(disallowed.contains("terraform apply"), "{disallowed}");
    let records = rig.records();
    let resumed = &records.last().unwrap().2;
    assert_eq!(
        resumed["settings"]["session.replay_bytes"]["value"].as_u64(),
        Some(replay_bytes),
        "the resumed host keeps the recorded value"
    );
    assert_ne!(replay_bytes, 100_000);
    let deny = &resumed["settings"]["chat.deny_commands"];
    assert_eq!(deny["value"][0], "terraform apply", "{deny}");
    assert_eq!(deny["scope"], format!("project:{}", rig.project));
}

#[test]
fn capturing_the_runtime_id_and_a_moved_session_write_the_row_and_no_revision() {
    let mut rig = ChatRig::new(PLAIN, Some("resume"), &[], &[]);
    assert_eq!(rig.records().len(), 1);
    rig.send("hello");
    rig.wait_items("the first turn", |i| {
        i.iter().any(|x| x.kind == "turn_result")
    });
    assert!(
        rig.row().runtime_ref.is_some(),
        "the runtime ID is on the row"
    );
    assert_eq!(rig.records().len(), 1, "no revision for the runtime ID");
    rig.restart();
    assert_eq!(rig.records().len(), 1, "no revision for a rebind");
    let _ = &mut rig;
}

#[test]
fn a_copy_that_changed_or_is_gone_fails_the_resume_and_starts_nothing() {
    let rig = lost_chat(&[]);
    // The wrapper is replaced after the first launch.
    let mut text = std::fs::read_to_string(&rig.wrapper).unwrap();
    text.push_str("# another copy\n");
    std::fs::write(&rig.wrapper, text).unwrap();
    let why = rig.refused(&["session", "send", &rig.session, "again"]);
    assert!(
        why.contains("the CLI copy this chat was launched with is gone or changed"),
        "{why}"
    );
    assert!(why.contains(&rig.wrapper.display().to_string()), "{why}");
    assert_eq!(rig.launches(), 1, "nothing started");
    assert!(rig.row().ended_at.is_none());
    assert!(
        rig.notices("context_lost").is_empty(),
        "it is not context lost"
    );
    // The copy is gone.
    std::fs::remove_file(&rig.wrapper).unwrap();
    let why = rig.refused(&["session", "send", &rig.session, "again"]);
    assert!(why.contains("gone or changed"), "{why}");
    assert_eq!(rig.launches(), 1);
    // The record still names the copy, so putting it back works: the session resumes.
    assert!(!Path::new(&rig.wrapper).exists());
}

#[test]
fn a_secret_with_no_value_now_fails_the_resume_and_names_it() {
    let mut rig = lost_chat(&[(SECRET_ENV, "EXAMPLE_ROLE=s3cr3t-value")]);
    // The record kept only the secret's name and a stand-in.
    let record = &rig.records()[0].2;
    assert_eq!(record["secret_names"][0], "EXAMPLE_ROLE");
    assert!(
        record["env"]["EXAMPLE_ROLE"]
            .as_str()
            .unwrap()
            .starts_with("sha256:"),
        "{record}"
    );
    assert!(
        !record.to_string().contains("s3cr3t-value"),
        "no secret value in the record"
    );
    // The controller starts again with no value for it.
    rig.r.kill9();
    rig.r.extra_env.retain(|(k, _)| k != SECRET_ENV);
    rig.r.start_controller();
    let why = rig.refused(&["session", "send", &rig.session, "again"]);
    assert!(why.contains("EXAMPLE_ROLE"), "{why}");
    assert_eq!(rig.launches(), 1);
}

#[test]
fn a_session_with_no_launch_record_resumes_from_its_row() {
    let mut rig = ChatRig::new(PLAIN, Some("resume"), &[], &[]);
    rig.send("hello");
    rig.wait_items("the first turn", |i| {
        i.iter().any(|x| x.kind == "turn_result")
    });
    let unit = rig.unit();
    rig.r.kill9();
    rig.r.detached().stop(&unit).unwrap();
    // A session from before launch records: no record and no host row (the test removes both, past
    // the triggers that keep them write-once).
    {
        let db = rusqlite::Connection::open(rig.r.state.join("store.sqlite")).unwrap();
        db.execute_batch(
            "DROP TRIGGER hosts_never_delete; DROP TRIGGER launch_records_never_delete; \
             DELETE FROM hosts; DELETE FROM launch_records; \
             DELETE FROM admissions WHERE command_id LIKE 'host-%';",
        )
        .unwrap();
    }
    // The stopped host's own exit file would collide with the unit name of the next launch.
    let _ = std::fs::remove_file(pf_host::names::exit_path(&rig.r.state, &unit));
    rig.r.start_controller();
    assert_eq!(rig.row().state, "idle");
    assert!(
        rig.notices("host_lost").is_empty(),
        "no host row: settled as before"
    );
    rig.write_transcript(&rig.row().runtime_ref.unwrap());
    rig.send("again");
    wait_second_turn(&rig);
    assert_eq!(rig.launches(), 2);
    assert!(
        rig.argv(1).iter().any(|a| a == "--resume"),
        "{:?}",
        rig.argv(1)
    );
    let opens = open_lines(&rig);
    assert!(opens.last().unwrap().contains("resume=yes"), "{opens:?}");
    // The new launch wrote a record again: incarnation 1, because the table was empty.
    let records = rig.records();
    assert_eq!((records[0].0, records[0].1), (1, 1));
    let _: Value = records[0].2.clone();
}

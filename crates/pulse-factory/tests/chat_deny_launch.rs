//! AC-12 of `r1.s4.w4`: the deny list a chat launches with. `Rec` over `FakeChat` records each
//! `ChatOpen`; the controller resolves `chat.deny_commands` at every open. No test starts `claude`.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::chat::*;
use common::ctl::Live;

const DENY: &str = "chat.deny_commands";

/// The fifteen built-in deny patterns, in order (ruling 9).
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

fn live_with_idle(idle: Duration) -> (Live, Arc<Rec>) {
    let rec = Rec::new(strict_fake());
    let live = start_with(rec.clone(), move |cfg| cfg.chat_idle = idle);
    (live, rec)
}

/// Sweep until the sweep has released the process. The reader thread stores a turn's `turn_result`
/// item first and clears the turn hold after it, so a sweep that runs the moment `wait_turns` sees
/// the item can find the process still held and release nothing. The controller sweeps on every
/// tick, so a test that needs the release sweeps again, within a bound.
fn sweep_until_released(live: &Live, rec: &Rec) {
    let end = Instant::now() + Duration::from_secs(20);
    while rec.stop_count() == 0 {
        live.controller.sweep_chats();
        assert!(
            Instant::now() < end,
            "no idle release within 20 s of sweeping"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn set(live: &Live, value: &str, scope: &[&str]) {
    let mut args = vec!["settings", "set", DENY, value];
    args.extend_from_slice(scope);
    cli_json(live, &args);
}

/// The `open` line of the chat ledger, newest first.
fn last_open_line(live: &Live) -> String {
    chat_ledger(live)
        .into_iter()
        .rev()
        .find(|l| l.contains(" open session="))
        .expect("an open line")
}

fn start_session(live: &Live, project: &str) -> String {
    cli_json(live, &["session", "start", "--project", project])["session"]["id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn end_session(live: &Live, s: &str) {
    cli_json(live, &["session", "end", s]);
}

#[test]
fn the_first_open_carries_the_fifteen_built_ins() {
    let (live, rec) = live_with_idle(Duration::from_secs(1800));
    let (_, _) = started(&live, "alpha");
    let want: Vec<String> = BUILT_IN.iter().map(ToString::to_string).collect();
    assert_eq!(rec.last_open().deny_commands, want);
    let line = last_open_line(&live);
    assert!(line.contains("deny=15 deny_from=built_in"), "{line}");
}

#[test]
fn a_change_applies_at_the_next_launch_and_leaves_the_live_process_alone() {
    let (live, rec) = live_with_idle(Duration::from_secs(1800));
    let (project, s) = started(&live, "alpha");
    set(&live, r#"["npm publish"]"#, &["--project", &project]);
    assert_eq!(rec.open_count(), 1, "the live process was left alone");
    assert_eq!(rec.stop_count(), 0);
    assert_eq!(rec.last_open().deny_commands.len(), 15);

    end_session(&live, &s);
    let s2 = start_session(&live, &project);
    assert_ne!(s, s2);
    assert_eq!(rec.open_count(), 2);
    assert_eq!(rec.last_open().deny_commands, ["npm publish"]);
    let line = last_open_line(&live);
    assert!(line.contains("deny=1 deny_from=project:P-1"), "{line}");
}

#[test]
fn a_project_and_agent_row_overrides_the_project_row() {
    let (live, rec) = live_with_idle(Duration::from_secs(1800));
    let (project, s) = started(&live, "alpha");
    set(&live, r#"["cargo publish"]"#, &["--project", &project]);
    set(
        &live,
        r#"["pip publish"]"#,
        &["--project", &project, "--agent", "orchestrator"],
    );
    end_session(&live, &s);
    start_session(&live, &project);
    assert_eq!(rec.last_open().deny_commands, ["pip publish"]);
    let line = last_open_line(&live);
    assert!(
        line.contains("deny=1 deny_from=project:P-1/agent:orchestrator"),
        "{line}"
    );
}

#[test]
fn the_most_specific_list_wins_whole_and_is_no_union() {
    let (live, rec) = live_with_idle(Duration::from_secs(1800));
    let (project, s) = started(&live, "alpha");
    set(&live, r#"["npm publish"]"#, &["--agent", "orchestrator"]);
    set(&live, r#"["cargo publish"]"#, &["--project", &project]);
    end_session(&live, &s);
    start_session(&live, &project);
    assert_eq!(
        rec.last_open().deny_commands,
        ["cargo publish"],
        "override, not a union"
    );
}

#[test]
fn a_resume_after_an_idle_release_carries_the_list_in_force_then() {
    let (live, rec) = live_with_idle(Duration::ZERO);
    let (project, s) = started(&live, "alpha");
    cli_json(&live, &["session", "send", &s, "hello"]);
    wait_turns(&live, &s, 1);
    sweep_until_released(&live, &rec);
    assert_eq!(rec.stop_count(), 1, "the idle release stopped the process");

    set(&live, r#"["terraform apply"]"#, &["--project", &project]);
    cli_json(&live, &["session", "send", &s, "hello again"]);
    wait_turns(&live, &s, 2);
    assert_eq!(rec.open_count(), 2);
    let spec = rec.last_open();
    assert!(spec.resume.is_some(), "the reopen is a resume");
    assert_eq!(spec.deny_commands, ["terraform apply"]);
    let line = last_open_line(&live);
    assert!(line.contains("deny=1 deny_from=project:P-1"), "{line}");
}

#[test]
fn an_empty_list_at_the_server_scope_opens_with_no_rules() {
    let (live, rec) = live_with_idle(Duration::from_secs(1800));
    let (project, s) = started(&live, "alpha");
    set(&live, "[]", &[]);
    end_session(&live, &s);
    start_session(&live, &project);
    assert!(rec.last_open().deny_commands.is_empty());
    let line = last_open_line(&live);
    assert!(line.contains("deny=0 deny_from=server"), "{line}");
}

#[test]
fn a_hand_written_bad_row_is_skipped_and_the_next_scope_decides() {
    let (live, rec) = live_with_idle(Duration::from_secs(1800));
    let (project, s) = started(&live, "alpha");
    set(&live, r#"["npm publish"]"#, &[]);

    // A row only a hand edit can make: the table takes any JSON, the controller's rule does not.
    {
        let conn = rusqlite::Connection::open(live.sb.state.join("store.sqlite")).unwrap();
        conn.busy_timeout(Duration::from_secs(5)).unwrap();
        conn.execute(
            "INSERT INTO settings (project_id, agent_id, key, value, set_by, set_at) \
             VALUES (?1, '', ?2, ?3, 'cli', '2026-11-10T00:00:00Z')",
            rusqlite::params![project, DENY, r#"["git push * :*"]"#],
        )
        .unwrap();
    }
    let resolved = live
        .controller
        .resolve_setting(DENY, Some(&project), Some("orchestrator"))
        .unwrap();
    assert_eq!(
        resolved.from_text, "server",
        "the bad project row is skipped"
    );
    assert_eq!(resolved.layers.len(), 1, "only the readable row is a layer");

    end_session(&live, &s);
    start_session(&live, &project);
    assert_eq!(rec.last_open().deny_commands, ["npm publish"]);
    let line = last_open_line(&live);
    assert!(line.contains("deny=1 deny_from=server"), "{line}");

    // With no readable row at all, the built-in list applies.
    let conn = rusqlite::Connection::open(live.sb.state.join("store.sqlite")).unwrap();
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    conn.execute(
        "UPDATE settings SET value = ?1 WHERE project_id = ''",
        [r#"{"a":1}"#],
    )
    .unwrap();
    drop(conn);
    let current = last_session(&live);
    end_session(&live, &current);
    start_session(&live, &project);
    assert_eq!(rec.last_open().deny_commands.len(), 15);
    let line = last_open_line(&live);
    assert!(line.contains("deny=15 deny_from=built_in"), "{line}");
}

fn last_session(live: &Live) -> String {
    let v = cli_json(live, &["session", "list"]);
    v["sessions"].as_array().unwrap().last().unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

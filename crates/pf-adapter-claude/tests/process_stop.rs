//! AC-8: `stop` ends the process and its whole group, `Exited` comes once, and an exit by itself
//! carries its code and a redacted stderr tail. Shell stubs stand in for the CLI.

mod common;

use std::time::{Duration, Instant};

use common::*;
use pf_adapter_claude::pinned;
use pf_engine::port::chat::{ChatEvent, ChatEvents, ChatPort};

/// A stub that runs `before`, answers `initialize`, then runs `after`. What a test reads at once
/// after `open` belongs in `before`: `open` returns when the answer is written.
fn stub_with(before: &str, after: &str) -> Managed {
    managed(
        &format!("{} (Claude Code)", pinned::CLI_VERSION),
        &format!("{before}\n{ANSWER_INITIALIZE}{after}"),
    )
}

fn stub(after: &str) -> Managed {
    stub_with("", after)
}

fn read_pid(m: &Managed, name: &str) -> u32 {
    std::fs::read_to_string(m.dir.join(name))
        .unwrap_or_else(|e| panic!("{name}: {e}"))
        .trim()
        .parse()
        .unwrap()
}

/// Everything on the channel now.
fn drain(events: &mut ChatEvents) -> Vec<ChatEvent> {
    std::iter::from_fn(|| events.try_recv().ok()).collect()
}

/// Wait for `Exited`, up to 10 s, and return it with what came before.
fn until_exited(events: &mut ChatEvents) -> Vec<ChatEvent> {
    let end = Instant::now() + Duration::from_secs(10);
    let mut seen = Vec::new();
    while Instant::now() < end {
        seen.extend(drain(events));
        if seen.iter().any(|e| matches!(e, ChatEvent::Exited { .. })) {
            return seen;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("no Exited event in 10 s: {seen:?}");
}

fn exited(events: &[ChatEvent]) -> Vec<(Option<i32>, String)> {
    events
        .iter()
        .filter_map(|e| match e {
            ChatEvent::Exited { code, reason } => Some((*code, reason.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn stop_ends_the_stub_and_a_grandchild_in_its_group_and_sends_exited_once() {
    let m = stub_with(
        "echo $$ > \"$DIR/pid\"\nsleep 300 &\necho $! > \"$DIR/gpid\"",
        "cat > /dev/null",
    );
    let c = chat(&m);
    let work = tempfile::tempdir().unwrap();
    let mut opened = c.open(&spec(work.path())).expect("open");
    let (pid, grandchild) = (read_pid(&m, "pid"), read_pid(&m, "gpid"));
    assert!(c.is_alive(&opened.chat).unwrap(), "alive before stop");
    assert_eq!(c.process_id(&opened.chat), Some(pid), "the stub's pid");
    assert!(
        !is_gone(pid) && !is_gone(grandchild),
        "both run before stop"
    );

    c.stop(&opened.chat).unwrap();
    assert!(wait_gone(pid), "the stub is gone");
    assert!(wait_gone(grandchild), "the grandchild in its group is gone");
    assert!(!c.is_alive(&opened.chat).unwrap(), "not alive after stop");
    let first = drain(&mut opened.events);
    assert_eq!(
        exited(&first),
        [(Some(0), "stopped".to_string())],
        "one Exited, last, with the reason stopped: {first:?}"
    );
    assert!(matches!(first.last(), Some(ChatEvent::Exited { .. })));

    c.stop(&opened.chat).expect("a repeat is safe");
    assert!(
        drain(&mut opened.events).is_empty(),
        "a repeat sends nothing"
    );
    assert_eq!(c.process_id(&opened.chat), None, "no pid after the exit");
}

#[test]
fn a_stub_that_ignores_stdin_close_and_term_is_ended_by_kill() {
    let m = stub_with(
        "echo $$ > \"$DIR/pid\"\ntrap '' TERM HUP",
        "while true; do sleep 1; done",
    );
    let c = chat(&m);
    let work = tempfile::tempdir().unwrap();
    let mut opened = c.open(&spec(work.path())).expect("open");
    let pid = read_pid(&m, "pid");
    let started = Instant::now();
    c.stop(&opened.chat).unwrap();
    assert!(
        started.elapsed() >= Duration::from_secs(5),
        "stop waits for the stdin close before it signals"
    );
    assert!(wait_gone(pid), "KILL ended what TERM did not");
    let events = drain(&mut opened.events);
    let ended = exited(&events);
    assert_eq!(ended.len(), 1, "one Exited: {events:?}");
    assert_eq!(ended[0].0, None, "ended by a signal, so no exit code");
    assert!(!c.is_alive(&opened.chat).unwrap());
}

#[test]
fn a_stub_that_exits_by_itself_gives_exited_with_its_code_and_stderr_tail() {
    let m = stub("echo 'the stub broke: out of luck' >&2\nexit 3");
    let c = chat(&m);
    let work = tempfile::tempdir().unwrap();
    let mut opened = c.open(&spec(work.path())).expect("open");
    let events = until_exited(&mut opened.events);
    let ended = exited(&events);
    assert_eq!(ended.len(), 1, "{events:?}");
    assert_eq!(ended[0].0, Some(3), "its own exit code");
    assert!(
        ended[0].1.contains("the stub broke: out of luck"),
        "the stderr tail is the reason: {}",
        ended[0].1
    );
    assert!(!c.is_alive(&opened.chat).unwrap());
    c.stop(&opened.chat).expect("stop after an exit is fine");
    assert!(
        drain(&mut opened.events).is_empty(),
        "stop adds no second Exited"
    );
}

#[test]
fn a_secret_on_stderr_is_redacted_in_the_reason() {
    let key = format!("sk-{}", "A".repeat(32));
    let m = stub(&format!(
        "echo 'refused: key {key} and Authorization: Bearer abcdefghijklmnop' >&2\nexit 1"
    ));
    let c = chat(&m);
    let work = tempfile::tempdir().unwrap();
    let mut opened = c.open(&spec(work.path())).expect("open");
    let events = until_exited(&mut opened.events);
    let ended = exited(&events);
    assert_eq!(ended.len(), 1);
    let reason = &ended[0].1;
    assert!(reason.contains("refused"), "the text stays: {reason}");
    assert!(!reason.contains(&key), "the key is gone: {reason}");
    assert!(
        !reason.contains("abcdefghijklmnop"),
        "the bearer value is gone: {reason}"
    );
    assert!(reason.chars().count() <= 500, "at most 500 characters");
    assert!(!reason.contains('\n'), "one line");
}

#[test]
fn a_stderr_tail_longer_than_the_limit_is_cut_to_500_characters() {
    let m = stub(
        "i=0\nwhile [ $i -lt 400 ]; do echo \"line $i of noise to fill the tail\" >&2; i=$((i+1)); done\nexit 1",
    );
    let c = chat(&m);
    let work = tempfile::tempdir().unwrap();
    let mut opened = c.open(&spec(work.path())).expect("open");
    let events = until_exited(&mut opened.events);
    let ended = exited(&events);
    assert_eq!(ended.len(), 1);
    assert!(
        ended[0].1.chars().count() <= 500,
        "{} chars",
        ended[0].1.chars().count()
    );
    assert!(
        ended[0].1.contains("noise"),
        "the tail is kept, not blanked"
    );
}

//! Real probe: handing off to a successor session (duty 9).
//!
//! The handoff is a successor launch whose first message names a handoff file that the first
//! session wrote. The adapter starts the successor first and then ends the old session.
//!
//! `#[ignore]`d. It runs only with `PF_REAL_OMP=1` and spends two real sessions.
//! `PF_REAL_OMP=1 cargo test -p pf-adapter-omp --test real_probe_handoff -- --ignored --nocapture`

mod common;

use std::time::Duration;

use common::*;
use pf_adapter_omp::probe::{self, Probe};
use pf_engine::port::agent::AgentPort;

#[test]
#[ignore = "spends two real sessions; runs only with PF_REAL_OMP=1"]
fn a_successor_reads_the_handoff_of_the_first_session() {
    if !probe::enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let a = real_adapter(dir.path());
    let first = probe_plan(
        dir.path(),
        scratch.path(),
        "Write the single word PINEAPPLE into the file handoff.md in the current directory, using the write tool. Then reply with the single word DONE.",
        &[],
    );
    let Probe::Started(s1) = probe::launch(&a, &first, 0).expect("the probe starts") else {
        return;
    };
    let ended = wait_until(Duration::from_secs(120), || {
        a.status(&s1).unwrap().turn_ended
    });
    let written = std::fs::read_to_string(scratch.path().join("handoff.md")).unwrap_or_default();
    report("handoff.first_turn_ended", &ended.to_string());
    report("handoff.handoff_file", written.trim());
    let pid1 = a.pid(&s1).unwrap();

    // The second session counts against the budget too.
    probe::reserve(1).expect("the budget has room");
    let successor = probe_plan(
        dir.path(),
        scratch.path(),
        "Read the file handoff.md in the current directory. Write its content, exactly, into the file answer.md, using the write tool. Then reply with the single word DONE.",
        &[],
    );
    let s2 = a
        .hand_off(&s1, &successor)
        .expect("the handoff starts a successor");
    report(
        "handoff.successor_first_turn",
        &a.status(&s2).unwrap().first_turn_started.to_string(),
    );
    let done = wait_until(Duration::from_secs(120), || {
        a.status(&s2).unwrap().turn_ended
    });
    let answer = std::fs::read_to_string(scratch.path().join("answer.md")).unwrap_or_default();
    report("handoff.successor_turn_ended", &done.to_string());
    report("handoff.answer", answer.trim());
    let gone = wait_until(Duration::from_secs(10), || !alive(pid1));
    report("handoff.old_process_gone", &gone.to_string());
    report(
        "handoff.old_session_alive_per_adapter",
        &format!("{:?}", a.is_alive(&s1)),
    );
    a.stop(&s2).unwrap();
    assert!(ended && done);
    assert_eq!(written.trim(), "PINEAPPLE");
    assert_eq!(answer.trim(), "PINEAPPLE");
    assert!(gone);
}

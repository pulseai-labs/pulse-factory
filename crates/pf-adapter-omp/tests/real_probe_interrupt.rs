//! Real probe: interrupting a running turn (duty 4), delivering after it (duty 3), and the exit
//! code of a failing bash call (the push event's outcome depends on it).
//!
//! `#[ignore]`d. It runs only with `PF_REAL_OMP=1` and spends one real session.
//! `PF_REAL_OMP=1 cargo test -p pf-adapter-omp --test real_probe_interrupt -- --ignored --nocapture`

mod common;

use std::time::{Duration, Instant};

use common::*;
use pf_adapter_omp::probe::{self, Probe};
use pf_engine::port::agent::{AgentPort, SessionState};

#[test]
#[ignore = "spends a real session; runs only with PF_REAL_OMP=1"]
fn interrupt_then_deliver_and_a_failing_command() {
    if !probe::enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let a = real_adapter(dir.path());
    let plan = probe_plan(
        dir.path(),
        scratch.path(),
        "Run the bash command `sleep 120` once and wait for it to finish. Then reply with the single word DONE.",
        &[],
    );
    let Probe::Started(s) = probe::launch(&a, &plan, 0).expect("the probe starts") else {
        return;
    };
    let events = a.event_file(&s).unwrap();
    let text = || std::fs::read_to_string(&events).unwrap_or_default();
    let tool_started = wait_until(Duration::from_secs(90), || {
        text().contains("\"ev\":\"tool_call\"")
    });
    report("interrupt.tool_call_seen", &tool_started.to_string());
    std::thread::sleep(Duration::from_secs(2));
    report(
        "interrupt.state_before",
        &format!("{:?}", a.status(&s).unwrap().state),
    );

    // Escape first; then escalate only if the turn does not end.
    let t0 = Instant::now();
    a.interrupt(&s).unwrap();
    let mut by = "escape";
    let mut ended = wait_until(Duration::from_secs(15), || a.status(&s).unwrap().turn_ended);
    if !ended {
        a.write_terminal(&s, b"\x1b").unwrap();
        ended = wait_until(Duration::from_secs(10), || a.status(&s).unwrap().turn_ended);
        by = "escape twice";
    }
    if !ended {
        a.write_terminal(&s, b"\x03").unwrap();
        ended = wait_until(Duration::from_secs(10), || a.status(&s).unwrap().turn_ended);
        by = "ctrl-c";
    }
    report(
        "interrupt.turn_ended",
        &format!("{ended} by {by} after {:?}", t0.elapsed()),
    );
    report(
        "interrupt.state_after",
        &format!(
            "{:?} alive={}",
            a.status(&s).unwrap().state,
            a.is_alive(&s).unwrap()
        ),
    );
    report(
        "interrupt.events_after",
        &format!(
            "\n{}",
            text()
                .lines()
                .filter(|l| !l.contains("\"ev\":\"raw\""))
                .collect::<Vec<_>>()
                .join("\n")
        ),
    );

    if ended && a.is_alive(&s).unwrap() {
        // Duty 3 after an interrupt, and a command that fails.
        a.deliver(
            &s,
            "Run the bash command `sh -c 'echo oops-stdout; echo oops-stderr >&2; exit 3'` once. Then reply with the single word DONE.",
        )
        .expect("an idle session takes a message");
        let second = wait_until(Duration::from_secs(90), || {
            text().matches("\"ev\":\"agent_end\"").count() >= 2
        });
        report("interrupt.second_turn_ended", &second.to_string());
        let bash: Vec<String> = text()
            .lines()
            .filter(|l| l.contains("\"ev\":\"bash\""))
            .map(str::to_string)
            .collect();
        report("interrupt.bash_events", &bash.join("\n"));
        report(
            "interrupt.state_final",
            &format!("{:?}", a.status(&s).unwrap().state),
        );
        assert!(second, "delivery after an interrupt starts a new turn");
        assert_eq!(a.status(&s).unwrap().state, SessionState::Idle);
    }
    a.stop(&s).unwrap();
    assert!(ended, "the turn ended after an interrupt");
}

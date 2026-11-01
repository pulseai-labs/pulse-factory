//! Real probe: the launch, the event shapes, a bash call, usage and launch dialogs (duty 6).
//!
//! `#[ignore]`d. It runs only with `PF_REAL_OMP=1`, and it spends one real session of the budget
//! (`pf_adapter_omp::probe`). Run it with:
//! `PF_REAL_OMP=1 cargo test -p pf-adapter-omp --test real_probe_launch -- --ignored --nocapture`

mod common;

use std::time::Duration;

use common::*;
use pf_adapter_omp::probe::{self, Probe};
use pf_engine::port::agent::{AgentPort, SessionState};

#[test]
#[ignore = "spends a real session; runs only with PF_REAL_OMP=1"]
fn launch_events_bash_usage_and_dialogs() {
    if !probe::enabled() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let a = real_adapter(dir.path());
    let plan = probe_plan(
        dir.path(),
        scratch.path(),
        "Run the bash command `echo hello-from-bash` once. Then reply with the single word DONE and use no other tool.",
        &[],
    );
    let Probe::Started(s) = probe::launch(&a, &plan, 0).expect("the probe starts") else {
        return;
    };
    let events = a.event_file(&s).unwrap();
    // The screen at the first turn: any launch dialog would show here (duty 6).
    report("launch.screen", &a.screen_tail(&s, 450).unwrap());
    let ended = wait_until(Duration::from_secs(120), || {
        a.status(&s).unwrap().turn_ended
    });
    report("launch.turn_ended", &ended.to_string());
    report(
        "launch.state",
        &format!("{:?}", a.status(&s).unwrap().state),
    );
    report("launch.usage", &format!("{:?}", a.usage(&s).unwrap()));
    report(
        "launch.events",
        &format!("\n{}", std::fs::read_to_string(&events).unwrap()),
    );
    report(
        "launch.drain",
        &format!("{:?}", a.drain_events(&s).unwrap()),
    );
    assert!(ended, "the first turn ended");
    assert_eq!(a.status(&s).unwrap().state, SessionState::Idle);
    a.stop(&s).unwrap();
}

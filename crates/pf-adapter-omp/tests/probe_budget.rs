//! AC-6: the budget of real sessions. This target is not `#[ignore]`d. It sets `HOME` to a temp
//! directory, so it never touches the real counter, and it uses a stub, so it spends nothing.
//!
//! One test function holds every case: the cases change process environment variables, and a
//! second test thread would race with them.

mod common;

use common::*;
use pf_adapter_omp::probe::{self, LIMIT, Probe};

fn set(key: &str, value: &str) {
    // SAFETY: this target has one test thread that touches the environment.
    unsafe { std::env::set_var(key, value) }
}

fn unset(key: &str) {
    // SAFETY: as above.
    unsafe { std::env::remove_var(key) }
}

#[test]
fn the_budget_counts_sessions_and_stops_a_launch_before_it_starts() {
    let home = tempfile::tempdir().unwrap();
    set("HOME", home.path().to_str().unwrap());
    let counter = home
        .path()
        .join(".local/state/pulse-factory/probe-sessions");
    assert_eq!(probe::counter_path(), counter);
    assert_eq!(LIMIT, 20);

    let work = tempfile::tempdir().unwrap();
    let omp = stub_omp(work.path(), "18.11.3", STUB_STARTS_A_TURN);
    let a = adapter(work.path(), omp);
    let p = plan(work.path(), &[]);
    let ran = work.path().join("argv");

    // 1. With no PF_REAL_OMP a probe launches nothing and counts nothing.
    unset("PF_REAL_OMP");
    assert!(matches!(probe::launch(&a, &p, 0), Ok(Probe::Skipped)));
    assert!(!ran.exists(), "the stub did not run");
    assert!(!counter.exists(), "nothing was counted");
    set("PF_REAL_OMP", "0");
    assert!(matches!(probe::launch(&a, &p, 0), Ok(Probe::Skipped)));
    assert!(!ran.exists());

    // 2. A probe's launch adds one to the counter.
    set("PF_REAL_OMP", "1");
    let Ok(Probe::Started(_)) = probe::launch(&a, &p, 0) else {
        panic!("the probe should start");
    };
    assert!(ran.exists(), "the stub ran");
    assert_eq!(probe::used().unwrap(), 1);
    assert_eq!(std::fs::read_to_string(&counter).unwrap().trim(), "1");

    // 3. A probe that could start subagents reserves their count first.
    let Ok(Probe::Started(_)) = probe::launch(&a, &p, 3) else {
        panic!("the probe should start");
    };
    assert_eq!(probe::used().unwrap(), 5, "1 + 1 + 3 subagents");

    // 4. A launch that would pass the limit fails before it starts, and changes nothing.
    std::fs::write(&counter, "17\n").unwrap();
    std::fs::remove_file(&ran).unwrap();
    let err = probe::launch(&a, &p, 3)
        .err()
        .expect("17 + 1 + 3 passes 20");
    assert!(err.contains("17 of 20") && err.contains("limit"), "{err}");
    assert!(!ran.exists(), "the stub did not run");
    assert_eq!(probe::used().unwrap(), 17, "the count did not change");

    // 5. With the counter at 20 a probe fails before it starts.
    std::fs::write(&counter, "20\n").unwrap();
    let err = probe::launch(&a, &p, 0).err().expect("20 + 1 passes 20");
    assert!(err.contains("20 of 20"), "{err}");
    assert!(!ran.exists());
    assert_eq!(probe::used().unwrap(), 20);
    assert!(probe::reserve(1).is_err());

    // 6. A counter file that holds no number is an error. It never resets by accident.
    std::fs::write(&counter, "lots\n").unwrap();
    assert!(probe::launch(&a, &p, 0).is_err());
    assert!(!ran.exists());
    assert_eq!(std::fs::read_to_string(&counter).unwrap(), "lots\n");

    // 7. The lock is released after each call.
    assert!(!counter.with_extension("lock").exists());
    unset("PF_REAL_OMP");
}

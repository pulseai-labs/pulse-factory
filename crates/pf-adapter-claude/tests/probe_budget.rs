//! AC-9: the budget of real processes. This target is not `#[ignore]`d. It sets `HOME` to a temp
//! directory, so it never touches the real counter, and it uses a shell stub, so it spends nothing.
//!
//! One test function holds every case: the cases change process environment variables, and a
//! second test thread would race with them.

use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use pf_adapter_claude::probe::{self, LIMIT, Probe};

fn set(key: &str, value: &str) {
    // SAFETY: this target has one test thread that touches the environment.
    unsafe { std::env::set_var(key, value) }
}

fn unset(key: &str) {
    // SAFETY: as above.
    unsafe { std::env::remove_var(key) }
}

fn stub(dir: &std::path::Path) -> Command {
    let path = dir.join("claude-stub");
    std::fs::write(
        &path,
        format!("#!/bin/sh\necho ran >> '{}/ran'\n", dir.display()),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    Command::new(path)
}

fn started(p: Result<Probe, String>) {
    match p {
        Ok(Probe::Started(mut child)) => {
            child.wait().unwrap();
        }
        Ok(Probe::Skipped) => panic!("the probe should start"),
        Err(e) => panic!("the probe should start: {e}"),
    }
}

#[test]
fn the_budget_counts_processes_and_stops_a_start_before_it_happens() {
    let home = tempfile::tempdir().unwrap();
    set("HOME", home.path().to_str().unwrap());
    let counter = home
        .path()
        .join(".local/state/pulse-factory/claude-probe-processes");
    assert_eq!(probe::counter_path(), counter);
    assert_eq!(LIMIT, 30);

    let work = tempfile::tempdir().unwrap();
    let ran = work.path().join("ran");

    // 1. With no PF_REAL_CLAUDE a probe starts nothing and counts nothing.
    unset("PF_REAL_CLAUDE");
    assert!(!probe::enabled());
    assert!(matches!(
        probe::start(&mut stub(work.path())),
        Ok(Probe::Skipped)
    ));
    assert!(!ran.exists(), "the stub did not run");
    assert!(!counter.exists(), "nothing was counted");
    set("PF_REAL_CLAUDE", "0");
    assert!(matches!(
        probe::start(&mut stub(work.path())),
        Ok(Probe::Skipped)
    ));
    assert!(!ran.exists());

    // 2. With the switch, each process counts one.
    set("PF_REAL_CLAUDE", "1");
    assert!(probe::enabled());
    started(probe::start(&mut stub(work.path())));
    assert!(ran.exists(), "the stub ran");
    assert_eq!(probe::used().unwrap(), 1);
    assert_eq!(std::fs::read_to_string(&counter).unwrap().trim(), "1");
    started(probe::start(&mut stub(work.path())));
    assert_eq!(probe::used().unwrap(), 2);

    // 3. A reserve of several processes at once counts them all.
    assert_eq!(probe::reserve(3).unwrap(), 5);

    // 4. A start that would pass the limit fails before it starts, and changes nothing.
    std::fs::write(&counter, "29\n").unwrap();
    std::fs::remove_file(&ran).unwrap();
    started(probe::start(&mut stub(work.path())));
    assert_eq!(probe::used().unwrap(), 30);
    std::fs::remove_file(&ran).unwrap();
    let err = probe::start(&mut stub(work.path()))
        .err()
        .expect("30 + 1 passes 30");
    assert!(err.contains("30 of 30") && err.contains("limit"), "{err}");
    assert!(!ran.exists(), "the stub did not run");
    assert_eq!(probe::used().unwrap(), 30, "the count did not change");
    assert!(probe::reserve(1).is_err());
    std::fs::write(&counter, "28\n").unwrap();
    let err = probe::reserve(3).expect_err("28 + 3 passes 30");
    assert!(err.contains("28 of 30"), "{err}");
    assert_eq!(probe::used().unwrap(), 28);

    // 5. A counter file that holds no number is an error. It never resets by accident.
    std::fs::write(&counter, "lots\n").unwrap();
    assert!(probe::start(&mut stub(work.path())).is_err());
    assert!(!ran.exists());
    assert!(probe::used().is_err());
    assert_eq!(std::fs::read_to_string(&counter).unwrap(), "lots\n");

    // 6. The lock is released after each call.
    assert!(!counter.with_extension("lock").exists());
    unset("PF_REAL_CLAUDE");
}

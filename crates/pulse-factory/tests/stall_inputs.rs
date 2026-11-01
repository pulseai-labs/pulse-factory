//! AC-5: the inputs of the stall rule. Each input alone keeps a seat `Active`; a file under
//! `.git` or `target` is no input; a stall relaunches once, then parks with the redacted tail.

mod common;

use std::path::Path;
use std::process::Command;

use common::rig::*;
use common::*;
use pf_adapter_fake::Step;
use pf_engine::status::{Status, redact};

fn one_minute() -> Options {
    Options {
        implement_stall: 1,
        ..Options::default()
    }
}

/// A rig with `u1` launched, its first turn seen, and the clock 100 seconds on: with no input the
/// next tick relaunches.
fn quiet_rig() -> Rig {
    let r = Rig::with(&one_minute(), |_| {});
    r.queue_item("u1", 1);
    r.tick();
    r.tick();
    r.advance(100);
    r
}

fn attempts(r: &Rig) -> usize {
    r.stage_node("u1", "implement").len()
}

/// The control: with no input, the tick stalls the seat and starts attempt 2.
#[test]
fn without_an_input_the_seat_stalls() {
    let r = quiet_rig();
    r.tick();
    assert_eq!(attempts(&r), 2);
    let first = r.stage_node("u1", "implement");
    assert_eq!(first[0].1, Status::Failed);
    assert!(
        first[0].2.as_deref().unwrap().starts_with("stalled:"),
        "{first:?}"
    );
}

fn fresh(r: &Rig) -> u64 {
    r.clock() - 5
}

#[test]
fn a_new_file_in_the_worktree_is_an_input() {
    let r = quiet_rig();
    let f = r.spine_dir.join("wt-u1/new.txt");
    r.touch(&f);
    set_mtime(&f, fresh(&r));
    r.tick();
    assert_eq!(attempts(&r), 1);
}

#[test]
fn a_new_file_in_the_units_out_directory_is_an_input() {
    let r = quiet_rig();
    let f = r.out("u1", "progress.txt");
    r.touch(&f);
    set_mtime(&f, fresh(&r));
    r.tick();
    assert_eq!(attempts(&r), 1);
}

#[test]
fn a_new_file_in_the_report_directory_is_an_input() {
    let r = quiet_rig();
    let f = r.spine_dir.join("work-u1/notes.md");
    r.touch(&f);
    set_mtime(&f, fresh(&r));
    r.tick();
    assert_eq!(attempts(&r), 1);
}

#[test]
fn a_new_commit_is_an_input() {
    let r = quiet_rig();
    let wt = r.spine_dir.join("wt-u1");
    let when = format!("@{} +0000", fresh(&r));
    let out = Command::new("git")
        .args([
            "-C",
            wt.to_str().unwrap(),
            "-c",
            "user.name=T",
            "-c",
            "user.email=t@example.invalid",
        ])
        .args(["commit", "--allow-empty", "-q", "-m", "work"])
        .env("GIT_AUTHOR_DATE", &when)
        .env("GIT_COMMITTER_DATE", &when)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    r.tick();
    assert_eq!(attempts(&r), 1);
}

#[test]
fn a_new_entry_in_the_deps_directory_is_an_input() {
    let r = quiet_rig();
    let target = r.root.path().join("build-target");
    let deps = target.join("debug/deps");
    std::fs::create_dir_all(&deps).unwrap();
    set_mtime(&deps, r.clock() - 1000);
    // The control: an old deps directory is no input.
    let control = r.fresh_driver(target.to_str().unwrap(), 100_000);
    control.tick();
    r.advance(100);
    control.tick();
    assert_eq!(attempts(&r), 2, "an old deps directory keeps nothing alive");

    // A build writes an entry into deps: the directory's own modified time moves.
    let r = quiet_rig();
    let target = r.root.path().join("build-target");
    let deps = target.join("debug/deps");
    std::fs::create_dir_all(&deps).unwrap();
    std::fs::write(deps.join("libx.rlib"), "x").unwrap();
    set_mtime(&deps, r.clock() - 5);
    let driver = r.fresh_driver(target.to_str().unwrap(), 100_000);
    driver.tick();
    assert_eq!(
        attempts(&r),
        1,
        "the new entry is an input: {:?}",
        driver.notes()
    );
}

#[test]
fn a_turn_is_an_input() {
    let r = Rig::with(&one_minute(), |_| {});
    r.queue_item("u1", 1);
    r.tick();
    // The first turn starts only now, 100 seconds after the launch.
    r.advance(100);
    r.tick();
    assert_eq!(attempts(&r), 1, "the first turn counts as activity");
    r.advance(30);
    r.tick();
    assert_eq!(attempts(&r), 1);
    r.advance(40);
    r.tick();
    assert_eq!(attempts(&r), 2, "no change for more than 60 seconds");
}

#[test]
fn a_file_under_git_or_target_is_no_input() {
    let r = quiet_rig();
    let wt = r.spine_dir.join("wt-u1");
    for rel in [".git/progress", "target/debug/out", "crates/x/target/y"] {
        let f = wt.join(rel);
        r.touch(&f);
        set_mtime(&f, r.clock());
    }
    r.tick();
    assert_eq!(attempts(&r), 2, "those files must not keep the seat alive");
}

#[test]
fn stall_minutes_is_sixty_seconds_on_the_test_clock() {
    let r = Rig::with(&one_minute(), |_| {});
    r.queue_item("u1", 1);
    r.tick();
    r.tick();
    r.advance(60);
    r.tick();
    assert_eq!(attempts(&r), 1, "exactly the limit is still active");
    r.advance(1);
    r.tick();
    assert_eq!(attempts(&r), 2);
}

#[test]
fn the_second_stall_parks_with_the_redacted_tail_and_the_count_comes_from_the_store() {
    let token = "ghp_0123456789abcdefghijklmnopqrstuvwxyz";
    let r = Rig::with(&one_minute(), |_| {});
    r.script(
        "u1",
        vec![Step::Stalled(format!("build hung, token={token}"))],
    );
    r.queue_item("u1", 1);
    r.tick();
    r.tick();
    r.advance(100);
    r.tick();
    let nodes = r.stage_node("u1", "implement");
    assert_eq!(nodes.len(), 2);
    assert_eq!(
        nodes[0].2.as_deref(),
        Some("stalled: no screen text"),
        "no Stalled event was drained"
    );

    // The controller restarts: a new driver reads the count of stalls from the store.
    let driver = r.fresh_driver("", 100_000);
    driver.tick();
    r.fake.advance(&r.session_ref("u1").unwrap(), 1);
    driver.tick();
    r.advance(100);
    driver.tick();
    let (status, reason) = r.unit("u1");
    assert_eq!(status, Status::Parked, "ledger: {}", r.ledger_text());
    let reason = reason.unwrap();
    assert!(reason.starts_with("stalled: "), "{reason}");
    assert!(reason.contains("build hung"), "{reason}");
    assert!(!reason.contains(token), "{reason}");
    assert!(reason.contains(&redact(token)), "{reason}");
    assert_eq!(attempts(&r), 2, "no third attempt");
}

#[test]
fn a_walk_that_reaches_the_cut_off_counts_as_now() {
    let r = quiet_rig();
    let wt = r.spine_dir.join("wt-u1");
    for i in 0..6 {
        let f = wt.join(format!("old-{i}.txt"));
        r.touch(&f);
        set_mtime(&f, r.clock() - 1000);
    }
    // A driver with a small cut-off cannot read the whole tree: it treats that as a change now.
    let driver = r.fresh_driver("", 3);
    driver.tick();
    r.advance(100);
    driver.tick();
    assert_eq!(attempts(&r), 1, "the cut-off counts as now");
    // The real cut-off is 100,000 entries.
    assert_eq!(pulse_factory::controller::driver::WALK_LIMIT, 100_000);
    // With the full walk, the same tree is old and the seat stalls.
    let full = r.fresh_driver("", 100_000);
    full.tick();
    r.advance(100);
    full.tick();
    assert_eq!(attempts(&r), 2);
}

#[allow(dead_code)]
fn unused(_: &Path) {}

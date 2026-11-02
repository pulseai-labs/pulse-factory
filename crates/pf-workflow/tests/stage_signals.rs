//! AC-7: stage signals. One pure function decides, from a stage, a probe and the session's state,
//! whether to keep waiting, call the stage done, or park the unit.

#[path = "../../pf-engine/tests/common/mod.rs"]
mod common;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use common::{Env, TrackedSource, env, item_workflow, unit};
use pf_engine::Status;
use pf_engine::launch::LaunchStep;
use pf_engine::port::forge::PrRef;
use pf_engine::render::{RenderContext, UnitView};
use pf_engine::unit::Signal;
use pf_workflow::commands::{CommandFailure, CommandSpec};
use pf_workflow::file::{DoneCond, ParkOn, PrTest, StageSpec};
use pf_workflow::signals::{
    ApplyResult, Probe, SessionState, StageVerdict, SystemProbe, apply, evaluate, parse_pr,
};

fn stage(dir: &Path, done: Vec<DoneCond>, park_on: Vec<ParkOn>) -> StageSpec {
    let d = dir.display();
    StageSpec {
        name: "build".to_string(),
        role: "build-role".to_string(),
        brief: vec!["briefs/b.md".to_string()],
        message: "go".to_string(),
        cwd: "/work".to_string(),
        slots: vec!["session".to_string()],
        before: Vec::new(),
        after_first_turn: Vec::new(),
        stop_file: format!("{d}/{{unit.id}}/stop"),
        stall_minutes: 10,
        first_turn_seconds: 60,
        skills: Vec::new(),
        settings: BTreeMap::new(),
        done,
        park_on,
    }
}

fn file(dir: &Path, name: &str) -> DoneCond {
    DoneCond::File(format!("{}/{{unit.id}}/{name}", dir.display()))
}

fn write(dir: &Path, unit: &str, name: &str, text: &str) {
    std::fs::create_dir_all(dir.join(unit)).unwrap();
    std::fs::write(dir.join(unit).join(name), text).unwrap();
}

const WAITING: SessionState = SessionState {
    turn_ended: false,
    ended: false,
};
const TURN_ENDED: SessionState = SessionState {
    turn_ended: true,
    ended: false,
};

fn params() -> BTreeMap<String, String> {
    BTreeMap::new()
}

fn verdict(
    stage: &StageSpec,
    vars: &BTreeMap<String, String>,
    probe: &dyn Probe,
    s: &SessionState,
) -> StageVerdict {
    let p = params();
    let ctx = RenderContext {
        run_id: "run-1",
        run_dir: "/run",
        params: &p,
        unit: Some(UnitView { id: "u1", vars }),
    };
    evaluate(stage, &ctx, probe, s)
}

fn v(stage: &StageSpec, probe: &dyn Probe, s: &SessionState) -> StageVerdict {
    verdict(stage, &BTreeMap::new(), probe, s)
}

/// A probe with scripted command answers. Files come from the real file system.
#[derive(Default)]
struct Scripted {
    exit0: RefCell<BTreeMap<String, bool>>,
    stdout: RefCell<BTreeMap<String, Option<String>>>,
    ran: RefCell<Vec<Vec<String>>>,
    missing: BTreeSet<String>,
}

impl Probe for Scripted {
    fn file_exists(&self, path: &str) -> bool {
        SystemProbe.file_exists(path)
    }
    fn read_text(&self, path: &str) -> Result<String, String> {
        SystemProbe.read_text(path)
    }
    fn exit0(&self, argv: &[String], _t: u64) -> Result<bool, CommandFailure> {
        self.ran.borrow_mut().push(argv.to_vec());
        if self.missing.contains(&argv[0]) {
            return Err(CommandFailure::Start("no such program".into()));
        }
        Ok(*self.exit0.borrow().get(&argv[0]).unwrap_or(&false))
    }
    fn stdout(&self, argv: &[String], _t: u64) -> Result<Option<String>, CommandFailure> {
        self.ran.borrow_mut().push(argv.to_vec());
        Ok(self.stdout.borrow().get(&argv[0]).cloned().flatten())
    }
}

fn pr_cond(program: &str) -> DoneCond {
    DoneCond::Pr {
        command: CommandSpec::new(&[program, "{unit.id}"]),
        test: PrTest::Url,
    }
}

fn exit0_cond(program: &str) -> DoneCond {
    DoneCond::Exit0(CommandSpec::new(&[program]))
}

// ------------------------------------------------------------------ the three verdicts

#[test]
fn it_keeps_waiting_while_a_done_condition_is_unmet() {
    let dir = tempfile::tempdir().unwrap();
    let s = stage(
        dir.path(),
        vec![file(dir.path(), "a.done"), file(dir.path(), "b.done")],
        vec![],
    );
    assert_eq!(v(&s, &SystemProbe, &WAITING), StageVerdict::Wait);
    // One of two files: still waiting. All conditions must hold.
    write(dir.path(), "u1", "a.done", "");
    assert_eq!(v(&s, &SystemProbe, &WAITING), StageVerdict::Wait);
}

#[test]
fn it_is_done_when_every_done_condition_holds() {
    let dir = tempfile::tempdir().unwrap();
    let s = stage(
        dir.path(),
        vec![file(dir.path(), "a.done"), file(dir.path(), "b.done")],
        vec![],
    );
    write(dir.path(), "u1", "a.done", "");
    write(dir.path(), "u1", "b.done", "");
    let StageVerdict::Done { pr, signal } = v(&s, &SystemProbe, &WAITING) else {
        panic!("done")
    };
    assert_eq!(pr, None);
    assert_eq!(
        signal,
        Signal::DoneFile {
            path: format!("{}/u1/a.done", dir.path().display())
        }
    );
}

#[test]
fn all_conditions_must_hold_across_files_commands_and_prs() {
    let dir = tempfile::tempdir().unwrap();
    let s = stage(
        dir.path(),
        vec![
            file(dir.path(), "a.done"),
            exit0_cond("check"),
            pr_cond("show-pr"),
        ],
        vec![],
    );
    write(dir.path(), "u1", "a.done", "");
    let probe = Scripted::default();
    // The command says no.
    assert_eq!(v(&s, &probe, &WAITING), StageVerdict::Wait);
    // The command says yes, and the PR command prints nothing useful.
    probe.exit0.borrow_mut().insert("check".into(), true);
    for printed in [None, Some("null"), Some(""), Some("not a url")] {
        probe
            .stdout
            .borrow_mut()
            .insert("show-pr".into(), printed.map(String::from));
        assert_eq!(v(&s, &probe, &WAITING), StageVerdict::Wait, "{printed:?}");
    }
    // Now all hold. The verdict carries the PR.
    probe.stdout.borrow_mut().insert(
        "show-pr".into(),
        Some("https://github.com/owner/repo/pull/12\n".into()),
    );
    let StageVerdict::Done { pr, .. } = v(&s, &probe, &WAITING) else {
        panic!("done")
    };
    assert_eq!(
        pr,
        Some(PrRef {
            repo: "owner/repo".into(),
            number: 12
        })
    );
    // The command argv was rendered: the unit ID reached the PR command.
    assert!(
        probe
            .ran
            .borrow()
            .iter()
            .any(|a| a == &["show-pr".to_string(), "u1".to_string()])
    );
}

#[test]
fn a_command_is_not_run_while_an_earlier_condition_is_unmet() {
    let dir = tempfile::tempdir().unwrap();
    let s = stage(
        dir.path(),
        vec![file(dir.path(), "a.done"), exit0_cond("check")],
        vec![],
    );
    let probe = Scripted::default();
    assert_eq!(v(&s, &probe, &WAITING), StageVerdict::Wait);
    assert!(probe.ran.borrow().is_empty());
}

#[test]
fn a_pr_reference_is_read_from_a_url_or_a_short_form() {
    let ok = |t: &str| parse_pr(t);
    assert_eq!(
        ok("https://github.com/o/r/pull/7"),
        Some(PrRef {
            repo: "o/r".into(),
            number: 7
        })
    );
    assert_eq!(
        ok("  https://github.com/o/r/pull/7  \n"),
        Some(PrRef {
            repo: "o/r".into(),
            number: 7
        })
    );
    assert_eq!(
        ok("o/r#7"),
        Some(PrRef {
            repo: "o/r".into(),
            number: 7
        })
    );
    // The bare forge names a repo by its absolute path.
    assert_eq!(
        ok("file:///a/b#3"),
        Some(PrRef {
            repo: "/a/b".into(),
            number: 3
        })
    );
    for bad in [
        "file://a/b#3",
        "file:///a/b#0",
        "file:///a/b",
        "file://#3",
        "",
        "null",
        "https://github.com/o/r/issues/7",
        "https://github.com/o/pull/7",
        "o/r#x",
        "a b",
        "http://github.com/o/r/pull/7 extra",
        "https://github.com/o/r/pull/0",
    ] {
        assert_eq!(ok(bad), None, "{bad:?}");
    }
}

// ------------------------------------------------------------------ stop file and park files

#[test]
fn the_stop_file_wins_over_a_done_file() {
    let dir = tempfile::tempdir().unwrap();
    let s = stage(dir.path(), vec![file(dir.path(), "a.done")], vec![]);
    write(dir.path(), "u1", "a.done", "");
    write(dir.path(), "u1", "stop", "the plan has no spec\n");
    assert_eq!(
        v(&s, &SystemProbe, &WAITING),
        StageVerdict::Park {
            reason: "the plan has no spec".to_string()
        }
    );
}

#[test]
fn a_park_file_parks_with_its_reason() {
    let dir = tempfile::tempdir().unwrap();
    let park = ParkOn {
        file: format!("{}/{{unit.id}}/gaps.json", dir.path().display()),
        reason: "gaps surfaced for {unit.id}".to_string(),
    };
    let s = stage(dir.path(), vec![file(dir.path(), "a.done")], vec![park]);
    assert_eq!(v(&s, &SystemProbe, &WAITING), StageVerdict::Wait);
    write(dir.path(), "u1", "gaps.json", "{}");
    assert_eq!(
        v(&s, &SystemProbe, &WAITING),
        StageVerdict::Park {
            reason: "gaps surfaced for u1".to_string()
        }
    );
    // The stop file still wins over it.
    write(dir.path(), "u1", "stop", "stopped");
    assert_eq!(
        v(&s, &SystemProbe, &WAITING),
        StageVerdict::Park {
            reason: "stopped".to_string()
        }
    );
}

#[test]
fn a_park_file_wins_over_a_done_file() {
    let dir = tempfile::tempdir().unwrap();
    let park = ParkOn {
        file: format!("{}/{{unit.id}}/correction.txt", dir.path().display()),
        reason: "correction needed".to_string(),
    };
    let s = stage(dir.path(), vec![file(dir.path(), "a.done")], vec![park]);
    write(dir.path(), "u1", "a.done", "");
    write(dir.path(), "u1", "correction.txt", "x");
    assert_eq!(
        v(&s, &SystemProbe, &WAITING),
        StageVerdict::Park {
            reason: "correction needed".to_string()
        }
    );
}

#[test]
fn a_turn_end_with_no_done_condition_parks() {
    let dir = tempfile::tempdir().unwrap();
    let s = stage(dir.path(), vec![file(dir.path(), "a.done")], vec![]);
    let expected = StageVerdict::Park {
        reason: "seat ended with no done signal".to_string(),
    };
    assert_eq!(v(&s, &SystemProbe, &TURN_ENDED), expected);
    // A session that ended without a turn end counts too.
    assert_eq!(
        v(
            &s,
            &SystemProbe,
            &SessionState {
                turn_ended: false,
                ended: true
            }
        ),
        expected
    );
    // A done condition wins over the turn end.
    write(dir.path(), "u1", "a.done", "");
    assert!(matches!(
        v(&s, &SystemProbe, &TURN_ENDED),
        StageVerdict::Done { .. }
    ));
}

#[test]
fn an_exit0_that_says_not_yet_waits_and_parks_only_at_the_turn_end() {
    let dir = tempfile::tempdir().unwrap();
    let s = stage(dir.path(), vec![exit0_cond("false")], vec![]);
    assert_eq!(v(&s, &SystemProbe, &WAITING), StageVerdict::Wait);
    assert_eq!(
        v(&s, &SystemProbe, &TURN_ENDED),
        StageVerdict::Park {
            reason: "seat ended with no done signal".to_string()
        }
    );
    let s = stage(dir.path(), vec![exit0_cond("true")], vec![]);
    assert!(matches!(
        v(&s, &SystemProbe, &WAITING),
        StageVerdict::Done {
            signal: Signal::ExitCode { code: 0 },
            ..
        }
    ));
}

#[test]
fn a_command_that_cannot_run_parks_with_its_failure() {
    let dir = tempfile::tempdir().unwrap();
    let s = stage(
        dir.path(),
        vec![exit0_cond("definitely-not-a-program-xyz")],
        vec![],
    );
    let StageVerdict::Park { reason } = v(&s, &SystemProbe, &WAITING) else {
        panic!("park")
    };
    assert!(
        reason.contains("done condition 1") && reason.contains("could not start"),
        "{reason}"
    );

    // A value from the record that starts with a dash is refused.
    let s = stage(
        dir.path(),
        vec![DoneCond::Exit0(CommandSpec::new(&[
            "echo",
            "{unit.vars.x}",
        ]))],
        vec![],
    );
    let vars = BTreeMap::from([("x".to_string(), "--evil".to_string())]);
    let StageVerdict::Park { reason } = verdict(&s, &vars, &SystemProbe, &WAITING) else {
        panic!("park")
    };
    assert!(reason.contains("starts with"), "{reason}");

    // A record that lacks a variable parks too.
    let StageVerdict::Park { reason } = verdict(&s, &BTreeMap::new(), &SystemProbe, &WAITING)
    else {
        panic!("park")
    };
    assert!(reason.contains("x"), "{reason}");
}

// ------------------------------------------------------------------ against a real engine

fn running_unit() -> Env {
    let e = env(item_workflow(&["build"], false));
    e.engine
        .ingest(&mut TrackedSource {
            track: "item".into(),
            records: vec![unit("u1", "g1", 1)],
        })
        .unwrap();
    assert!(matches!(
        e.engine.start_stage("u1").unwrap(),
        LaunchStep::Launched { .. }
    ));
    e
}

#[test]
fn a_multi_line_stop_file_parks_a_real_unit_with_a_one_line_reason() {
    let dir = tempfile::tempdir().unwrap();
    let e = running_unit();
    let s = stage(dir.path(), vec![file(dir.path(), "a.done")], vec![]);
    write(
        dir.path(),
        "u1",
        "stop",
        "not planned yet: g has no spec\nplan it, then clear this park\r\nhalt: spec missing\n",
    );
    let verdict = v(&s, &SystemProbe, &WAITING);
    let applied = apply(&e.engine, "u1", "build", &verdict).unwrap();
    let ApplyResult::Parked(reason) = applied else {
        panic!("{applied:?}")
    };
    assert!(
        !reason.contains('\n') && !reason.contains('\r'),
        "{reason:?}"
    );
    assert!(
        reason.starts_with("not planned yet: g has no spec"),
        "{reason}"
    );
    assert!(reason.contains("halt: spec missing"), "{reason}");
    let (status, stored) = e.status_of("u1");
    assert_eq!(status, Status::Parked);
    assert_eq!(stored.as_deref(), Some(reason.as_str()));
}

#[test]
fn an_empty_stop_file_parks_a_real_unit_with_a_one_line_reason() {
    let dir = tempfile::tempdir().unwrap();
    let e = running_unit();
    let s = stage(dir.path(), vec![file(dir.path(), "a.done")], vec![]);
    write(dir.path(), "u1", "stop", "");
    let verdict = v(&s, &SystemProbe, &WAITING);
    let ApplyResult::Parked(reason) = apply(&e.engine, "u1", "build", &verdict).unwrap() else {
        panic!("park")
    };
    assert_eq!(reason, "no reason text given");
    assert_eq!(
        e.status_of("u1"),
        (Status::Parked, Some("no reason text given".to_string()))
    );
}

#[test]
fn a_done_verdict_ends_the_stage_and_a_wait_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let e = running_unit();
    let s = stage(dir.path(), vec![file(dir.path(), "a.done")], vec![]);
    assert_eq!(
        apply(&e.engine, "u1", "build", &v(&s, &SystemProbe, &WAITING)).unwrap(),
        ApplyResult::Waiting
    );
    assert_eq!(e.status_of("u1").0, Status::Running);
    write(dir.path(), "u1", "a.done", "");
    let applied = apply(&e.engine, "u1", "build", &v(&s, &SystemProbe, &WAITING)).unwrap();
    assert_eq!(applied, ApplyResult::Signalled);
    assert_eq!(e.status_of("u1").0, Status::Done);
    // The signal is recorded once: a second call is a replay.
    let again = apply(&e.engine, "u1", "build", &v(&s, &SystemProbe, &WAITING)).unwrap();
    assert_eq!(again, ApplyResult::Signalled);
}

#[test]
fn a_park_file_and_a_turn_end_park_a_real_unit() {
    let dir = tempfile::tempdir().unwrap();
    let park = ParkOn {
        file: format!("{}/{{unit.id}}/gaps.json", dir.path().display()),
        reason: "gaps surfaced\nanswer them".to_string(),
    };
    let s = stage(dir.path(), vec![file(dir.path(), "a.done")], vec![park]);
    let e = running_unit();
    write(dir.path(), "u1", "gaps.json", "{}");
    let ApplyResult::Parked(reason) =
        apply(&e.engine, "u1", "build", &v(&s, &SystemProbe, &WAITING)).unwrap()
    else {
        panic!("park")
    };
    assert_eq!(reason, "gaps surfaced / answer them");
    assert_eq!(e.status_of("u1").0, Status::Parked);

    let e = running_unit();
    let s = stage(dir.path(), vec![file(dir.path(), "never.done")], vec![]);
    std::fs::remove_dir_all(dir.path().join("u1")).unwrap();
    let ApplyResult::Parked(reason) =
        apply(&e.engine, "u1", "build", &v(&s, &SystemProbe, &TURN_ENDED)).unwrap()
    else {
        panic!("park")
    };
    assert_eq!(reason, "seat ended with no done signal");
    assert_eq!(
        e.status_of("u1"),
        (
            Status::Parked,
            Some("seat ended with no done signal".to_string())
        )
    );
}

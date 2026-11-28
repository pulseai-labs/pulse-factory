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
use pf_engine::port::store::{Cause, RequestKind};
use pf_engine::render::{RenderContext, UnitView};
use pf_engine::unit::Signal;
use pf_workflow::commands::{CommandFailure, CommandSpec};
use pf_workflow::file::{DoneCond, NoSignal, OutcomeSpec, ParkOn, PrTest, StageSpec};
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
        stop_on_answer: Vec::new(),
        on_answer: Vec::new(),
        stall_minutes: 10,
        first_turn_seconds: 60,
        skills: Vec::new(),
        settings: BTreeMap::new(),
        done,
        park_on,
        outcomes: Vec::new(),
        entered_by_outcome: false,
        entered_by_conflict: false,
        base_push: None,
        no_signal: None,
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
    nudges_sent: 0,
    no_signal_ran: false,
};
const TURN_ENDED: SessionState = SessionState {
    turn_ended: true,
    ended: false,
    nudges_sent: 0,
    no_signal_ran: false,
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
            reason: "the plan has no spec".to_string(),
            kind: RequestKind::Stop,
            cause: Cause::StopFile,
        }
    );
}

#[test]
fn a_park_file_parks_with_its_reason() {
    let dir = tempfile::tempdir().unwrap();
    let park = ParkOn {
        file: format!("{}/{{unit.id}}/gaps.json", dir.path().display()),
        reason: "gaps surfaced for {unit.id}".to_string(),
        on_answer: Vec::new(),
        options: Vec::new(),
    };
    let s = stage(dir.path(), vec![file(dir.path(), "a.done")], vec![park]);
    assert_eq!(v(&s, &SystemProbe, &WAITING), StageVerdict::Wait);
    write(dir.path(), "u1", "gaps.json", "{}");
    assert_eq!(
        v(&s, &SystemProbe, &WAITING),
        StageVerdict::Park {
            reason: "gaps surfaced for u1".to_string(),
            kind: RequestKind::Gaps,
            cause: Cause::ParkFile,
        }
    );
    // The stop file still wins over it.
    write(dir.path(), "u1", "stop", "stopped");
    assert_eq!(
        v(&s, &SystemProbe, &WAITING),
        StageVerdict::Park {
            reason: "stopped".to_string(),
            kind: RequestKind::Stop,
            cause: Cause::StopFile,
        }
    );
}

#[test]
fn a_park_file_wins_over_a_done_file() {
    let dir = tempfile::tempdir().unwrap();
    let park = ParkOn {
        file: format!("{}/{{unit.id}}/correction.txt", dir.path().display()),
        reason: "correction needed".to_string(),
        on_answer: Vec::new(),
        options: Vec::new(),
    };
    let s = stage(dir.path(), vec![file(dir.path(), "a.done")], vec![park]);
    write(dir.path(), "u1", "a.done", "");
    write(dir.path(), "u1", "correction.txt", "x");
    assert_eq!(
        v(&s, &SystemProbe, &WAITING),
        StageVerdict::Park {
            reason: "correction needed".to_string(),
            kind: RequestKind::RedGate,
            cause: Cause::ParkFile,
        }
    );
}

#[test]
fn a_turn_end_with_no_done_condition_parks() {
    let dir = tempfile::tempdir().unwrap();
    let s = stage(dir.path(), vec![file(dir.path(), "a.done")], vec![]);
    let expected = StageVerdict::Park {
        reason: "seat ended with no done signal".to_string(),
        kind: RequestKind::Park,
        cause: Cause::NoDoneSignal,
    };
    assert_eq!(v(&s, &SystemProbe, &TURN_ENDED), expected);
    // A session that ended without a turn end counts too.
    assert_eq!(
        v(
            &s,
            &SystemProbe,
            &SessionState {
                turn_ended: false,
                ended: true,
                nudges_sent: 0,
                no_signal_ran: false,
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
            reason: "seat ended with no done signal".to_string(),
            kind: RequestKind::Park,
            cause: Cause::NoDoneSignal,
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
    let StageVerdict::Park { reason, cause, .. } = v(&s, &SystemProbe, &WAITING) else {
        panic!("park")
    };
    assert!(
        reason.contains("done condition 1") && reason.contains("could not start"),
        "{reason}"
    );
    assert_eq!(cause, Cause::HookFailed);

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
    let StageVerdict::Park { reason, cause, .. } = verdict(&s, &vars, &SystemProbe, &WAITING)
    else {
        panic!("park")
    };
    assert!(reason.contains("starts with"), "{reason}");
    assert_eq!(cause, Cause::RenderFailed);

    // A record that lacks a variable parks too.
    let StageVerdict::Park { reason, cause, .. } =
        verdict(&s, &BTreeMap::new(), &SystemProbe, &WAITING)
    else {
        panic!("park")
    };
    assert!(reason.contains("x"), "{reason}");
    assert_eq!(cause, Cause::RenderFailed);
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
        on_answer: Vec::new(),
        options: Vec::new(),
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

// ------------------------------------------------------------------ outcomes

fn outcome(when: Vec<DoneCond>, to: &str, max: u32) -> OutcomeSpec {
    OutcomeSpec {
        when,
        to: to.to_string(),
        max,
    }
}

/// A stage that is done when `a.done` exists, with two outcomes: `blocked` sends the unit to
/// `fix`, and `stale` sends it to `build`.
fn looping_stage(dir: &Path) -> StageSpec {
    let mut s = stage(dir, vec![file(dir, "a.done")], vec![]);
    s.outcomes = vec![
        outcome(vec![exit0_cond("blocked")], "fix", 2),
        outcome(vec![exit0_cond("stale")], "build", 1),
    ];
    s
}

#[test]
fn an_outcome_is_tried_only_when_the_session_is_over() {
    let dir = tempfile::tempdir().unwrap();
    let s = looping_stage(dir.path());
    write(dir.path(), "u1", "a.done", "");
    let probe = Scripted::default();
    probe.exit0.borrow_mut().insert("blocked".into(), true);

    // The seat still works: the stage is done, and no `when` command ran.
    assert!(matches!(v(&s, &probe, &WAITING), StageVerdict::Done { .. }));
    assert!(probe.ran.borrow().is_empty(), "{:?}", probe.ran.borrow());

    // The turn ended: the first entry that holds wins.
    let StageVerdict::Outcome { entry, signal, .. } = v(&s, &probe, &TURN_ENDED) else {
        panic!("an outcome")
    };
    assert_eq!(entry, 0);
    assert!(matches!(signal, Signal::DoneFile { .. }));
    let ended = SessionState {
        turn_ended: false,
        ended: true,
        nudges_sent: 0,
        no_signal_ran: false,
    };
    assert!(matches!(
        v(&s, &probe, &ended),
        StageVerdict::Outcome { entry: 0, .. }
    ));
}

#[test]
fn the_first_entry_that_holds_wins_and_the_later_ones_are_not_read() {
    let dir = tempfile::tempdir().unwrap();
    let s = looping_stage(dir.path());
    write(dir.path(), "u1", "a.done", "");
    let probe = Scripted::default();
    probe.exit0.borrow_mut().insert("stale".into(), true);
    // Entry 1 does not hold, entry 2 does.
    assert!(matches!(
        v(&s, &probe, &TURN_ENDED),
        StageVerdict::Outcome { entry: 1, .. }
    ));
    probe.ran.borrow_mut().clear();
    probe.exit0.borrow_mut().insert("blocked".into(), true);
    assert!(matches!(
        v(&s, &probe, &TURN_ENDED),
        StageVerdict::Outcome { entry: 0, .. }
    ));
    assert_eq!(
        probe
            .ran
            .borrow()
            .iter()
            .map(|a| a[0].as_str())
            .collect::<Vec<_>>(),
        ["blocked"],
        "entry 2 is not read once entry 1 holds"
    );
}

#[test]
fn a_stage_whose_outcomes_match_nothing_is_done_as_before() {
    let dir = tempfile::tempdir().unwrap();
    let s = looping_stage(dir.path());
    write(dir.path(), "u1", "a.done", "");
    let probe = Scripted::default();
    assert!(matches!(
        v(&s, &probe, &TURN_ENDED),
        StageVerdict::Done { .. }
    ));
    // The done conditions still decide first: with no done file there is no outcome.
    let dir2 = tempfile::tempdir().unwrap();
    let s2 = looping_stage(dir2.path());
    probe.exit0.borrow_mut().insert("blocked".into(), true);
    assert_eq!(
        v(&s2, &probe, &TURN_ENDED),
        StageVerdict::Park {
            reason: "seat ended with no done signal".to_string(),
            kind: RequestKind::Park,
            cause: Cause::NoDoneSignal,
        }
    );
}

#[test]
fn every_when_condition_of_an_entry_must_hold() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = stage(dir.path(), vec![file(dir.path(), "a.done")], vec![]);
    s.outcomes = vec![outcome(
        vec![file(dir.path(), "extra.flag"), exit0_cond("yes")],
        "build",
        1,
    )];
    write(dir.path(), "u1", "a.done", "");
    let probe = Scripted::default();
    probe.exit0.borrow_mut().insert("yes".into(), true);
    assert!(matches!(
        v(&s, &probe, &TURN_ENDED),
        StageVerdict::Done { .. }
    ));
    write(dir.path(), "u1", "extra.flag", "");
    assert!(matches!(
        v(&s, &probe, &TURN_ENDED),
        StageVerdict::Outcome { entry: 0, .. }
    ));
}

#[test]
fn a_when_command_that_cannot_run_parks_with_its_place() {
    let dir = tempfile::tempdir().unwrap();
    let s = looping_stage(dir.path());
    write(dir.path(), "u1", "a.done", "");
    let mut probe = Scripted::default();
    probe.missing.insert("blocked".to_string());
    let StageVerdict::Park {
        reason,
        kind,
        cause,
    } = v(&s, &probe, &TURN_ENDED)
    else {
        panic!("park")
    };
    assert_eq!(kind, RequestKind::Park);
    // The condition's command could not run: a failed command, not a render failure.
    assert_eq!(cause, Cause::HookFailed);
    assert!(
        reason.contains("outcome 1 condition 1") && reason.contains("blocked"),
        "{reason}"
    );
}

#[test]
fn applying_an_outcome_sends_a_real_unit_to_the_stage_it_names() {
    let mut wf = item_workflow(&["build", "fix"], false);
    wf.tracks[0].stages[0].outcomes = vec![pf_engine::OutcomeDef {
        to: "fix".to_string(),
        max: 1,
    }];
    let e = env(wf);
    e.engine
        .ingest(&mut TrackedSource {
            track: "item".into(),
            records: vec![unit("u1", "g1", 1)],
        })
        .unwrap();
    e.engine.start_stage("u1").unwrap();
    let verdict = StageVerdict::Outcome {
        pr: None,
        signal: Signal::DoneFile {
            path: "x".to_string(),
        },
        entry: 0,
    };
    assert_eq!(
        apply(&e.engine, "u1", "build", &verdict).unwrap(),
        ApplyResult::Signalled
    );
    let view = e.engine.view().unwrap();
    let detail = view.unit_detail(view.node("u1").unwrap()).unwrap();
    assert_eq!(detail.stages_done, 1);
    assert_eq!(detail.visits.get("build#0"), Some(&1));
}

// ------------------------------------------------------------------ no_signal (r2.s1.w1)

/// A stage that is done when `a.done` exists and declares `no_signal`.
fn nudging_stage(dir: &Path, nudges: u32, run: &[&str]) -> StageSpec {
    let mut s = stage(dir, vec![file(dir, "a.done")], vec![]);
    s.no_signal = Some(NoSignal {
        nudge: (nudges > 0).then(|| "Finish {unit.id}.".to_string()),
        nudges,
        run: run.iter().map(|p| CommandSpec::new(&[p])).collect(),
    });
    s
}

fn state(turn_ended: bool, ended: bool, nudges_sent: u32, no_signal_ran: bool) -> SessionState {
    SessionState {
        turn_ended,
        ended,
        nudges_sent,
        no_signal_ran,
    }
}

const NO_SIGNAL_REASON: &str = "seat ended with no done signal";

#[test]
fn with_nudges_left_a_turn_end_with_no_signal_gets_the_nudge_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let s = nudging_stage(dir.path(), 2, &["explain"]);
    // The text is the template, rendered for the unit. The verdict counts the nudge: 1 of 2.
    assert_eq!(
        v(&s, &SystemProbe, &state(true, false, 0, false)),
        StageVerdict::Nudge {
            text: "Finish u1.".to_string(),
            number: 1,
            of: 2,
        }
    );
    assert_eq!(
        v(&s, &SystemProbe, &state(true, false, 1, false)),
        StageVerdict::Nudge {
            text: "Finish u1.".to_string(),
            number: 2,
            of: 2,
        }
    );
}

#[test]
fn a_seat_that_still_works_is_never_nudged() {
    let dir = tempfile::tempdir().unwrap();
    let s = nudging_stage(dir.path(), 2, &["explain"]);
    assert_eq!(v(&s, &SystemProbe, &WAITING), StageVerdict::Wait);
}

#[test]
fn with_no_nudges_left_the_verdict_is_run_then_park() {
    let dir = tempfile::tempdir().unwrap();
    let s = nudging_stage(dir.path(), 2, &["explain"]);
    assert_eq!(
        v(&s, &SystemProbe, &state(true, false, 2, false)),
        StageVerdict::RunThenPark
    );
    // A stage that declares no nudge, only `run`, goes to the commands at once.
    let run_only = nudging_stage(dir.path(), 0, &["explain"]);
    assert_eq!(
        v(&run_only, &SystemProbe, &TURN_ENDED),
        StageVerdict::RunThenPark
    );
}

#[test]
fn a_session_whose_process_ended_is_not_nudged() {
    let dir = tempfile::tempdir().unwrap();
    let s = nudging_stage(dir.path(), 2, &["explain"]);
    // A message reaches a live session only, so the commands run at once.
    assert_eq!(
        v(&s, &SystemProbe, &state(false, true, 0, false)),
        StageVerdict::RunThenPark
    );
}

#[test]
fn once_the_commands_ran_the_stage_parks_as_it_did_before() {
    let dir = tempfile::tempdir().unwrap();
    let s = nudging_stage(dir.path(), 1, &["explain"]);
    assert_eq!(
        v(&s, &SystemProbe, &state(true, false, 1, true)),
        StageVerdict::Park {
            reason: NO_SIGNAL_REASON.to_string(),
            kind: RequestKind::Park,
            cause: Cause::NoDoneSignal,
        }
    );
    // Even with nudges left in the count: the commands ran, so there is no second chance.
    assert_eq!(
        v(&s, &SystemProbe, &state(true, false, 0, true)),
        StageVerdict::Park {
            reason: NO_SIGNAL_REASON.to_string(),
            kind: RequestKind::Park,
            cause: Cause::NoDoneSignal,
        }
    );
}

#[test]
fn a_stage_that_declares_a_nudge_and_no_commands_parks_when_the_nudges_are_spent() {
    let dir = tempfile::tempdir().unwrap();
    let s = nudging_stage(dir.path(), 1, &[]);
    assert!(matches!(
        v(&s, &SystemProbe, &state(true, false, 0, false)),
        StageVerdict::Nudge { .. }
    ));
    assert_eq!(
        v(&s, &SystemProbe, &state(true, false, 1, false)),
        StageVerdict::Park {
            reason: NO_SIGNAL_REASON.to_string(),
            kind: RequestKind::Park,
            cause: Cause::NoDoneSignal,
        }
    );
}

#[test]
fn a_stage_without_no_signal_parks_with_the_generic_reason_as_before() {
    let dir = tempfile::tempdir().unwrap();
    let s = stage(dir.path(), vec![file(dir.path(), "a.done")], vec![]);
    assert_eq!(s.no_signal, None);
    for st in [
        state(true, false, 0, false),
        state(false, true, 0, false),
        state(true, false, 3, false),
    ] {
        assert_eq!(
            v(&s, &SystemProbe, &st),
            StageVerdict::Park {
                reason: NO_SIGNAL_REASON.to_string(),
                kind: RequestKind::Park,
                cause: Cause::NoDoneSignal,
            }
        );
    }
}

#[test]
fn a_stop_file_a_park_file_and_a_done_signal_win_over_every_no_signal_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = nudging_stage(dir.path(), 2, &["explain"]);
    s.park_on = vec![ParkOn {
        file: format!("{}/{{unit.id}}/gaps.json", dir.path().display()),
        reason: "gaps".to_string(),
        on_answer: Vec::new(),
        options: Vec::new(),
    }];
    let states = [
        state(true, false, 0, false),
        state(true, false, 2, false),
        state(true, false, 2, true),
        state(false, true, 0, false),
    ];

    // The done signal wins.
    write(dir.path(), "u1", "a.done", "");
    for st in &states {
        assert!(
            matches!(v(&s, &SystemProbe, st), StageVerdict::Done { .. }),
            "{st:?}"
        );
    }
    std::fs::remove_file(dir.path().join("u1/a.done")).unwrap();

    // The park file wins over the nudge and over the commands.
    write(dir.path(), "u1", "gaps.json", "[]");
    for st in &states {
        assert_eq!(
            v(&s, &SystemProbe, st),
            StageVerdict::Park {
                reason: "gaps".to_string(),
                kind: RequestKind::Gaps,
                cause: Cause::ParkFile,
            },
            "{st:?}"
        );
    }

    // The stop file wins over every other file, with its text as the reason.
    write(dir.path(), "u1", "stop", "the seat says stop");
    for st in &states {
        assert_eq!(
            v(&s, &SystemProbe, st),
            StageVerdict::Park {
                reason: "the seat says stop".to_string(),
                kind: RequestKind::Stop,
                cause: Cause::StopFile,
            },
            "{st:?}"
        );
    }
}

#[test]
fn the_stop_file_a_command_wrote_parks_as_a_stop_after_the_commands_ran() {
    let dir = tempfile::tempdir().unwrap();
    let s = nudging_stage(dir.path(), 1, &["explain"]);
    let ran = state(true, false, 1, true);
    assert_eq!(
        v(&s, &SystemProbe, &ran),
        StageVerdict::Park {
            reason: NO_SIGNAL_REASON.to_string(),
            kind: RequestKind::Park,
            cause: Cause::NoDoneSignal,
        }
    );
    write(
        dir.path(),
        "u1",
        "stop",
        "stranded: the PR was never opened",
    );
    assert_eq!(
        v(&s, &SystemProbe, &ran),
        StageVerdict::Park {
            reason: "stranded: the PR was never opened".to_string(),
            kind: RequestKind::Stop,
            cause: Cause::StopFile,
        }
    );
}

#[test]
fn an_exit0_that_says_not_yet_is_nudged_at_the_turn_end_and_waits_before_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = nudging_stage(dir.path(), 1, &["explain"]);
    s.done = vec![exit0_cond("false")];
    assert_eq!(v(&s, &SystemProbe, &WAITING), StageVerdict::Wait);
    assert!(matches!(
        v(&s, &SystemProbe, &TURN_ENDED),
        StageVerdict::Nudge {
            number: 1,
            of: 1,
            ..
        }
    ));
}

#[test]
fn a_nudge_that_cannot_render_parks_with_the_stage_named() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = nudging_stage(dir.path(), 1, &["explain"]);
    s.no_signal.as_mut().unwrap().nudge = Some("Finish {unit.vars.missing}.".to_string());
    let StageVerdict::Park {
        reason,
        kind,
        cause,
    } = v(&s, &SystemProbe, &TURN_ENDED)
    else {
        panic!("a park")
    };
    assert_eq!(kind, RequestKind::Park);
    assert_eq!(cause, Cause::RenderFailed);
    assert!(
        reason.contains("stage build") && reason.contains("nudge"),
        "{reason}"
    );
}

#[test]
fn applying_a_nudge_or_run_verdict_changes_nothing_in_the_engine() {
    let wf = item_workflow(&["build"], false);
    let e = env(wf);
    e.engine
        .ingest(&mut TrackedSource {
            track: "item".into(),
            records: vec![unit("u1", "g1", 1)],
        })
        .unwrap();
    e.engine.start_stage("u1").unwrap();
    for verdict in [
        StageVerdict::Nudge {
            text: "x".to_string(),
            number: 1,
            of: 1,
        },
        StageVerdict::RunThenPark,
    ] {
        assert_eq!(
            apply(&e.engine, "u1", "build", &verdict).unwrap(),
            ApplyResult::Waiting
        );
    }
    assert_eq!(
        e.engine.view().unwrap().node("u1").unwrap().status,
        Status::Running
    );
}

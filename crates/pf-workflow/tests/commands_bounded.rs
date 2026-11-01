//! AC-6: commands as data. Every command the file names is an argv list, run under fixed bounds.
//! There is no shell in the runner.

#[path = "../../pf-engine/tests/common/mod.rs"]
mod common;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use common::{Env, TrackedSource, env, item_workflow, unit};
use pf_engine::Status;
use pf_engine::launch::LaunchStep;
use pf_engine::render::{RenderContext, UnitView};
use pf_workflow::commands::{
    CommandFailure, CommandSpec, DEFAULT_TIMEOUT_SECONDS, HookOutcome, MAX_OUTPUT_BYTES,
    MAX_TIMEOUT_SECONDS, exit0, run_after_merge, run_command, run_plan_check, run_unit_hook,
};

fn params() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("dash".to_string(), "-p".to_string()),
        ("word".to_string(), "hello".to_string()),
        ("count".to_string(), "5".to_string()),
    ])
}

fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn ctx<'a>(params: &'a BTreeMap<String, String>, unit: Option<UnitView<'a>>) -> RenderContext<'a> {
    RenderContext {
        run_id: "run-1",
        run_dir: "/run/dir",
        params,
        unit,
    }
}

fn spec(argv: &[&str]) -> CommandSpec {
    CommandSpec::new(argv)
}

fn with_timeout(argv: &[&str], seconds: u64) -> CommandSpec {
    CommandSpec {
        timeout_seconds: seconds,
        ..CommandSpec::new(argv)
    }
}

fn stdout(out: &pf_workflow::commands::RunOutput) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// An engine with one running unit `u1` whose record holds `pairs`.
fn running_unit(pairs: &[(&str, &str)]) -> Env {
    let e = env(item_workflow(&["build"], false));
    let mut record = unit("u1", "g1", 1);
    record.vars = vars(pairs);
    e.engine
        .ingest(&mut TrackedSource {
            track: "item".into(),
            records: vec![record],
        })
        .unwrap();
    assert!(matches!(
        e.engine.start_stage("u1").unwrap(),
        LaunchStep::Launched { .. }
    ));
    e
}

fn unit_ctx<'a>(
    e: &Env,
    params: &'a BTreeMap<String, String>,
    vars: &'a BTreeMap<String, String>,
) -> RenderContext<'a> {
    let _ = e;
    ctx(params, Some(UnitView { id: "u1", vars }))
}

fn parked_reason(e: &Env) -> String {
    let (status, reason) = e.status_of("u1");
    assert_eq!(status, Status::Parked);
    reason.unwrap()
}

// ------------------------------------------------------------------ argv, one item at a time

#[test]
fn the_argv_reaches_the_program_item_by_item() {
    let p = params();
    let v = vars(&[("x", "two words"), ("y", "a;b")]);
    let c = ctx(&p, Some(UnitView { id: "u1", vars: &v }));
    let out = run_command(
        &spec(&[
            "printf",
            "[%s]\n",
            "{unit.vars.x}",
            "{unit.vars.y}",
            "pre-{unit.id}-post",
            "{param.word}",
        ]),
        &c,
    )
    .unwrap();
    assert_eq!(out.exit_code, Some(0));
    assert_eq!(stdout(&out), "[two words]\n[a;b]\n[pre-u1-post]\n[hello]\n");
}

#[test]
fn a_value_holding_a_shell_command_stays_one_argument_and_is_never_run() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("marker");
    let hostile = format!("; rm -rf x; touch {}", marker.display());
    let p = params();
    let v = vars(&[
        ("x", &hostile),
        ("y", "$(touch /nonexistent/never)"),
        ("z", "`id`"),
    ]);
    let c = ctx(&p, Some(UnitView { id: "u1", vars: &v }));
    let out = run_command(
        &spec(&[
            "printf",
            "<%s>\n",
            "{unit.vars.x}",
            "{unit.vars.y}",
            "{unit.vars.z}",
        ]),
        &c,
    )
    .unwrap();
    let lines: Vec<String> = stdout(&out).lines().map(str::to_string).collect();
    assert_eq!(
        lines,
        [
            format!("<{hostile}>"),
            "<$(touch /nonexistent/never)>".into(),
            "<`id`>".into()
        ]
    );
    assert!(!marker.exists());

    // A value with a newline stays one item too.
    let v = vars(&[("x", "one\ntwo")]);
    let c = ctx(&p, Some(UnitView { id: "u1", vars: &v }));
    let out = run_command(&spec(&["printf", "%s", "{unit.vars.x}"]), &c).unwrap();
    assert_eq!(stdout(&out), "one\ntwo");
}

#[test]
fn a_value_that_starts_with_a_dash_from_a_record_is_refused() {
    let p = params();
    let v = vars(&[("x", "--exec=evil"), ("y", "-n")]);
    let c = ctx(&p, Some(UnitView { id: "u1", vars: &v }));
    let err = run_command(&spec(&["echo", "{unit.vars.x}"]), &c).unwrap_err();
    assert!(matches!(err, CommandFailure::Dash { .. }), "{err:?}");
    assert!(err.to_string().contains("starts with"), "{err}");
    // The unit ID counts too: it comes from a record.
    let v = vars(&[]);
    let c = ctx(
        &p,
        Some(UnitView {
            id: "-rf",
            vars: &v,
        }),
    );
    assert!(matches!(
        run_command(&spec(&["echo", "{unit.id}"]), &c).unwrap_err(),
        CommandFailure::Dash { .. }
    ));
    // A dash inside an item, a literal option and a parameter are fine.
    let v = vars(&[("x", "-n")]);
    let c = ctx(&p, Some(UnitView { id: "u1", vars: &v }));
    let out = run_command(
        &spec(&[
            "printf",
            "%s|%s|%s",
            "a{unit.vars.x}",
            "-literal",
            "{param.dash}",
        ]),
        &c,
    )
    .unwrap();
    assert_eq!(stdout(&out), "a-n|-literal|-p");
}

#[test]
fn the_program_may_not_come_from_a_record() {
    let p = params();
    let v = vars(&[("x", "echo")]);
    let c = ctx(&p, Some(UnitView { id: "u1", vars: &v }));
    let err = run_command(&spec(&["{unit.vars.x}", "hi"]), &c).unwrap_err();
    assert!(err.to_string().contains("program"), "{err}");
}

#[test]
fn a_missing_variable_is_a_failure() {
    let p = params();
    let v = vars(&[]);
    let c = ctx(&p, Some(UnitView { id: "u1", vars: &v }));
    let err = run_command(&spec(&["echo", "{unit.vars.absent}"]), &c).unwrap_err();
    assert!(matches!(err, CommandFailure::Render(_)), "{err:?}");
    assert!(err.to_string().contains("absent"), "{err}");
}

// ------------------------------------------------------------------ parks, halts, errors

#[test]
fn a_dash_value_parks_the_unit_with_the_hook_place_and_program() {
    let e = running_unit(&[("x", "--oops")]);
    let (p, v) = (params(), vars(&[("x", "--oops")]));
    let c = unit_ctx(&e, &p, &v);
    let outcome = run_unit_hook(
        &e.engine,
        "u1",
        "before",
        &[spec(&["true"]), spec(&["echo", "{unit.vars.x}"])],
        &c,
    )
    .unwrap();
    let HookOutcome::Failed(reason) = outcome else {
        panic!("{outcome:?}")
    };
    assert!(
        reason.contains("before") && reason.contains("2") && reason.contains("echo"),
        "{reason}"
    );
    assert_eq!(parked_reason(&e), reason);
}

#[test]
fn a_non_zero_exit_parks_the_unit() {
    let e = running_unit(&[]);
    let (p, v) = (params(), vars(&[]));
    let c = unit_ctx(&e, &p, &v);
    let outcome = run_unit_hook(
        &e.engine,
        "u1",
        "before",
        &[spec(&["sh", "-c", "echo boom >&2; exit 3"])],
        &c,
    )
    .unwrap();
    let HookOutcome::Failed(reason) = outcome else {
        panic!("{outcome:?}")
    };
    assert!(
        reason.contains("before") && reason.contains("1") && reason.contains("sh"),
        "{reason}"
    );
    assert!(reason.contains("exited with code 3"), "{reason}");
    assert!(
        reason.contains("boom"),
        "the first line of stderr helps: {reason}"
    );
    assert!(!reason.contains('\n'));
    assert_eq!(parked_reason(&e), reason);
}

#[test]
fn a_missing_program_parks_the_unit() {
    let e = running_unit(&[]);
    let (p, v) = (params(), vars(&[]));
    let c = unit_ctx(&e, &p, &v);
    let outcome = run_unit_hook(
        &e.engine,
        "u1",
        "after_first_turn",
        &[spec(&["definitely-not-a-program-xyz"])],
        &c,
    )
    .unwrap();
    let HookOutcome::Failed(reason) = outcome else {
        panic!("{outcome:?}")
    };
    assert!(
        reason.contains("after_first_turn") && reason.contains("definitely-not-a-program-xyz"),
        "{reason}"
    );
    assert!(reason.contains("could not start"), "{reason}");
    assert_eq!(parked_reason(&e), reason);
}

#[test]
fn a_record_that_lacks_a_variable_parks_the_unit() {
    let e = running_unit(&[]);
    let (p, v) = (params(), vars(&[]));
    let c = unit_ctx(&e, &p, &v);
    let outcome = run_unit_hook(
        &e.engine,
        "u1",
        "before",
        &[spec(&["echo", "{unit.vars.absent}"])],
        &c,
    )
    .unwrap();
    let HookOutcome::Failed(reason) = outcome else {
        panic!("{outcome:?}")
    };
    assert!(reason.contains("absent"), "{reason}");
    assert_eq!(e.status_of("u1").0, Status::Parked);
}

#[test]
fn a_hook_that_passes_leaves_the_unit_running() {
    let e = running_unit(&[]);
    let (p, v) = (params(), vars(&[]));
    let c = unit_ctx(&e, &p, &v);
    let outcome = run_unit_hook(
        &e.engine,
        "u1",
        "before",
        &[spec(&["true"]), spec(&["echo", "ok"])],
        &c,
    )
    .unwrap();
    assert_eq!(outcome, HookOutcome::Passed);
    assert_eq!(e.status_of("u1").0, Status::Running);
}

#[test]
fn a_hook_stops_at_the_first_failure_and_nothing_retries() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("log");
    let log = log.display().to_string();
    let e = running_unit(&[]);
    let (p, v) = (params(), vars(&[]));
    let c = unit_ctx(&e, &p, &v);
    let outcome = run_unit_hook(
        &e.engine,
        "u1",
        "before",
        &[
            spec(&["sh", "-c", "echo first >> \"$1\"; exit 1", "sh", &log]),
            spec(&["sh", "-c", "echo second >> \"$1\"", "sh", &log]),
        ],
        &c,
    )
    .unwrap();
    assert!(matches!(outcome, HookOutcome::Failed(_)));
    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        "first\n",
        "the second command did not run, and the first ran once"
    );
}

#[test]
fn a_failed_after_merge_halts_the_run() {
    let e = running_unit(&[]);
    let (p, v) = (params(), vars(&[]));
    let c = unit_ctx(&e, &p, &v);
    let outcome =
        run_after_merge(&e.engine, "u1", &[spec(&["true"]), spec(&["false"])], &c).unwrap();
    let HookOutcome::Failed(reason) = outcome else {
        panic!("{outcome:?}")
    };
    assert!(
        reason.contains("after_merge") && reason.contains("2") && reason.contains("false"),
        "{reason}"
    );
    let halted = e.engine.view().unwrap().run.unwrap().halted_reason;
    assert_eq!(halted.as_deref(), Some(reason.as_str()));
    // The unit is not parked by it: it is already done in a real run.
    assert_eq!(e.status_of("u1").0, Status::Running);
}

#[test]
fn a_dash_or_missing_variable_in_after_merge_halts_the_run_too() {
    for (pairs, template) in [
        (&[("x", "-bad")][..], "{unit.vars.x}"),
        (&[][..], "{unit.vars.absent}"),
    ] {
        let e = running_unit(pairs);
        let (p, v) = (params(), vars(pairs));
        let c = unit_ctx(&e, &p, &v);
        let outcome = run_after_merge(&e.engine, "u1", &[spec(&["echo", template])], &c).unwrap();
        assert!(matches!(outcome, HookOutcome::Failed(_)), "{outcome:?}");
        assert!(
            e.engine
                .view()
                .unwrap()
                .run
                .unwrap()
                .halted_reason
                .is_some()
        );
    }
}

#[test]
fn a_passing_after_merge_changes_nothing() {
    let e = running_unit(&[]);
    let (p, v) = (params(), vars(&[]));
    let c = unit_ctx(&e, &p, &v);
    assert_eq!(
        run_after_merge(&e.engine, "u1", &[spec(&["true"])], &c).unwrap(),
        HookOutcome::Passed
    );
    assert!(
        e.engine
            .view()
            .unwrap()
            .run
            .unwrap()
            .halted_reason
            .is_none()
    );
}

#[test]
fn plan_check_returns_a_count_or_an_error_with_no_unit() {
    let p = params();
    let c = ctx(&p, None);
    assert_eq!(run_plan_check(&spec(&["printf", "7\n"]), &c).unwrap(), 7);
    assert_eq!(
        run_plan_check(&spec(&["printf", "  12  \n\n"]), &c).unwrap(),
        12
    );
    assert_eq!(run_plan_check(&spec(&["printf", "0"]), &c).unwrap(), 0);
    for bad in [
        "seven",
        "3 items",
        "-2",
        "1.5",
        "",
        "4\n5\n",
        "99999999999999999999999",
    ] {
        let err = run_plan_check(&spec(&["printf", "%s", bad]), &c).unwrap_err();
        assert!(err.to_string().contains("plan_check"), "{bad:?}: {err}");
    }
    let err = run_plan_check(&spec(&["false"]), &c).unwrap_err();
    assert!(
        err.to_string().contains("plan_check") && err.to_string().contains("false"),
        "{err}"
    );
    let err = run_plan_check(&spec(&["definitely-not-a-program-xyz"]), &c).unwrap_err();
    assert!(err.to_string().contains("could not start"), "{err}");
    // It has no unit: a unit placeholder is an error.
    assert!(run_plan_check(&spec(&["echo", "{unit.id}"]), &c).is_err());
    // A parameter is fine.
    assert_eq!(
        run_plan_check(&spec(&["printf", "%s", "{param.count}"]), &c).unwrap(),
        5
    );
}

// ------------------------------------------------------------------ bounds

#[test]
fn a_sleeping_command_is_killed_at_its_timeout_and_the_unit_parks() {
    let e = running_unit(&[]);
    let (p, v) = (params(), vars(&[]));
    let c = unit_ctx(&e, &p, &v);
    let started = Instant::now();
    let outcome = run_unit_hook(
        &e.engine,
        "u1",
        "before",
        &[with_timeout(&["sleep", "30"], 1)],
        &c,
    )
    .unwrap();
    let took = started.elapsed();
    assert!(took >= Duration::from_secs(1), "{took:?}");
    assert!(
        took < Duration::from_secs(10),
        "the runner waited for the sleep: {took:?}"
    );
    let HookOutcome::Failed(reason) = outcome else {
        panic!("{outcome:?}")
    };
    assert!(
        reason.contains("before") && reason.contains("1") && reason.contains("sleep"),
        "{reason}"
    );
    assert!(reason.contains("timed out after 1"), "{reason}");
    assert_eq!(parked_reason(&e), reason);
}

#[test]
fn a_timed_out_process_is_killed() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("late");
    let p = params();
    let c = ctx(&p, None);
    let started = Instant::now();
    // The shell sleeps, then writes the marker. If the kill missed it, the marker appears.
    let script = format!("sleep 2; touch {}", marker.display());
    let err = run_command(&with_timeout(&["sh", "-c", &script], 1), &c).unwrap_err();
    assert!(
        matches!(err, CommandFailure::Timeout { seconds: 1 }),
        "{err:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    std::thread::sleep(Duration::from_millis(2500));
    assert!(!marker.exists(), "the timed-out shell was still running");
}

#[test]
fn a_command_that_prints_over_the_limit_is_cut_and_does_not_hang() {
    let p = params();
    let c = ctx(&p, None);
    let started = Instant::now();
    let out = run_command(&spec(&["head", "-c", "300000", "/dev/zero"]), &c).unwrap();
    assert_eq!(out.stdout.len(), MAX_OUTPUT_BYTES);
    assert!(out.stdout_cut);
    assert_eq!(out.exit_code, Some(0));
    // The same on stderr, and a command that fills both.
    let out = run_command(
        &spec(&[
            "sh",
            "-c",
            "head -c 200000 /dev/zero >&2; head -c 100000 /dev/zero",
        ]),
        &c,
    )
    .unwrap();
    assert_eq!(out.stderr.len(), MAX_OUTPUT_BYTES);
    assert!(out.stderr_cut);
    assert_eq!(out.stdout.len(), MAX_OUTPUT_BYTES);
    assert!(out.stdout_cut);
    // A command under the limit is not marked cut.
    let out = run_command(&spec(&["printf", "abc"]), &c).unwrap();
    assert!(!out.stdout_cut && !out.stderr_cut);
    assert!(started.elapsed() < Duration::from_secs(20));
}

#[test]
fn a_grandchild_that_holds_the_pipe_does_not_hang_the_runner() {
    let p = params();
    let c = ctx(&p, None);
    let started = Instant::now();
    // The shell exits at once. The background sleep keeps the pipe open for 3 seconds.
    let out = run_command(&spec(&["sh", "-c", "sleep 3 & echo hi"]), &c).unwrap();
    assert_eq!(stdout(&out), "hi\n");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn the_bounds_are_the_ones_the_spec_names() {
    assert_eq!(DEFAULT_TIMEOUT_SECONDS, 60);
    assert_eq!(MAX_TIMEOUT_SECONDS, 600);
    assert_eq!(MAX_OUTPUT_BYTES, 64 * 1024);
    assert_eq!(CommandSpec::new(&["true"]).timeout_seconds, 60);
    // A timeout over the limit does not run.
    let p = params();
    let c = ctx(&p, None);
    assert!(run_command(&with_timeout(&["true"], 601), &c).is_err());
    assert!(run_command(&with_timeout(&["true"], 0), &c).is_err());
}

// ------------------------------------------------------------------ exit0

#[test]
fn an_exit0_signal_treats_a_non_zero_exit_as_not_yet() {
    let p = params();
    let c = ctx(&p, None);
    assert!(exit0(&spec(&["true"]), &c).unwrap());
    assert!(!exit0(&spec(&["false"]), &c).unwrap());
    assert!(!exit0(&spec(&["sh", "-c", "exit 7"]), &c).unwrap());
    // The other failures are still failures.
    assert!(exit0(&spec(&["definitely-not-a-program-xyz"]), &c).is_err());
    assert!(matches!(
        exit0(&with_timeout(&["sleep", "30"], 1), &c).unwrap_err(),
        CommandFailure::Timeout { .. }
    ));
    let v = vars(&[("x", "-n")]);
    let c = ctx(&p, Some(UnitView { id: "u1", vars: &v }));
    assert!(matches!(
        exit0(&spec(&["echo", "{unit.vars.x}"]), &c).unwrap_err(),
        CommandFailure::Dash { .. }
    ));
}

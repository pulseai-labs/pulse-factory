//! AC-8: launch settings. `start_stage` renders each setting of the stage from the unit's
//! variables and the run's parameters, and carries the role's runtime into the launch plan. A
//! working directory that comes from a unit record must lie under a declared root.

mod common;

use std::collections::BTreeMap;
use std::os::unix::fs::symlink;
use std::path::Path;

use common::{Env, TrackedSource, env_with_params, unit};
use pf_engine::capacity::Settings;
use pf_engine::launch::LaunchStep;
use pf_engine::{MergeRule, StageDef, Status, Workflow};

fn settings(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn workflow(stage_settings: &[(&str, &str)], roots: &[&str]) -> Workflow {
    let mut stage = StageDef::new("build", "builder", "model-a");
    stage.runtime = "omp".to_string();
    stage.settings = settings(stage_settings);
    let mut wf = Workflow::single_track(
        "settings-flow",
        vec![stage],
        Some(MergeRule::GreenChecksInDeclaredOrder),
        Settings::default(),
    );
    wf.roots = roots.iter().map(|r| r.to_string()).collect();
    wf
}

/// An engine whose parameter `root` is `root`, with one unit `u1` whose record holds `vars`.
fn run(wf: Workflow, root: &Path, vars: &[(&str, &str)]) -> Env {
    let params = settings(&[("root", &root.display().to_string()), ("mode", "fast")]);
    let e = env_with_params(wf, params);
    let mut record = unit("u1", "g1", 1);
    record.vars = settings(vars);
    e.engine
        .ingest(&mut TrackedSource {
            track: "main".into(),
            records: vec![record],
        })
        .unwrap();
    e
}

fn parked(step: LaunchStep) -> String {
    match step {
        LaunchStep::Parked { reason } => reason,
        other => panic!("the launch must park: {other:?}"),
    }
}

#[test]
fn each_setting_is_rendered_from_the_unit_and_the_run() {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let wf = workflow(
        &[
            ("cwd", "{unit.vars.dir}"),
            (
                "message",
                "Do {unit.id} in {run.id} at {run.dir} in mode {param.mode} with {unit.vars.brief}.",
            ),
            ("env.OUT_FILE", "{run.dir}/out/{unit.id}/stop"),
            ("brief", "briefs/common.md,briefs/build.md"),
            ("skills", "alpha,beta"),
            ("first_turn_seconds", "120"),
            ("stall_minutes", "15"),
            ("push_dir", "{unit.vars.dir}"),
            ("push_branch", "work/{unit.id}"),
        ],
        &["{param.root}"],
    );
    let e = run(
        wf,
        root.path(),
        &[("dir", &work.display().to_string()), ("brief", "tidy up")],
    );
    assert!(matches!(
        e.engine.start_stage("u1").unwrap(),
        LaunchStep::Launched { .. }
    ));
    let plan = e.agent.launches.lock().unwrap()[0].clone();
    let run_dir = e.dir.path().join("run").display().to_string();
    assert_eq!(plan.settings["cwd"], work.display().to_string());
    assert_eq!(
        plan.settings["message"],
        format!("Do u1 in run-1 at {run_dir} in mode fast with tidy up.")
    );
    assert_eq!(
        plan.settings["env.OUT_FILE"],
        format!("{run_dir}/out/u1/stop")
    );
    assert_eq!(plan.settings["brief"], "briefs/common.md,briefs/build.md");
    assert_eq!(plan.settings["skills"], "alpha,beta");
    assert_eq!(plan.settings["first_turn_seconds"], "120");
    assert_eq!(plan.settings["stall_minutes"], "15");
    assert_eq!(plan.settings["push_dir"], work.display().to_string());
    assert_eq!(plan.settings["push_branch"], "work/u1");
    // The adapter's own edits sit beside them.
    assert_eq!(plan.settings["rules"], "");
    assert_eq!(plan.settings["ask"], "off");
}

#[test]
fn the_runtime_of_the_role_reaches_the_plan() {
    let root = tempfile::tempdir().unwrap();
    let e = run(workflow(&[], &[]), root.path(), &[]);
    e.engine.start_stage("u1").unwrap();
    assert_eq!(e.agent.launches.lock().unwrap()[0].runtime, "omp");
    // A stage that names no runtime gets the default name.
    assert_eq!(StageDef::new("s", "r", "m").runtime, "default");
}

#[test]
fn a_record_that_lacks_a_variable_parks_the_unit() {
    let root = tempfile::tempdir().unwrap();
    let wf = workflow(&[("message", "Do {unit.vars.absent}")], &[]);
    let e = run(wf, root.path(), &[]);
    let reason = parked(e.engine.start_stage("u1").unwrap());
    assert!(
        reason.contains("message") && reason.contains("absent"),
        "{reason}"
    );
    assert_eq!(e.status_of("u1"), (Status::Parked, Some(reason)));
    assert_eq!(e.agent.launch_count(), 0);
    // The stage attempt ended failed, so the unit holds no running session.
    let view = e.engine.view().unwrap();
    assert_eq!(view.stages_of("u1")[0].status, Status::Failed);
    assert!(view.sessions.iter().all(|s| s.ended));
}

#[test]
fn a_parameter_the_run_lacks_parks_the_unit() {
    let root = tempfile::tempdir().unwrap();
    let wf = workflow(&[("message", "Mode {param.nope}")], &[]);
    let e = run(wf, root.path(), &[]);
    let reason = parked(e.engine.start_stage("u1").unwrap());
    assert!(reason.contains("nope"), "{reason}");
    assert_eq!(e.agent.launch_count(), 0);
}

fn cwd_case(make: impl Fn(&Path, &Path) -> String) -> (Env, String, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    let inside = root.path().join("inside");
    std::fs::create_dir(&inside).unwrap();
    let outside = root.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    // The root is `inside`.
    let wf = workflow(&[("cwd", "{unit.vars.dir}")], &["{param.root}/inside"]);
    let dir = make(&inside, &outside);
    let e = run(wf, root.path(), &[("dir", &dir)]);
    (e, dir, root)
}

#[test]
fn a_cwd_under_a_declared_root_launches() {
    let (e, dir, _root) = cwd_case(|inside, _| {
        let sub = inside.join("item");
        std::fs::create_dir(&sub).unwrap();
        sub.display().to_string()
    });
    assert!(matches!(
        e.engine.start_stage("u1").unwrap(),
        LaunchStep::Launched { .. }
    ));
    assert_eq!(e.agent.launches.lock().unwrap()[0].settings["cwd"], dir);
    // The root itself is allowed.
    let (e, _, _root) = cwd_case(|inside, _| inside.display().to_string());
    assert!(matches!(
        e.engine.start_stage("u1").unwrap(),
        LaunchStep::Launched { .. }
    ));
}

#[test]
fn a_cwd_outside_the_roots_parks_the_unit_and_no_launch_reaches_the_agent() {
    let (e, _, _root) = cwd_case(|_, outside| outside.display().to_string());
    let reason = parked(e.engine.start_stage("u1").unwrap());
    assert!(
        reason.contains("cwd") && reason.contains("outside"),
        "{reason}"
    );
    assert_eq!(e.agent.launch_count(), 0);
    assert_eq!(e.status_of("u1").0, Status::Parked);
}

#[test]
fn a_relative_cwd_parks_the_unit() {
    let (e, _, _root) = cwd_case(|_, _| "relative/dir".to_string());
    let reason = parked(e.engine.start_stage("u1").unwrap());
    assert!(
        reason.contains("cwd") && reason.contains("absolute"),
        "{reason}"
    );
    assert_eq!(e.agent.launch_count(), 0);
}

#[test]
fn a_cwd_that_does_not_exist_parks_the_unit() {
    let (e, _, _root) = cwd_case(|inside, _| inside.join("missing").display().to_string());
    let reason = parked(e.engine.start_stage("u1").unwrap());
    assert!(
        reason.contains("cwd") && reason.contains("does not exist"),
        "{reason}"
    );
    assert_eq!(e.agent.launch_count(), 0);
}

#[test]
fn a_symlink_that_leads_outside_the_roots_parks_the_unit() {
    let (e, _, _root) = cwd_case(|inside, outside| {
        let link = inside.join("link");
        symlink(outside, &link).unwrap();
        link.display().to_string()
    });
    let reason = parked(e.engine.start_stage("u1").unwrap());
    assert!(reason.contains("outside"), "{reason}");
    assert_eq!(e.agent.launch_count(), 0);
}

#[test]
fn a_path_with_dotdot_that_escapes_the_roots_parks_the_unit() {
    let (e, _, _root) = cwd_case(|inside, _| format!("{}/../outside", inside.display()));
    let reason = parked(e.engine.start_stage("u1").unwrap());
    assert!(reason.contains("outside"), "{reason}");
    assert_eq!(e.agent.launch_count(), 0);
}

#[test]
fn a_file_is_not_a_working_directory() {
    let (e, _, _root) = cwd_case(|inside, _| {
        let f = inside.join("file.txt");
        std::fs::write(&f, "x").unwrap();
        f.display().to_string()
    });
    let reason = parked(e.engine.start_stage("u1").unwrap());
    assert!(reason.contains("not a directory"), "{reason}");
}

#[test]
fn with_no_declared_root_a_cwd_from_a_record_parks_the_unit() {
    let root = tempfile::tempdir().unwrap();
    let wf = workflow(&[("cwd", "{unit.vars.dir}")], &[]);
    let e = run(
        wf,
        root.path(),
        &[("dir", &root.path().display().to_string())],
    );
    let reason = parked(e.engine.start_stage("u1").unwrap());
    assert!(reason.contains("no root"), "{reason}");
}

#[test]
fn a_push_dir_from_a_record_passes_the_same_check() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let wf = workflow(&[("push_dir", "{unit.vars.dir}")], &["{param.root}"]);
    let e = run(
        wf,
        root.path(),
        &[("dir", &outside.path().display().to_string())],
    );
    let reason = parked(e.engine.start_stage("u1").unwrap());
    assert!(
        reason.contains("push_dir") && reason.contains("outside"),
        "{reason}"
    );
    assert_eq!(e.agent.launch_count(), 0);
}

#[test]
fn a_cwd_that_does_not_come_from_a_record_is_not_checked() {
    // The file author wrote the path. Only a record's path needs the roots.
    let root = tempfile::tempdir().unwrap();
    let wf = workflow(&[("cwd", "{param.root}")], &[]);
    let e = run(wf, root.path(), &[]);
    assert!(matches!(
        e.engine.start_stage("u1").unwrap(),
        LaunchStep::Launched { .. }
    ));
    let wf = workflow(&[("cwd", "/not/a/real/dir")], &[]);
    let e = run(wf, root.path(), &[]);
    assert!(matches!(
        e.engine.start_stage("u1").unwrap(),
        LaunchStep::Launched { .. }
    ));
}

#[test]
fn a_halted_run_still_refuses_the_launch_before_it_renders_anything() {
    let root = tempfile::tempdir().unwrap();
    let wf = workflow(&[("message", "Do {unit.vars.absent}")], &[]);
    let e = run(wf, root.path(), &[]);
    e.engine.halt("stop everything").unwrap();
    match e.engine.start_stage("u1").unwrap() {
        LaunchStep::Refused { reason } => assert_eq!(reason, "run halted"),
        other => panic!("{other:?}"),
    }
    assert_eq!(e.status_of("u1").0, Status::Running);
}

//! The parsed file becomes the value the engine runs. This test converts the real package file,
//! checks the tracks, stages and merge definition, and then runs a real engine on fake ports to
//! see `start_stage` render the settings from a record and the run's parameters.

#[path = "../../pf-engine/tests/common/mod.rs"]
mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use common::{Env, TrackedSource, env_with_params, unit};
use pf_engine::launch::LaunchStep;
use pf_engine::{MergeRule, Status, Workflow};
use pf_workflow::WorkflowFile;

fn package_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../workflows/ossify-spine")
}

fn converted() -> (WorkflowFile, Workflow) {
    let text = std::fs::read_to_string(package_dir().join("workflow.yaml")).unwrap();
    let file = WorkflowFile::parse(&text).unwrap_or_else(|e| panic!("{e}"));
    let wf = file.to_workflow();
    (file, wf)
}

#[test]
fn the_tracks_stages_and_runtime_come_over() {
    let (file, wf) = converted();
    assert_eq!(wf.name, "ossify-spine");
    assert_eq!(wf.roots, file.roots);
    assert_eq!(wf.settings, file.settings);
    let names: Vec<&str> = wf.tracks.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["lane", "item"]);

    let item = wf.track("item").unwrap();
    let stages: Vec<&str> = item.stages.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(stages, ["implement", "verify", "close"]);
    let roles: Vec<&str> = item.stages.iter().map(|s| s.role.as_str()).collect();
    assert_eq!(roles, ["implementer", "verifier", "close"]);
    for stage in wf.tracks.iter().flat_map(|t| &t.stages) {
        assert_eq!(stage.runtime, "omp", "{}", stage.name);
        assert!(!stage.model.is_empty(), "{}", stage.name);
        assert_eq!(stage.rules.name, stage.role);
        assert!(stage.rules.prompts_off);
        assert!(stage.rules.deny.is_empty());
    }
}

#[test]
fn only_the_item_track_merges_and_it_checks_the_head() {
    let (_, wf) = converted();
    assert!(wf.track("lane").unwrap().merge.is_none());
    let merge = wf
        .track("item")
        .unwrap()
        .merge
        .as_ref()
        .expect("item merges");
    assert_eq!(merge.rule, MergeRule::GreenChecksInDeclaredOrder);
    assert!(merge.head_check);
    // `merge.repo` is the repo on the forge. `{param.repo}` stays the local path for `git -C`.
    assert_eq!(merge.repo, "{param.forge_repo}");
    assert_eq!(merge.base, "{param.spine_branch}");
    assert!(wf.has_merge());
}

#[test]
fn the_settings_hold_the_stage_fields_and_the_settings_map() {
    let (file, wf) = converted();
    let implement = &wf.track("item").unwrap().stages[0];
    let s = &implement.settings;
    assert_eq!(s["cwd"], "{unit.vars.worktree_path}");
    assert_eq!(s["brief"], "briefs/common.md,briefs/implement.md");
    assert_eq!(s["skills"], "work-item");
    assert_eq!(s["first_turn_seconds"], "180");
    assert_eq!(s["stall_minutes"], "20");
    assert!(s["message"].contains("{unit.vars.handoff_path}"));
    assert!(s.contains_key("env.SPINE_STOP_FILE"));
    // Every key of the stage's `settings` map came over.
    let spec = &file.track("item").unwrap().stages[0];
    for (key, value) in &spec.settings {
        assert_eq!(&s[key], value);
    }
    // `push_dir` and `push_branch` belong to the close stage only.
    let close = &wf.track("item").unwrap().stages[2];
    assert_eq!(close.settings["push_dir"], "{unit.vars.worktree_path}");
    assert_eq!(close.settings["push_branch"], "{unit.vars.branch}");
    assert!(!s.contains_key("push_dir"));
}

#[test]
fn each_stage_carries_the_run_facts_its_seat_needs() {
    let (_, wf) = converted();
    let lane = &wf.track("lane").unwrap().stages[0];
    assert_eq!(lane.settings["report_dir"], "{param.spine_dir}");
    assert_eq!(
        lane.settings["decisions_file"],
        "{run.dir}/out/{unit.id}/decisions.md"
    );
    // A lane has no worktree and no branch of its own.
    assert!(!lane.settings.contains_key("worktree"));
    assert!(!lane.settings.contains_key("own_branch"));
    for stage in &wf.track("item").unwrap().stages {
        let s = &stage.settings;
        assert_eq!(
            s["report_dir"], "{param.spine_dir}/work-{unit.id}",
            "{}",
            stage.name
        );
        assert_eq!(s["worktree"], "{unit.vars.worktree_path}", "{}", stage.name);
        assert_eq!(s["own_branch"], "{unit.vars.branch}", "{}", stage.name);
    }
    // The decisions file is set where the stage's env names one.
    for name in ["implement", "close"] {
        let stage = wf
            .track("item")
            .unwrap()
            .stages
            .iter()
            .find(|s| s.name == name)
            .unwrap();
        assert_eq!(
            stage.settings["env.SPINE_DECISIONS"],
            stage.settings["decisions_file"]
        );
    }
    let verify = &wf.track("item").unwrap().stages[1];
    assert!(!verify.settings.contains_key("decisions_file"));
}

/// The parameters `arm` supplies, with real directories under `base`.
fn params(base: &Path) -> BTreeMap<String, String> {
    let dir = |name: &str| {
        let p = base.join(name);
        std::fs::create_dir_all(&p).unwrap();
        p.display().to_string()
    };
    let mut p = BTreeMap::new();
    p.insert("spine".to_string(), "s1".to_string());
    p.insert("spine_branch".to_string(), "spine/s1".to_string());
    p.insert("spine_dir".to_string(), dir("spec"));
    p.insert("method_root".to_string(), dir("method"));
    p.insert("repo".to_string(), "owner/repo".to_string());
    p.insert("worktree_root".to_string(), dir("worktrees"));
    p.insert(
        "state_file".to_string(),
        base.join("state.json").display().to_string(),
    );
    p.insert("oss".to_string(), "tool".to_string());
    p.insert("briefs".to_string(), dir("briefs"));
    p
}

fn item_record(id: &str, worktree: &str) -> pf_engine::port::unit_source::UnitRecord {
    let mut record = unit(id, "g1", 1);
    record.vars = [
        ("worktree_path", worktree),
        ("branch", "work/x"),
        ("handoff_path", "/specs/work-x/handoff.md"),
        ("base_sha", "abc123"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    record
}

fn engine(base: &Path) -> Env {
    let (_, wf) = converted();
    env_with_params(wf, params(base))
}

#[test]
fn a_real_engine_renders_the_settings_of_an_item_unit() {
    let base = tempfile::tempdir().unwrap();
    let e = engine(base.path());
    let worktree = base.path().join("worktrees").join("x");
    std::fs::create_dir_all(&worktree).unwrap();
    let wt = worktree.display().to_string();
    e.engine
        .ingest(&mut TrackedSource {
            track: "item".to_string(),
            records: vec![item_record("x", &wt)],
        })
        .unwrap();
    assert!(matches!(
        e.engine.start_stage("x").unwrap(),
        LaunchStep::Launched { .. }
    ));

    let plan = e.agent.launches.lock().unwrap()[0].clone();
    assert_eq!(plan.runtime, "omp");
    assert_eq!(plan.stage, "implement");
    assert_eq!(plan.role, "implementer");
    assert_eq!(plan.settings["rules"], "implementer");
    assert_eq!(plan.settings["cwd"], wt);
    assert_eq!(
        plan.settings["brief"],
        "briefs/common.md,briefs/implement.md"
    );
    let run_dir = e.dir.path().join("run").display().to_string();
    let spec_dir = base.path().join("spec").display().to_string();
    let message = &plan.settings["message"];
    assert!(message.contains("/specs/work-x/handoff.md"), "{message}");
    assert!(
        message.contains(&format!("{spec_dir}/work-x/report.md")),
        "{message}"
    );
    assert!(
        message.contains(&format!("{run_dir}/out/x/gaps.json")),
        "{message}"
    );
    assert!(!message.contains("{unit."), "{message}");
    assert_eq!(
        plan.settings["env.SPINE_STOP_FILE"],
        format!("{run_dir}/out/x/stop")
    );
}

#[test]
fn a_record_cwd_outside_the_roots_parks_the_item_and_no_launch_follows() {
    let base = tempfile::tempdir().unwrap();
    let e = engine(base.path());
    let outside = base.path().join("elsewhere");
    std::fs::create_dir_all(&outside).unwrap();
    e.engine
        .ingest(&mut TrackedSource {
            track: "item".to_string(),
            records: vec![item_record("x", &outside.display().to_string())],
        })
        .unwrap();
    let step = e.engine.start_stage("x").unwrap();
    let LaunchStep::Parked { reason } = step else {
        panic!("the launch must park: {step:?}");
    };
    assert!(
        reason.contains("cwd") && reason.contains("roots"),
        "{reason}"
    );
    assert_eq!(e.status_of("x").0, Status::Parked);
    assert_eq!(e.agent.launch_count(), 0);
}

#[test]
fn a_lane_unit_runs_in_the_method_root_from_the_parameters() {
    let base = tempfile::tempdir().unwrap();
    let e = engine(base.path());
    let mut lane = unit("lane", "lane", 1);
    lane.vars.clear();
    e.engine
        .ingest(&mut TrackedSource {
            track: "lane".to_string(),
            records: vec![lane],
        })
        .unwrap();
    assert!(matches!(
        e.engine.start_stage("lane").unwrap(),
        LaunchStep::Launched { .. }
    ));
    let plan = e.agent.launches.lock().unwrap()[0].clone();
    assert_eq!(plan.stage, "lane");
    assert_eq!(
        plan.settings["cwd"],
        base.path().join("method").display().to_string()
    );
    assert_eq!(plan.settings["skills"], "run-spine");
}

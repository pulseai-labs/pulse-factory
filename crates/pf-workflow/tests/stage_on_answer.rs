//! AC-3 of `r2.s11.w2`: a stage declares a fallback answer hook. `on_answer` on a stage is a command
//! list in the answer scope. It runs for a park of the stage that no hook of its own file covers, so
//! a text typed on any park of that stage reaches the next seat as a note. The new placeholder
//! `{answer.source}` says whether a person typed the answer or took an option, and who wrote it.

#[path = "../../pf-engine/tests/common/mod.rs"]
mod common;

use std::collections::BTreeMap;
use std::path::Path;

use common::{Env, TrackedSource, env, item_workflow, unit};
use pf_engine::Cause;
use pf_engine::claim::{AnswerAct, sha256_hex};
use pf_engine::launch::LaunchStep;
use pf_engine::port::store::{RequestKind, RequestRow};
use pf_engine::render::{RenderContext, UnitView, render};
use pf_engine::{EndStatus, Status};
use pf_workflow::WorkflowFile;
use pf_workflow::commands::{AnswerOutcome, CommandSpec, answer_through_hook};
use pf_workflow::package::Package;

const BASE: &str = r#"version: 1
name: demo-flow
params: [repo, briefs]
roles:
  worker:
    runtime: omp
    model: small-model
    rules: rules/worker.yaml
unit_sources:
  - dir: "{run.dir}/queue"
    track: item
    vars: [handoff_path]
tracks:
  item:
    stages:
      - name: build
        role: worker
        brief: [briefs/item.md]
        message: "Build {unit.vars.handoff_path}."
        cwd: "{param.repo}"
        stall_minutes: 15
        first_turn_seconds: 180
        stop_file: "{run.dir}/out/{unit.id}/stop"
@FALLBACK@
        done:
          - file: "{run.dir}/out/{unit.id}/report.md"
      - name: check
        role: worker
        brief: [briefs/item.md]
        message: "Check {unit.vars.handoff_path}."
        cwd: "{param.repo}"
        stall_minutes: 15
        first_turn_seconds: 180
        stop_file: "{run.dir}/out/{unit.id}/stop"
        done:
          - file: "{run.dir}/out/{unit.id}/checked.md"
"#;

const FALLBACK: &str = r#"        on_answer:
          - [sh, "{run.package}/scripts/note.sh", "{run.dir}/out/{unit.id}/rulings.md", "-", "{answer.file}", "{answer.request}", "{answer.generation}", "build", "{answer.source}"]"#;

fn flow(fallback: &str) -> String {
    BASE.replace("@FALLBACK@", fallback)
}

fn parse(text: &str) -> Result<WorkflowFile, String> {
    WorkflowFile::parse(text).map_err(|e| e.to_string())
}

fn refused(text: &str) -> String {
    match parse(text) {
        Ok(_) => panic!("the file should be refused:\n{text}"),
        Err(e) => e,
    }
}

#[test]
fn a_stage_level_on_answer_parses_in_the_answer_scope() {
    let file = parse(&flow(FALLBACK)).unwrap_or_else(|e| panic!("{e}"));
    let stages = &file.track("item").unwrap().stages;
    assert_eq!(stages[0].on_answer.len(), 1);
    let argv = &stages[0].on_answer[0].argv;
    assert_eq!(argv.len(), 9);
    assert_eq!(argv[8], "{answer.source}");
    // The second stage declares none, and the stop file is a plain path as before.
    assert!(stages[1].on_answer.is_empty());
    assert!(stages[0].stop_on_answer.is_empty());
    // An empty list declares no fallback.
    let empty = parse(&flow("        on_answer: []")).unwrap();
    assert!(empty.track("item").unwrap().stages[0].on_answer.is_empty());
}

#[test]
fn a_stage_level_on_answer_is_still_refused_on_the_track_and_at_the_top() {
    let top = flow("").replace("name: demo-flow", "name: demo-flow\non_answer: [[true]]");
    assert!(refused(&top).contains("unknown field `on_answer`"));
    let track = flow("").replace(
        "  item:\n    stages:",
        "  item:\n    on_answer: [[true]]\n    stages:",
    );
    assert!(refused(&track).contains("unknown field `on_answer`"));
    // A fallback is a list of commands, not one command.
    let err = refused(&flow("        on_answer: \"true\""));
    assert!(err.contains("on_answer"), "{err}");
}

#[test]
fn the_answer_source_placeholder_is_allowed_in_an_answer_hook_only() {
    for place in [
        "        before:\n          - [echo, \"{answer.source}\"]",
        "        after_first_turn:\n          - [echo, \"{answer.source}\"]",
    ] {
        let err = refused(&flow(place));
        assert!(
            err.contains("answer.source") && err.contains("only in an `on_answer` command"),
            "{err}"
        );
    }
    let in_message = flow("").replace("Check {unit.vars.handoff_path}.", "Source {answer.source}.");
    assert!(refused(&in_message).contains("answer.source"));
    // The engine's render refuses it outside an answer hook too.
    let params = BTreeMap::new();
    let ctx = RenderContext {
        run_id: "run-1",
        run_dir: "/run",
        params: &params,
        unit: None,
    };
    let err = render("{answer.source}", &ctx).unwrap_err().to_string();
    assert!(err.contains("only in an `on_answer` command"), "{err}");
}

#[test]
fn to_workflow_sets_takes_text_only_for_a_stage_that_declares_a_fallback() {
    let file = parse(&flow(FALLBACK)).unwrap();
    let wf = file.to_workflow();
    let stages = &wf.track("item").unwrap().stages;
    assert!(stages[0].takes_text, "build declares a fallback");
    assert!(!stages[1].takes_text, "check declares none");
    // A stop file's own hook is not the stage's fallback.
    let stop_hook = flow("").replace(
        "        stop_file: \"{run.dir}/out/{unit.id}/stop\"\n        done:\n          - file: \"{run.dir}/out/{unit.id}/report.md\"",
        "        stop_file:\n          file: \"{run.dir}/out/{unit.id}/stop\"\n          on_answer:\n            - [true]\n        done:\n          - file: \"{run.dir}/out/{unit.id}/report.md\"",
    );
    let file = parse(&stop_hook).unwrap();
    assert!(!file.to_workflow().track("item").unwrap().stages[0].takes_text);
}

fn package(dir: &Path, workflow: &str, scripts: &[(&str, &str)]) {
    std::fs::create_dir_all(dir.join("briefs")).unwrap();
    std::fs::create_dir_all(dir.join("rules")).unwrap();
    std::fs::write(dir.join("workflow.yaml"), workflow).unwrap();
    std::fs::write(dir.join("briefs/item.md"), "# item\n").unwrap();
    std::fs::write(
        dir.join("rules/worker.yaml"),
        "version: 1\nrole: worker\nallow:\n  - delete_own_scratch\n",
    )
    .unwrap();
    for (name, text) in scripts {
        std::fs::create_dir_all(dir.join("scripts")).unwrap();
        std::fs::write(dir.join("scripts").join(name), text).unwrap();
    }
}

#[test]
fn a_fallback_that_runs_a_file_the_package_does_not_pin_is_refused_at_load() {
    let root = tempfile::tempdir().unwrap();
    let ok = root.path().join("ok");
    package(&ok, &flow(FALLBACK), &[("note.sh", "exit 0\n")]);
    Package::load(&ok).unwrap_or_else(|e| panic!("{e}"));

    let lacking = root.path().join("lacking");
    package(&lacking, &flow(FALLBACK), &[("other.sh", "exit 0\n")]);
    let err = Package::load(&lacking).unwrap_err().to_string();
    assert!(
        err.contains("scripts/note.sh") && err.contains("does not pin"),
        "{err}"
    );
    assert!(err.contains("stage `build`"), "{err}");
}

// ------------------------------------------------------------------ running the fallback

/// An engine with one unit `u1` parked at `build` by a stop file with two options. The seat's
/// session has ended, as it has when the driver parks the unit.
fn stopped() -> (Env, RequestRow, u64) {
    let e = env(item_workflow(&["build", "check"], false));
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
    e.engine
        .park_with(
            "u1",
            RequestKind::Stop,
            Cause::StopFile,
            "the seat wrote a stop file",
            Some(&serde_json::json!({
                "stop_text": "which way?",
                "stop_options": [
                    { "text": "go on, as planned", "recommended": true },
                    { "text": "stop here", "recommended": false },
                ],
            })),
        )
        .unwrap();
    e.engine
        .end("u1/build#1", EndStatus::Failed, "ended")
        .unwrap();
    let generation = e
        .engine
        .claim("op@host", &sha256_hex(b"t1"))
        .unwrap()
        .generation;
    let request = e.engine.requests().unwrap().remove(0);
    (e, request, generation)
}

fn run(
    e: &Env,
    request: &RequestRow,
    generation: u64,
    act: &AnswerAct,
    specs: &[CommandSpec],
    files_dir: &Path,
) -> AnswerOutcome {
    let params = BTreeMap::new();
    let vars = BTreeMap::new();
    let ctx = RenderContext {
        run_id: "run-1",
        run_dir: "/run/dir",
        params: &params,
        unit: Some(UnitView {
            id: "u1",
            vars: &vars,
        }),
    };
    answer_through_hook(
        &e.engine,
        request,
        request.revision,
        generation,
        act,
        specs,
        &ctx,
        files_dir,
    )
    .unwrap_or_else(|e| panic!("{e}"))
}

/// A hook that writes the source word and the text of the answer file where the test reads them.
fn recording_hook(work: &Path) -> Vec<CommandSpec> {
    let script = work.join("record.sh");
    std::fs::write(
        &script,
        "out=$1; source=$2; file=$3\nprintf '%s' \"$source\" > \"$out.source\"\ncat \"$file\" > \"$out.text\"\n",
    )
    .unwrap();
    let out = work.join("seen");
    vec![CommandSpec::new(&[
        "sh",
        script.to_str().unwrap(),
        out.to_str().unwrap(),
        "{answer.source}",
        "{answer.file}",
    ])]
}

#[test]
fn an_option_writes_its_text_into_the_answer_file_and_names_its_source() {
    let (e, request, generation) = stopped();
    let work = tempfile::tempdir().unwrap();
    let specs = recording_hook(work.path());
    let outcome = run(
        &e,
        &request,
        generation,
        &AnswerAct::Option("s1"),
        &specs,
        &work.path().join("answers"),
    );
    assert_eq!(outcome, AnswerOutcome::Resumed);
    assert_eq!(
        std::fs::read_to_string(work.path().join("seen.text")).unwrap(),
        "go on, as planned",
        "the answer file holds the option's text, not a caller's"
    );
    assert_eq!(
        std::fs::read_to_string(work.path().join("seen.source")).unwrap(),
        "option:s1:seat",
        "one word"
    );
    let node = e.store.node("run-1", "u1").unwrap();
    assert_eq!(node.status, Status::Running);
    let answer: serde_json::Value =
        serde_json::from_str(e.engine.requests().unwrap()[0].answer.as_deref().unwrap()).unwrap();
    assert_eq!(answer["option"]["id"], "s1");
    assert_eq!(answer["text"], "go on, as planned");
}

#[test]
fn a_typed_text_names_the_source_typed() {
    let (e, request, generation) = stopped();
    let work = tempfile::tempdir().unwrap();
    let specs = recording_hook(work.path());
    let outcome = run(
        &e,
        &request,
        generation,
        &AnswerAct::Text {
            text: "my own ruling",
            to_stage: None,
        },
        &specs,
        &work.path().join("answers"),
    );
    assert_eq!(outcome, AnswerOutcome::Resumed);
    assert_eq!(
        std::fs::read_to_string(work.path().join("seen.source")).unwrap(),
        "typed"
    );
    assert_eq!(
        std::fs::read_to_string(work.path().join("seen.text")).unwrap(),
        "my own ruling"
    );
}

#[test]
fn a_refused_option_runs_no_hook() {
    let (e, request, generation) = stopped();
    let work = tempfile::tempdir().unwrap();
    let specs = recording_hook(work.path());
    let params = BTreeMap::new();
    let vars = BTreeMap::new();
    let ctx = RenderContext {
        run_id: "run-1",
        run_dir: "/run/dir",
        params: &params,
        unit: Some(UnitView {
            id: "u1",
            vars: &vars,
        }),
    };
    let err = answer_through_hook(
        &e.engine,
        &request,
        request.revision,
        generation,
        &AnswerAct::Option("s9"),
        &specs,
        &ctx,
        &work.path().join("answers"),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("holds no option \"s9\""), "{err}");
    assert!(!work.path().join("seen.source").exists(), "no hook ran");
}

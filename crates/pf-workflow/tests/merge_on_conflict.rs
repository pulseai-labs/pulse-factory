//! AC-5 of `r2.s3.w3`: the workflow file reads a track's conflict stage (ADR-0004, amended). A track's
//! `merge` may declare `on_conflict: {stage, then}` and `guarded: [<glob>, ...]`, and a stage may be
//! `entered_by: conflict`. Each defect is refused with a message that names the track and the field.
//! A v1 file with none of these loads as before.

use pf_engine::MergeRule;
use pf_workflow::{ParseError, WorkflowFile};

/// A file with an `item` track of `build`, `review`, `fix` and the conflict stage `resolve`.
/// `{merge_extra}` goes at the end of the merge, `{resolve_extra}` at the end of `resolve`.
fn file(merge_extra: &str, resolve_extra: &str) -> String {
    format!(
        r#"version: 1
name: conflict-flow
params: [repo, forge_repo, base_branch]
roles:
  worker:
    runtime: omp
    model: small-model
    rules: rules/worker.yaml
unit_sources:
  - dir: "{{run.dir}}/queue"
    track: item
    vars: [branch]
tracks:
  item:
    stages:
      - name: build
        role: worker
        brief: briefs/item.md
        message: "Build {{unit.id}}."
        cwd: "{{param.repo}}"
        stall_minutes: 15
        first_turn_seconds: 180
        stop_file: "{{run.dir}}/out/{{unit.id}}/stop"
        done:
          - file: "{{run.dir}}/out/{{unit.id}}/build.done"
      - name: review
        role: worker
        brief: briefs/item.md
        message: "Review {{unit.id}}."
        cwd: "{{param.repo}}"
        stall_minutes: 15
        first_turn_seconds: 180
        stop_file: "{{run.dir}}/out/{{unit.id}}/stop"
        done:
          - file: "{{run.dir}}/out/{{unit.id}}/review.md"
      - name: resolve
        role: worker
        entered_by: conflict
        brief: briefs/item.md
        message: "Resolve {{unit.id}}."
        cwd: "{{param.repo}}"
        stall_minutes: 15
        first_turn_seconds: 180
        stop_file: "{{run.dir}}/out/{{unit.id}}/stop"
        done:
          - file: "{{run.dir}}/out/{{unit.id}}/resolve-done"
{resolve_extra}    merge:
      rule: green_checks_in_declared_order
      repo: "{{param.forge_repo}}"
      base: "{{param.base_branch}}"
      pr: [show-pr, "{{unit.id}}"]
{merge_extra}"#
    )
}

const CONFLICT: &str = "      on_conflict:\n        stage: resolve\n        then: review\n      guarded:\n        - \"docs/**\"\n        - \"**/tests/**\"\n";

fn refused(text: &str) -> ParseError {
    WorkflowFile::parse(text).expect_err("the file must be refused")
}

#[test]
fn a_file_with_on_conflict_and_guarded_loads_and_converts() {
    let parsed = WorkflowFile::parse(&file(CONFLICT, "")).unwrap_or_else(|e| panic!("{e}"));
    let item = parsed.track("item").unwrap();
    let merge = item.merge.as_ref().unwrap();
    let on = merge.on_conflict.as_ref().unwrap();
    assert_eq!((on.stage.as_str(), on.then.as_str()), ("resolve", "review"));
    assert_eq!(merge.guarded, ["docs/**", "**/tests/**"]);
    assert!(item.stages[2].entered_by_conflict);
    assert!(!item.stages[2].entered_by_outcome);

    let wf = parsed.to_workflow();
    let track = wf.track("item").unwrap();
    let def = track.merge.as_ref().unwrap();
    assert_eq!(def.rule, MergeRule::GreenChecksInDeclaredOrder);
    let on = def.on_conflict.as_ref().unwrap();
    assert_eq!((on.stage.as_str(), on.then.as_str()), ("resolve", "review"));
    assert_eq!(def.guarded, ["docs/**", "**/tests/**"]);
    assert!(track.stages[2].entered_by_conflict);
    assert_eq!(track.on_conflict(), Some(on));
}

#[test]
fn a_conflict_stage_is_passed_over_in_order() {
    let wf = WorkflowFile::parse(&file(CONFLICT, ""))
        .unwrap_or_else(|e| panic!("{e}"))
        .to_workflow();
    let track = wf.track("item").unwrap();
    // `review` is place 1; after it the conflict stage is passed over and the merge comes.
    assert_eq!(track.next_stage_index(0), 0);
    assert_eq!(track.next_stage_index(2), track.stages.len());
}

#[test]
fn a_file_with_neither_loads_as_before() {
    // The conflict stage and the merge keys go together: without them the stage is refused, so the
    // plain file has no `resolve` stage at all.
    let plain = file(CONFLICT, "")
        .replace(CONFLICT, "")
        .replace("        entered_by: conflict\n", "");
    let parsed = WorkflowFile::parse(&plain).unwrap_or_else(|e| panic!("{e}"));
    let merge = parsed.track("item").unwrap().merge.as_ref().unwrap();
    assert!(merge.on_conflict.is_none());
    assert!(merge.guarded.is_empty());
    let wf = parsed.to_workflow();
    assert!(wf.track("item").unwrap().on_conflict().is_none());
}

#[test]
fn an_on_conflict_with_no_then_is_refused() {
    let text = file(
        "      on_conflict:\n        stage: resolve\n      guarded:\n        - \"docs/**\"\n",
        "",
    );
    let e = refused(&text);
    assert!(
        e.message.contains("then")
            && e.message.contains("on_conflict")
            && e.message.contains("item"),
        "{e}"
    );
}

#[test]
fn a_then_that_is_the_conflict_stage_is_refused() {
    let text = file(
        "      on_conflict:\n        stage: resolve\n        then: resolve\n      guarded:\n        - \"docs/**\"\n",
        "",
    );
    let e = refused(&text);
    assert!(e.message.contains("is the conflict stage"), "{e}");
    assert!(e.message.contains("item"), "{e}");
}

#[test]
fn a_stage_or_a_then_that_the_track_lacks_is_refused() {
    let e = refused(&file(
        "      on_conflict:\n        stage: nowhere\n        then: review\n      guarded:\n        - \"docs/**\"\n",
        "",
    ));
    assert!(
        e.message.contains("`nowhere`") && e.message.contains("`stage`"),
        "{e}"
    );
    let e = refused(&file(
        "      on_conflict:\n        stage: resolve\n        then: nowhere\n      guarded:\n        - \"docs/**\"\n",
        "",
    ));
    assert!(
        e.message.contains("`nowhere`") && e.message.contains("`then`"),
        "{e}"
    );
}

#[test]
fn a_then_that_a_conflict_or_an_outcome_enters_is_refused() {
    // `then` is a stage that only an outcome enters.
    let text = file(CONFLICT, "")
        .replace(
            "        done:\n          - file: \"{run.dir}/out/{unit.id}/review.md\"\n",
            "        done:\n          - file: \"{run.dir}/out/{unit.id}/review.md\"\n        entered_by: outcome\n        outcomes:\n          - when:\n              - file: \"{run.dir}/out/{unit.id}/x\"\n            to: build\n            max: 1\n",
        );
    let e = refused(&text);
    assert!(
        e.message.contains("own sequence") || e.message.contains("only an outcome"),
        "{e}"
    );
}

#[test]
fn a_stage_that_is_not_entered_by_conflict_and_a_stray_conflict_stage_are_refused() {
    let e = refused(&file(CONFLICT, "").replace("        entered_by: conflict\n", ""));
    assert!(e.message.contains("must be `entered_by: conflict`"), "{e}");
    // An `entered_by: conflict` stage that no `on_conflict` names.
    let e = refused(&file("", ""));
    assert!(
        e.message.contains("is `entered_by: conflict`") && e.message.contains("no `on_conflict`"),
        "{e}"
    );
}

#[test]
fn outcomes_on_the_conflict_stage_and_an_outcome_to_it_are_refused() {
    let with_outcomes = file(CONFLICT, "").replace(
        "          - file: \"{run.dir}/out/{unit.id}/resolve-done\"\n",
        "          - file: \"{run.dir}/out/{unit.id}/resolve-done\"\n        outcomes:\n          - when:\n              - file: \"{run.dir}/out/{unit.id}/x\"\n            to: build\n            max: 1\n",
    );
    let e = refused(&with_outcomes);
    assert!(
        e.message.contains("`entered_by: conflict`") && e.message.contains("outcomes"),
        "{e}"
    );

    let to_it = file(CONFLICT, "").replace(
        "          - file: \"{run.dir}/out/{unit.id}/review.md\"\n",
        "          - file: \"{run.dir}/out/{unit.id}/review.md\"\n        outcomes:\n          - when:\n              - file: \"{run.dir}/out/{unit.id}/x\"\n            to: resolve\n            max: 1\n",
    );
    let e = refused(&to_it);
    assert!(e.message.contains("conflict stage `resolve`"), "{e}");
}

#[test]
fn on_conflict_under_claim_ack_is_refused() {
    let text =
        file(CONFLICT, "").replace("rule: green_checks_in_declared_order", "rule: claim_ack");
    let e = refused(&text);
    assert!(
        e.message.contains("claim_ack") && e.message.contains("on_conflict"),
        "{e}"
    );
}

#[test]
fn guarded_and_on_conflict_go_together() {
    // `on_conflict` with no `guarded`, and with an empty one.
    let none = "      on_conflict:\n        stage: resolve\n        then: review\n";
    let e = refused(&file(none, ""));
    assert!(
        e.message.contains("guarded") && e.message.contains("item"),
        "{e}"
    );
    let empty = format!("{none}      guarded: []\n");
    let e = refused(&file(&empty, ""));
    assert!(e.message.contains("guarded"), "{e}");
    // `guarded` with no `on_conflict`.
    let e = refused(
        &file("      guarded:\n        - \"docs/**\"\n", "")
            .replace("        entered_by: conflict\n", ""),
    );
    assert!(
        e.message.contains("guarded") && e.message.contains("on_conflict"),
        "{e}"
    );
}

#[test]
fn a_glob_that_is_empty_absolute_dotdot_long_or_too_many_is_refused() {
    let with = |glob: &str| {
        format!(
            "      on_conflict:\n        stage: resolve\n        then: review\n      guarded:\n        - {glob}\n"
        )
    };
    for (glob, word) in [
        ("\"\"", "is empty"),
        ("\"/etc/**\"", "starts with `/`"),
        ("\"a/../b\"", "`..`"),
    ] {
        let e = refused(&file(&with(glob), ""));
        assert!(e.message.contains(word), "{glob}: {e}");
    }
    let long = format!("\"{}\"", "a".repeat(201));
    let e = refused(&file(&with(&long), ""));
    assert!(e.message.contains("over 200 characters"), "{e}");
    let at_the_limit = format!("\"{}\"", "a".repeat(200));
    assert!(WorkflowFile::parse(&file(&with(&at_the_limit), "")).is_ok());

    let many: String = (0..33)
        .map(|i| format!("        - \"d{i}/**\"\n"))
        .collect();
    let text = format!(
        "      on_conflict:\n        stage: resolve\n        then: review\n      guarded:\n{many}"
    );
    let e = refused(&file(&text, ""));
    assert!(e.message.contains("33") && e.message.contains("32"), "{e}");
    let many: String = (0..32)
        .map(|i| format!("        - \"d{i}/**\"\n"))
        .collect();
    let text = format!(
        "      on_conflict:\n        stage: resolve\n        then: review\n      guarded:\n{many}"
    );
    assert!(WorkflowFile::parse(&file(&text, "")).is_ok());
}

#[test]
fn an_unknown_entered_by_value_names_both_known_values() {
    let e = refused(&file(CONFLICT, "").replace("entered_by: conflict", "entered_by: merge"));
    assert!(e.message.contains("outcome, conflict"), "{e}");
}

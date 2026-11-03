//! AC-1: stage outcomes in the workflow format (`outcomes:` and `entered_by:`). An outcome sends
//! the unit to an earlier or later stage of the same track, up to a cap. A version 1 file with
//! neither key loads as it did before (ADR-0004).

use pf_workflow::{DoneCond, ParseError, WorkflowFile};

/// A file with an `item` track of `build`, `review` and `fix`. `{outcomes}` goes in `review` and
/// `{fix_extra}` in `fix`. A second track, `other`, holds a stage that `item` cannot name.
fn file(review_extra: &str, fix_extra: &str) -> String {
    format!(
        r#"version: 1
name: loop-flow
params: [repo]
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
{review_extra}      - name: fix
        role: worker
        brief: briefs/item.md
        message: "Fix {{unit.id}}."
        cwd: "{{param.repo}}"
        stall_minutes: 15
        first_turn_seconds: 180
        stop_file: "{{run.dir}}/out/{{unit.id}}/stop"
        done:
          - file: "{{run.dir}}/out/{{unit.id}}/fix-done"
{fix_extra}  other:
    stages:
      - name: elsewhere
        role: worker
        brief: briefs/item.md
        message: "Elsewhere."
        cwd: "{{param.repo}}"
        stall_minutes: 15
        first_turn_seconds: 180
        stop_file: "{{run.dir}}/out/{{unit.id}}/stop"
        done:
          - file: "{{run.dir}}/out/{{unit.id}}/elsewhere.done"
"#
    )
}

const REVIEW_OUTCOMES: &str = r#"        outcomes:
          - when:
              - exit0: [grep, -qx, "verdict: block", "{run.dir}/out/{unit.id}/review.md"]
            to: fix
            max: 2
          - when:
              - file: "{run.dir}/out/{unit.id}/stale"
              - exit0: [test, -f, "{run.dir}/out/{unit.id}/review.md"]
            to: review
            max: 3
"#;

const FIX_OUTCOMES: &str = r#"        entered_by: outcome
        outcomes:
          - when:
              - file: "{run.dir}/out/{unit.id}/fix-done"
            to: review
            max: 2
"#;

fn refused(text: &str) -> ParseError {
    match WorkflowFile::parse(text) {
        Ok(_) => panic!("the file must be refused:\n{text}"),
        Err(e) => e,
    }
}

fn line_of(text: &str, needle: &str) -> usize {
    let at = text
        .find(needle)
        .unwrap_or_else(|| panic!("no `{needle}` in the sample"));
    text[..at].matches('\n').count() + 1
}

#[test]
fn outcomes_load_with_when_to_and_max() {
    let text = file(REVIEW_OUTCOMES, FIX_OUTCOMES);
    let parsed = WorkflowFile::parse(&text).unwrap_or_else(|e| panic!("{e}"));
    let item = parsed.track("item").unwrap();
    let review = &item.stages[1];
    assert_eq!(review.name, "review");
    assert!(!review.entered_by_outcome);
    assert_eq!(review.outcomes.len(), 2);

    let first = &review.outcomes[0];
    assert_eq!(first.to, "fix");
    assert_eq!(first.max, 2);
    assert_eq!(first.when.len(), 1);
    match &first.when[0] {
        DoneCond::Exit0(spec) => assert_eq!(
            spec.argv,
            [
                "grep",
                "-qx",
                "verdict: block",
                "{run.dir}/out/{unit.id}/review.md"
            ]
        ),
        other => panic!("{other:?}"),
    }

    // `when` takes every done kind, and all of its entries must hold.
    let second = &review.outcomes[1];
    assert_eq!(second.to, "review");
    assert_eq!(second.max, 3);
    assert_eq!(second.when.len(), 2);
    assert!(matches!(second.when[0], DoneCond::File(_)));
    assert!(matches!(second.when[1], DoneCond::Exit0(_)));

    let fix = &item.stages[2];
    assert!(fix.entered_by_outcome);
    assert_eq!(fix.outcomes[0].to, "review");
}

#[test]
fn a_to_naming_an_unknown_stage_is_refused_with_its_position() {
    let bad = FIX_OUTCOMES.replace("to: review", "to: reviw");
    let text = file(REVIEW_OUTCOMES, &bad);
    let e = refused(&text);
    assert!(
        e.message.contains("reviw") && e.message.contains("track `item`"),
        "{e}"
    );
    assert_eq!(e.line, line_of(&text, "to: reviw"));
}

#[test]
fn a_to_naming_a_stage_of_another_track_is_refused() {
    let bad = REVIEW_OUTCOMES.replacen("to: fix", "to: elsewhere", 1);
    let text = file(&bad, FIX_OUTCOMES);
    let e = refused(&text);
    assert!(
        e.message.contains("elsewhere") && e.message.contains("track `item`"),
        "{e}"
    );
    assert_eq!(e.line, line_of(&text, "to: elsewhere"));
}

#[test]
fn a_later_stage_and_an_earlier_stage_of_the_same_track_are_accepted() {
    // review -> fix is later, fix -> review is earlier, and review -> review is the same stage.
    let text = file(REVIEW_OUTCOMES, FIX_OUTCOMES);
    assert!(WorkflowFile::parse(&text).is_ok());
    let to_first = FIX_OUTCOMES.replace("to: review", "to: build");
    assert!(WorkflowFile::parse(&file(REVIEW_OUTCOMES, &to_first)).is_ok());
}

#[test]
fn max_zero_and_a_missing_max_are_refused() {
    let zero = REVIEW_OUTCOMES.replacen("max: 2", "max: 0", 1);
    let text = file(&zero, FIX_OUTCOMES);
    let e = refused(&text);
    assert!(e.message.contains("`max` must be at least 1"), "{e}");
    assert_eq!(e.line, line_of(&text, "max: 0"));

    let missing = "        outcomes:\n          - when:\n              - file: \"{run.dir}/x\"\n            to: fix\n";
    let text = file(missing, FIX_OUTCOMES);
    let e = refused(&text);
    assert!(e.message.contains("missing required field `max`"), "{e}");
}

#[test]
fn a_missing_to_or_when_and_an_unknown_field_are_refused() {
    let no_to = "        outcomes:\n          - when:\n              - file: \"{run.dir}/x\"\n            max: 1\n";
    let e = refused(&file(no_to, FIX_OUTCOMES));
    assert!(e.message.contains("missing required field `to`"), "{e}");

    let no_when = "        outcomes:\n          - to: fix\n            max: 1\n";
    let e = refused(&file(no_when, FIX_OUTCOMES));
    assert!(e.message.contains("missing required field `when`"), "{e}");

    let empty_when =
        "        outcomes:\n          - when: []\n            to: fix\n            max: 1\n";
    let e = refused(&file(empty_when, FIX_OUTCOMES));
    assert!(
        e.message.contains("`when`") && e.message.contains("at least one"),
        "{e}"
    );

    let unknown = REVIEW_OUTCOMES.replacen("max: 2", "max: 2\n            retry: yes", 1);
    let e = refused(&file(&unknown, FIX_OUTCOMES));
    assert!(e.message.contains("unknown field `retry`"), "{e}");

    let empty_list = "        outcomes: []\n";
    let e = refused(&file(empty_list, FIX_OUTCOMES));
    assert!(
        e.message.contains("`outcomes`") && e.message.contains("at least one"),
        "{e}"
    );
}

#[test]
fn a_when_entry_is_checked_like_a_done_entry() {
    let bad = REVIEW_OUTCOMES.replacen(
        "{run.dir}/out/{unit.id}/stale",
        "{run.dir}/out/{unit.vars.nope}/stale",
        1,
    );
    let e = refused(&file(&bad, FIX_OUTCOMES));
    assert!(e.message.contains("unit.vars.nope"), "{e}");

    let two = REVIEW_OUTCOMES.replacen(
        "              - file: \"{run.dir}/out/{unit.id}/stale\"\n",
        "              - file: x\n                exit0: [true]\n",
        1,
    );
    let e = refused(&file(&two, FIX_OUTCOMES));
    assert!(e.message.contains("exactly one of"), "{e}");
}

#[test]
fn entered_by_outcome_on_a_stage_with_no_outcomes_is_refused() {
    let text = file(REVIEW_OUTCOMES, "        entered_by: outcome\n");
    let e = refused(&text);
    assert!(
        e.message.contains("entered_by") && e.message.contains("outcomes"),
        "{e}"
    );
    assert_eq!(e.line, line_of(&text, "entered_by: outcome"));
}

#[test]
fn entered_by_takes_only_the_value_outcome() {
    let bad = FIX_OUTCOMES.replace("entered_by: outcome", "entered_by: order");
    let e = refused(&file(REVIEW_OUTCOMES, &bad));
    assert!(
        e.message.contains("`entered_by`") && e.message.contains("outcome"),
        "{e}"
    );
}

#[test]
fn a_version_1_file_with_no_outcomes_loads_unchanged() {
    let text = file("", "");
    let parsed = WorkflowFile::parse(&text).unwrap_or_else(|e| panic!("{e}"));
    for stage in parsed.tracks.iter().flat_map(|t| &t.stages) {
        assert!(stage.outcomes.is_empty(), "{}", stage.name);
        assert!(!stage.entered_by_outcome, "{}", stage.name);
    }
    let workflow = parsed.to_workflow();
    for stage in workflow.tracks.iter().flat_map(|t| &t.stages) {
        assert!(stage.outcomes.is_empty(), "{}", stage.name);
        assert!(!stage.entered_by_outcome, "{}", stage.name);
    }
}

#[test]
fn the_engine_workflow_carries_the_outcomes_and_the_entered_by_flag() {
    let text = file(REVIEW_OUTCOMES, FIX_OUTCOMES);
    let workflow = WorkflowFile::parse(&text).unwrap().to_workflow();
    let item = workflow.track("item").unwrap();
    let review = &item.stages[1];
    assert_eq!(review.outcomes.len(), 2);
    assert_eq!(
        (review.outcomes[0].to.as_str(), review.outcomes[0].max),
        ("fix", 2)
    );
    assert_eq!(
        (review.outcomes[1].to.as_str(), review.outcomes[1].max),
        ("review", 3)
    );
    assert!(!review.entered_by_outcome);
    let fix = &item.stages[2];
    assert!(fix.entered_by_outcome);
    assert_eq!(fix.outcomes[0].to, "review");
}

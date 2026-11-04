//! AC-1: the workflow file reads the spine-close constructs. A seed may carry `start_when`. The file
//! may carry `end_when`. A track's `merge` may say `rule: claim_ack`, with an optional
//! `head_branch`. Each defect is refused with a message that names it. A v1 file with none of
//! these loads as before.

use pf_engine::MergeRule;
use pf_workflow::file::{DoneCond, EndWhen, StartWhen};
use pf_workflow::{ParseError, WorkflowFile};

/// A file with an item track, a lane seed and a spine seed that starts later.
const SPINE: &str = r#"version: 1
name: spine-flow
params: [spine, state_file, base_branch, spine_branch]
roles:
  worker:
    runtime: omp
    model: small-model
    rules: rules/worker.yaml
seeds:
  - id: lane
    group: lane
    order: 1
    track: lane
    repeat:
      when_units_done: item
      until_file: "{run.dir}/rounds.done"
      max: 3
  - id: spine
    group: spine
    order: 2
    track: spine
    start_when:
      file: "{run.dir}/rounds.done"
      units_done: item
tracks:
  lane:
    stages:
      - name: lane
        role: worker
        brief: [briefs/lane.md]
        message: "Lane {unit.id}."
        cwd: "{run.dir}"
        stall_minutes: 10
        first_turn_seconds: 120
        stop_file: "{run.dir}/out/{unit.id}/stop"
        done:
          - file: "{run.dir}/out/{unit.id}/lane.done"
  item:
    stages:
      - name: build
        role: worker
        brief: [briefs/item.md]
        message: "Item {unit.id}."
        cwd: "{run.dir}"
        stall_minutes: 10
        first_turn_seconds: 120
        stop_file: "{run.dir}/out/{unit.id}/stop"
        done:
          - file: "{run.dir}/out/{unit.id}/report.md"
  spine:
    stages:
      - name: spine_close
        role: worker
        brief: [briefs/spine.md]
        message: "Close {param.spine}."
        cwd: "{run.dir}"
        stall_minutes: 30
        first_turn_seconds: 120
        stop_file: "{run.dir}/out/{unit.id}/stop"
        done:
          - pr:
              argv: [show-pr, "{param.spine_branch}"]
              test: url
    merge:
      rule: claim_ack
      repo: "{param.spine}"
      base: "{param.base_branch}"
      head_branch: "{param.spine_branch}"
      pr: [show-pr, "{param.spine_branch}"]
      head_check: false
      after_merge:
        - [echo, merged]
end_when:
  when:
    - exit0: [test, -f, "{param.state_file}"]
    - file: "{run.dir}/closed"
  waiting: "the spine PR is merged: run the second pass in the claim session"
  reason: "spine {param.spine} closed"
"#;

fn refused(text: &str) -> ParseError {
    match WorkflowFile::parse(text) {
        Ok(_) => panic!("the file must be refused:\n{text}"),
        Err(e) => e,
    }
}

fn spine_with(from: &str, to: &str) -> String {
    assert!(SPINE.contains(from), "the sample has no `{from}`");
    SPINE.replacen(from, to, 1)
}

#[test]
fn start_when_loads_with_a_file_and_a_track() {
    let file = WorkflowFile::parse(SPINE).unwrap_or_else(|e| panic!("{e}"));
    let spine = file.seeds.iter().find(|s| s.id == "spine").unwrap();
    assert_eq!(
        spine.start_when,
        Some(StartWhen {
            file: "{run.dir}/rounds.done".to_string(),
            units_done: "item".to_string(),
        })
    );
    assert!(spine.repeat.is_none());
    let lane = file.seeds.iter().find(|s| s.id == "lane").unwrap();
    assert!(lane.start_when.is_none());
}

#[test]
fn start_when_with_repeat_is_refused() {
    let text = spine_with(
        "      units_done: item\n",
        "      units_done: item\n    repeat:\n      when_units_done: item\n      until_file: \"{run.dir}/x\"\n      max: 2\n",
    );
    let e = refused(&text);
    assert!(
        e.message.contains("start_when") && e.message.contains("repeat"),
        "{e}"
    );
}

#[test]
fn start_when_units_done_must_name_a_track() {
    let text = spine_with("      units_done: item\n", "      units_done: nowhere\n");
    let e = refused(&text);
    assert!(
        e.message.contains("nowhere") && e.message.contains("track"),
        "{e}"
    );
}

#[test]
fn start_when_needs_both_fields_and_knows_no_other() {
    let no_file = spine_with(
        "      file: \"{run.dir}/rounds.done\"\n      units_done",
        "      units_done",
    );
    assert!(refused(&no_file).message.contains("file"));
    let extra = spine_with(
        "      units_done: item\n",
        "      units_done: item\n      also: yes\n",
    );
    assert!(refused(&extra).message.contains("unknown field `also`"));
}

#[test]
fn start_when_file_is_a_run_template() {
    let text = spine_with(
        "{run.dir}/rounds.done\"\n      units_done",
        "{unit.id}/rounds.done\"\n      units_done",
    );
    let e = refused(&text);
    assert!(e.message.contains("unit"), "{e}");
}

#[test]
fn end_when_loads_with_when_waiting_and_reason() {
    let file = WorkflowFile::parse(SPINE).unwrap_or_else(|e| panic!("{e}"));
    let end: &EndWhen = file.end_when.as_ref().expect("end_when");
    assert_eq!(end.when.len(), 2);
    assert!(matches!(&end.when[0], DoneCond::Exit0(_)));
    assert_eq!(end.when[1], DoneCond::File("{run.dir}/closed".to_string()));
    assert_eq!(
        end.waiting,
        "the spine PR is merged: run the second pass in the claim session"
    );
    assert_eq!(end.reason, "spine {param.spine} closed");
}

#[test]
fn end_when_with_an_empty_when_is_refused() {
    let from = "  when:\n    - exit0: [test, -f, \"{param.state_file}\"]\n    - file: \"{run.dir}/closed\"\n";
    let e = refused(&spine_with(from, "  when: []\n"));
    assert!(
        e.message.contains("when") && e.message.contains("at least one"),
        "{e}"
    );
}

#[test]
fn end_when_needs_all_three_fields_and_rejects_other_kinds_of_condition() {
    for key in ["  waiting:", "  reason:"] {
        let line = SPINE
            .lines()
            .find(|l| l.starts_with(key))
            .unwrap_or_else(|| panic!("no {key}"));
        let e = refused(&SPINE.replacen(&format!("{line}\n"), "", 1));
        assert!(
            e.message.contains(key.trim().trim_end_matches(':')),
            "{key}: {e}"
        );
    }
    // A `pr` condition has no meaning at the end of a run.
    let e = refused(&spine_with(
        "    - file: \"{run.dir}/closed\"\n",
        "    - pr:\n        argv: [show-pr, x]\n        test: url\n",
    ));
    assert!(e.message.contains("pr"), "{e}");
}

#[test]
fn end_when_text_is_checked_for_placeholders() {
    let e = refused(&spine_with(
        "reason: \"spine {param.spine} closed\"",
        "reason: \"spine {param.nowhere} closed\"",
    ));
    assert!(e.message.contains("nowhere"), "{e}");
    // `{unit.id}` needs a unit, and the end of a run has none.
    let e = refused(&spine_with(
        "reason: \"spine {param.spine} closed\"",
        "reason: \"unit {unit.id} closed\"",
    ));
    assert!(e.message.contains("unit"), "{e}");
}

#[test]
fn claim_ack_loads_with_and_without_head_branch() {
    let file = WorkflowFile::parse(SPINE).unwrap_or_else(|e| panic!("{e}"));
    let merge = file.track("spine").unwrap().merge.as_ref().unwrap();
    assert_eq!(merge.rule, MergeRule::ClaimAck);
    assert_eq!(merge.head_branch.as_deref(), Some("{param.spine_branch}"));
    assert!(!merge.head_check);

    let without = spine_with("      head_branch: \"{param.spine_branch}\"\n", "");
    let file = WorkflowFile::parse(&without).unwrap_or_else(|e| panic!("{e}"));
    let merge = file.track("spine").unwrap().merge.as_ref().unwrap();
    assert_eq!(merge.rule, MergeRule::ClaimAck);
    assert_eq!(merge.head_branch, None);

    // The engine's workflow carries the rule and the rendered-later head branch.
    let wf = file.to_workflow();
    let def = wf.track("spine").unwrap().merge.as_ref().unwrap();
    assert_eq!(def.rule, MergeRule::ClaimAck);
    assert_eq!(def.head_branch, "");
}

#[test]
fn head_branch_belongs_to_claim_ack_only() {
    let text = spine_with(
        "      rule: claim_ack\n",
        "      rule: green_checks_in_declared_order\n",
    );
    let e = refused(&text);
    assert!(
        e.message.contains("head_branch") && e.message.contains("claim_ack"),
        "{e}"
    );
}

#[test]
fn an_unknown_rule_names_both_known_rules() {
    let e = refused(&spine_with(
        "      rule: claim_ack\n",
        "      rule: ack_maybe\n",
    ));
    assert!(e.message.contains("`ack_maybe`"), "{e}");
    assert!(e.message.contains("green_checks_in_declared_order"), "{e}");
    assert!(e.message.contains("claim_ack"), "{e}");
}

#[test]
fn head_branch_is_checked_for_placeholders() {
    let e = refused(&spine_with(
        "head_branch: \"{param.spine_branch}\"",
        "head_branch: \"{param.nowhere}\"",
    ));
    assert!(e.message.contains("nowhere"), "{e}");
}

#[test]
fn a_v1_file_with_none_of_the_new_keys_loads_unchanged() {
    let old = r#"version: 1
name: old
roles:
  worker:
    runtime: omp
    model: m
    rules: rules/worker.yaml
seeds:
  - id: starter
    group: g
    track: item
tracks:
  item:
    stages:
      - name: build
        role: worker
        brief: [b.md]
        message: "Go."
        cwd: "."
        stall_minutes: 10
        first_turn_seconds: 120
        stop_file: "stop"
        done:
          - file: "report.md"
    merge:
      rule: green_checks_in_declared_order
      repo: "r"
      base: "b"
      pr: [show-pr]
"#;
    let file = WorkflowFile::parse(old).unwrap_or_else(|e| panic!("{e}"));
    assert!(file.end_when.is_none());
    assert!(file.seeds[0].start_when.is_none());
    let merge = file.track("item").unwrap().merge.as_ref().unwrap();
    assert_eq!(merge.rule, MergeRule::GreenChecksInDeclaredOrder);
    assert_eq!(merge.head_branch, None);
}

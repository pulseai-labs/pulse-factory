//! AC-1: the strict workflow file. A valid file parses. Each defect is refused with its line and
//! column (both count from 1, and point at the start of the offending key or value).

use pf_workflow::{ParseError, WorkflowFile};

const VALID: &str = r#"version: 1
name: demo-flow
params: [repo, briefs]
plugins:
  - name: toolkit
    min_version: 1.2.0
roles:
  worker:
    runtime: omp
    model: small-model
    rules: rules/worker.yaml
settings:
  model_sessions: 2
  cold_builds: 1
plan_check: [count-items, "{param.repo}"]
roots: ["{param.repo}"]
unit_sources:
  - dir: "{run.dir}/queue"
    track: item
    vars: [handoff]
seeds:
  - id: starter
    group: start
    order: 1
    track: lane
    vars:
      mode: fill
    repeat:
      when_units_done: item
      until_file: "{run.dir}/all.done"
      max: 5
tracks:
  lane:
    stages:
      - name: fill
        role: worker
        brief: [briefs/lane.md]
        message: "Read {param.briefs}/lane.md. Run {run.id}. Unit {unit.id}."
        cwd: "{param.repo}"
        slots: [session]
        stall_minutes: 10
        first_turn_seconds: 120
        stop_file: "{run.dir}/out/{unit.id}/stop"
        done:
          - file: "{run.dir}/out/{unit.id}/lane.done"
  item:
    stages:
      - name: build
        role: worker
        brief: briefs/item.md
        message: "Build {unit.vars.handoff}."
        cwd: "{unit.vars.handoff}"
        slots: [session, build]
        before:
          - [mkdir, -p, "{run.dir}/out/{unit.id}"]
        after_first_turn:
          - argv: [echo, started]
            timeout_seconds: 30
        stall_minutes: 15
        first_turn_seconds: 180
        stop_file: "{run.dir}/out/{unit.id}/stop"
        skills: [alpha, beta]
        settings:
          env.MODE: "{param.repo}"
          push_dir: "{unit.vars.handoff}"
          push_branch: "work/{unit.id}"
        done:
          - file: "{run.dir}/out/{unit.id}/report.md"
          - exit0: [test, -f, "{run.dir}/out/{unit.id}/report.md"]
          - pr:
              argv: [show-pr, "{unit.id}"]
              test: url
        park_on:
          - file: "{run.dir}/out/{unit.id}/gaps.json"
            reason: "gaps need an answer"
    merge:
      rule: green_checks_in_declared_order
      repo: "{param.repo}"
      base: "{param.briefs}"
      pr: [show-pr, "{unit.id}"]
      head_check: true
      after_merge:
        - [echo, merged, "{unit.id}"]
"#;

/// The 1-based line and column of the first occurrence of `needle` in `text`.
fn pos_of(text: &str, needle: &str) -> (usize, usize) {
    let at = text
        .find(needle)
        .unwrap_or_else(|| panic!("no `{needle}` in the sample"));
    let before = &text[..at];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().unwrap().chars().count() + 1;
    (line, col)
}

/// The 1-based position of the last occurrence.
fn last_pos_of(text: &str, needle: &str) -> (usize, usize) {
    let at = text.rfind(needle).unwrap();
    let before = &text[..at];
    (
        before.matches('\n').count() + 1,
        before.rsplit('\n').next().unwrap().chars().count() + 1,
    )
}

fn refused(text: &str) -> ParseError {
    match WorkflowFile::parse(text) {
        Ok(_) => panic!("the file must be refused:\n{text}"),
        Err(e) => e,
    }
}

#[test]
fn a_valid_file_parses() {
    let file = WorkflowFile::parse(VALID).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(file.version, 1);
    assert_eq!(file.name, "demo-flow");
    assert_eq!(file.params, ["repo", "briefs"]);
    assert_eq!(file.plugins.len(), 1);
    assert_eq!(file.plugins[0].name, "toolkit");
    assert_eq!(file.plugins[0].min_version, "1.2.0");
    assert_eq!(file.roles.len(), 1);
    assert_eq!(file.roles[0].runtime, "omp");
    assert_eq!(file.roles[0].rules, "rules/worker.yaml");
    assert_eq!(file.settings.model_sessions, 2);
    assert_eq!(file.roots, ["{param.repo}"]);
    assert_eq!(file.unit_sources[0].track, "item");
    assert_eq!(file.seeds[0].id, "starter");
    assert_eq!(file.seeds[0].repeat.as_ref().unwrap().max, 5);
    assert_eq!(file.tracks.len(), 2);
    let item = &file.tracks[1];
    assert_eq!(item.name, "item");
    let stage = &item.stages[0];
    assert_eq!(stage.slots, ["session", "build"]);
    assert_eq!(stage.done.len(), 3);
    assert_eq!(stage.park_on.len(), 1);
    assert_eq!(stage.settings["push_branch"], "work/{unit.id}");
    assert!(item.merge.as_ref().unwrap().head_check);
    assert!(!file.tracks[0].stages.is_empty());
    assert!(file.tracks[0].merge.is_none());
}

#[test]
fn an_unknown_field_at_top_level_is_refused_with_its_position() {
    let text = VALID.replace("name: demo-flow\n", "name: demo-flow\ncolour: red\n");
    let e = refused(&text);
    assert_eq!((e.line, e.col), pos_of(&text, "colour"));
    assert!(e.message.contains("unknown field `colour`"), "{e}");
}

#[test]
fn an_unknown_field_in_a_stage_is_refused_with_its_position() {
    let text = VALID.replace(
        "        stall_minutes: 10\n",
        "        stall_minutes: 10\n        flavour: mint\n",
    );
    let e = refused(&text);
    assert_eq!((e.line, e.col), pos_of(&text, "flavour"));
    assert!(e.message.contains("unknown field `flavour`"), "{e}");
}

#[test]
fn an_unknown_field_in_a_track_is_refused_with_its_position() {
    let text = VALID.replace(
        "  lane:\n    stages:",
        "  lane:\n    shade: blue\n    stages:",
    );
    let e = refused(&text);
    assert_eq!((e.line, e.col), pos_of(&text, "shade"));
    assert!(e.message.contains("unknown field `shade`"), "{e}");
}

#[test]
fn a_duplicate_key_is_refused_at_the_second_key() {
    let text = VALID.replace("name: demo-flow\n", "name: demo-flow\nname: other-name\n");
    let e = refused(&text);
    assert_eq!((e.line, e.col), last_pos_of(&text, "name: other-name"));
    assert!(e.message.contains("duplicate key `name`"), "{e}");

    // A duplicate deep in the file is found too.
    let text = VALID.replace(
        "        stall_minutes: 10\n",
        "        stall_minutes: 10\n        stall_minutes: 11\n",
    );
    let e = refused(&text);
    assert_eq!((e.line, e.col), pos_of(&text, "stall_minutes: 11"));
    assert!(e.message.contains("duplicate key `stall_minutes`"), "{e}");
}

#[test]
fn a_missing_required_field_is_refused_at_its_mapping() {
    let text = VALID.replace("name: demo-flow\n", "");
    let e = refused(&text);
    assert_eq!((e.line, e.col), pos_of(&text, "version"));
    assert!(e.message.contains("missing required field `name`"), "{e}");

    let text = VALID.replace(
        "        role: worker\n        brief: [briefs/lane.md]\n",
        "        brief: [briefs/lane.md]\n",
    );
    let e = refused(&text);
    assert!(e.message.contains("missing required field `role`"), "{e}");
    assert_eq!((e.line, e.col), pos_of(&text, "name: fill"));
}

#[test]
fn a_role_with_no_runtime_is_refused() {
    let text = VALID.replace("    runtime: omp\n", "");
    let e = refused(&text);
    assert!(
        e.message.contains("missing required field `runtime`"),
        "{e}"
    );
    assert_eq!((e.line, e.col), pos_of(&text, "model: small-model"));
}

#[test]
fn an_unknown_version_is_refused_and_the_error_names_version_1() {
    let text = VALID.replace("version: 1\n", "version: 2\n");
    let e = refused(&text);
    assert_eq!((e.line, e.col), pos_of(&text, "2"));
    assert!(e.message.contains("unsupported version 2"), "{e}");
    assert!(e.message.contains("supported version is 1"), "{e}");

    let text = VALID.replace("version: 1\n", "version: one\n");
    let e = refused(&text);
    assert!(e.message.contains("supported version is 1"), "{e}");

    // The version is read first: a file of a future version gets the version error, even when
    // other fields are unknown to this parser.
    let future = "version: 9\nfuture_field: yes\n";
    let e = refused(future);
    assert!(e.message.contains("supported version is 1"), "{e}");
    assert_eq!(e.line, 1);
}

#[test]
fn a_placeholder_with_an_undeclared_name_is_refused() {
    // A parameter that `params` does not declare.
    let text = VALID.replace("Read {param.briefs}/lane.md.", "Read {param.nope}/lane.md.");
    let e = refused(&text);
    assert_eq!((e.line, e.col), pos_of(&text, "\"Read {param.nope}"));
    assert!(e.message.contains("{param.nope}"), "{e}");
    assert!(e.message.contains("not declared"), "{e}");

    // A unit variable that no source or seed of the track declares.
    let text = VALID.replace("Build {unit.vars.handoff}.", "Build {unit.vars.zzz}.");
    let e = refused(&text);
    assert!(e.message.contains("{unit.vars.zzz}"), "{e}");

    // A name outside the closed set.
    let text = VALID.replace("Run {run.id}.", "Run {bogus.thing}.");
    let e = refused(&text);
    assert!(e.message.contains("unknown placeholder"), "{e}");

    // In a command.
    let text = VALID.replace(
        "[echo, merged, \"{unit.id}\"]",
        "[echo, merged, \"{param.ghost}\"]",
    );
    let e = refused(&text);
    assert!(e.message.contains("{param.ghost}"), "{e}");

    // A unit placeholder where no unit exists: `plan_check` and `roots`.
    let text = VALID.replace(
        "plan_check: [count-items, \"{param.repo}\"]",
        "plan_check: [count-items, \"{unit.id}\"]",
    );
    let e = refused(&text);
    assert!(e.message.contains("{unit.id}"), "{e}");
}

#[test]
fn literal_braces_that_are_not_placeholders_pass() {
    let text = VALID.replace(
        "[echo, merged, \"{unit.id}\"]",
        "[jq, \"{a: 1}\", \"{ spaced }\", \"{unit.id}\"]",
    );
    WorkflowFile::parse(&text).unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn other_defects_are_refused_with_a_position() {
    // An alias and an anchor have no place in the strict file.
    let text = VALID.replace("name: demo-flow\n", "name: &n demo-flow\n");
    assert!(refused(&text).message.contains("anchors"), "anchor");
    // Bad YAML.
    let e = refused("version: 1\nname: [unclosed\n");
    assert!(e.line >= 1 && e.col >= 1, "{e}");
    // Two documents.
    let e = refused(&format!("{VALID}---\nversion: 1\n"));
    assert!(e.message.contains("one YAML document"), "{e}");
    // A wrong type.
    let text = VALID.replace("model_sessions: 2", "model_sessions: many");
    let e = refused(&text);
    assert_eq!((e.line, e.col), pos_of(&text, "many"));
    // A stage with an unknown role.
    let text = VALID.replace(
        "        role: worker\n        brief: [briefs/lane.md]",
        "        role: ghost\n        brief: [briefs/lane.md]",
    );
    let e = refused(&text);
    assert!(e.message.contains("role `ghost`"), "{e}");
    // A seed on a track that does not exist.
    let text = VALID.replace(
        "    track: lane\n    vars:\n      mode",
        "    track: nowhere\n    vars:\n      mode",
    );
    let e = refused(&text);
    assert!(e.message.contains("track `nowhere`"), "{e}");
    // A `repeat` without `max`.
    let text = VALID.replace("      max: 5\n", "");
    let e = refused(&text);
    assert!(e.message.contains("missing required field `max`"), "{e}");
    // An empty `done` list.
    let text = VALID.replace(
        "        done:\n          - file: \"{run.dir}/out/{unit.id}/lane.done\"\n",
        "        done: []\n",
    );
    let e = refused(&text);
    assert!(e.message.contains("`done`"), "{e}");
    // A timeout over the limit.
    let text = VALID.replace("timeout_seconds: 30", "timeout_seconds: 601");
    let e = refused(&text);
    assert!(e.message.contains("600"), "{e}");
    // A shell string in place of an argv.
    let text = VALID.replace("[echo, merged, \"{unit.id}\"]", "\"echo merged\"");
    let e = refused(&text);
    assert!(e.message.contains("argv"), "{e}");
}

#[test]
fn the_error_text_carries_the_position() {
    let text = VALID.replace("version: 1\n", "version: 2\n");
    let e = refused(&text);
    let shown = e.to_string();
    assert!(
        shown.starts_with(&format!("line {}, column {}: ", e.line, e.col)),
        "{shown}"
    );
}

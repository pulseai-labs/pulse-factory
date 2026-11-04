//! AC-1: the optional stage key `base_push: {dir, branch}` in the workflow format. Both fields are
//! templates in the stage's scope. A version 1 file without the key loads as it did before
//! (ADR-0004).

use pf_workflow::{BasePush, ParseError, WorkflowFile};

/// A file with an `item` track of `build` and `close`. `{close_extra}` goes in `close`.
fn file(close_extra: &str) -> String {
    format!(
        r#"version: 1
name: base-flow
params: [repo, spine_branch]
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
      - name: close
        role: worker
        brief: briefs/item.md
        message: "Close {{unit.id}}."
        cwd: "{{param.repo}}"
        stall_minutes: 15
        first_turn_seconds: 180
        stop_file: "{{run.dir}}/out/{{unit.id}}/stop"
        done:
          - file: "{{run.dir}}/out/{{unit.id}}/close.done"
{close_extra}"#
    )
}

const BASE_PUSH: &str = r#"        base_push:
          dir: "{param.repo}"
          branch: "{param.spine_branch}"
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
fn base_push_loads_with_dir_and_branch_as_templates() {
    let text = file(BASE_PUSH);
    let parsed = WorkflowFile::parse(&text).unwrap_or_else(|e| panic!("{e}"));
    let item = parsed.track("item").unwrap();
    assert!(item.stages[0].base_push.is_none());
    assert_eq!(
        item.stages[1].base_push,
        Some(BasePush {
            dir: "{param.repo}".to_string(),
            branch: "{param.spine_branch}".to_string(),
        })
    );

    // A unit variable is a placeholder of the stage's scope too.
    let unit_var = BASE_PUSH.replace("{param.spine_branch}", "{unit.vars.branch}");
    let parsed = WorkflowFile::parse(&file(&unit_var)).unwrap_or_else(|e| panic!("{e}"));
    let push = parsed.track("item").unwrap().stages[1]
        .base_push
        .clone()
        .unwrap();
    assert_eq!(push.branch, "{unit.vars.branch}");
}

#[test]
fn a_missing_dir_or_branch_is_refused() {
    let no_dir = "        base_push:\n          branch: \"{param.spine_branch}\"\n";
    let e = refused(&file(no_dir));
    assert!(e.message.contains("missing required field `dir`"), "{e}");

    let no_branch = "        base_push:\n          dir: \"{param.repo}\"\n";
    let e = refused(&file(no_branch));
    assert!(e.message.contains("missing required field `branch`"), "{e}");
}

#[test]
fn an_empty_dir_or_branch_is_refused_with_its_position() {
    let empty_branch = BASE_PUSH.replace("\"{param.spine_branch}\"", "\"\"");
    let text = file(&empty_branch);
    let e = refused(&text);
    assert!(
        e.message.contains("`branch`") && e.message.contains("must not be empty"),
        "{e}"
    );
    assert_eq!(e.line, line_of(&text, "branch: \"\""));

    let empty_dir = BASE_PUSH.replace("\"{param.repo}\"", "\"\"");
    let text = file(&empty_dir);
    let e = refused(&text);
    assert!(
        e.message.contains("`dir`") && e.message.contains("must not be empty"),
        "{e}"
    );
    assert_eq!(e.line, line_of(&text, "dir: \"\""));
}

#[test]
fn an_unknown_field_in_the_mapping_is_refused_with_its_position() {
    let extra = format!("{BASE_PUSH}          force: true\n");
    let text = file(&extra);
    let e = refused(&text);
    assert!(
        e.message.contains("unknown field `force`") && e.message.contains("base_push"),
        "{e}"
    );
    assert_eq!(e.line, line_of(&text, "force: true"));
}

#[test]
fn an_undeclared_placeholder_is_refused_with_its_position() {
    let bad_param = BASE_PUSH.replace("{param.spine_branch}", "{param.nope}");
    let text = file(&bad_param);
    let e = refused(&text);
    assert!(e.message.contains("param.nope"), "{e}");
    assert_eq!(e.line, line_of(&text, "branch: \"{param.nope}\""));

    let bad_var = BASE_PUSH.replace("{param.repo}", "{unit.vars.nope}");
    let text = file(&bad_var);
    let e = refused(&text);
    assert!(e.message.contains("unit.vars.nope"), "{e}");
    assert_eq!(e.line, line_of(&text, "dir: \"{unit.vars.nope}\""));
}

#[test]
fn base_push_must_be_a_mapping() {
    let e = refused(&file("        base_push: origin\n"));
    assert!(
        e.message.contains("base_push") && e.message.contains("mapping"),
        "{e}"
    );
}

#[test]
fn a_version_1_file_with_no_base_push_loads_unchanged() {
    let text = file("");
    let parsed = WorkflowFile::parse(&text).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(parsed.version, 1);
    for stage in parsed.tracks.iter().flat_map(|t| &t.stages) {
        assert!(stage.base_push.is_none(), "{}", stage.name);
    }
}

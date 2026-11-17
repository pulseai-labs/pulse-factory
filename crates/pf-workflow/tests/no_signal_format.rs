//! AC-1: the optional stage key `no_signal: {nudge, nudges, run}` in the workflow format. A stage
//! that declares it says what the controller does when a seat's turn ends with no done signal: it
//! may deliver a message into the same session up to a cap, and it may run commands before it
//! parks. A version 1 file without the key loads as it did before (ADR-0004).

use pf_workflow::{CommandSpec, NoSignal, ParseError, WorkflowFile};

/// A file with an `item` track of `build` and `close`. `{close_extra}` goes in `close`.
fn file(close_extra: &str) -> String {
    format!(
        r#"version: 1
name: no-signal-flow
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

const FULL: &str = r#"        no_signal:
          nudge: "Finish {unit.id}: write {run.dir}/out/{unit.id}/close.done."
          nudges: 2
          run:
            - [mkdir, -p, "{run.dir}/out/{unit.id}"]
            - argv: [sh, -c, "echo stranded > {run.dir}/out/{unit.id}/stop"]
              timeout_seconds: 30
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

fn close_no_signal(text: &str) -> Option<NoSignal> {
    let parsed = WorkflowFile::parse(text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    parsed.track("item").unwrap().stages[1].no_signal.clone()
}

#[test]
fn a_file_without_the_key_loads_as_before() {
    let parsed = WorkflowFile::parse(&file("")).unwrap_or_else(|e| panic!("{e}"));
    for stage in &parsed.track("item").unwrap().stages {
        assert_eq!(stage.no_signal, None, "stage {}", stage.name);
    }
}

#[test]
fn the_full_shape_loads_with_a_nudge_a_cap_and_commands() {
    let got = close_no_signal(&file(FULL)).expect("no_signal is read");
    assert_eq!(
        got.nudge.as_deref(),
        Some("Finish {unit.id}: write {run.dir}/out/{unit.id}/close.done.")
    );
    assert_eq!(got.nudges, 2);
    assert_eq!(got.run.len(), 2);
    assert_eq!(
        got.run[0],
        CommandSpec::new(&["mkdir", "-p", "{run.dir}/out/{unit.id}"])
    );
    assert_eq!(got.run[1].timeout_seconds, 30);
    // The other stage of the file declares none.
    let parsed = WorkflowFile::parse(&file(FULL)).unwrap();
    assert_eq!(parsed.track("item").unwrap().stages[0].no_signal, None);
}

#[test]
fn a_nudge_alone_loads_and_the_cap_defaults_to_zero_when_only_run_is_given() {
    let nudge_only =
        "        no_signal:\n          nudge: \"Finish {unit.id}.\"\n          nudges: 1\n";
    let got = close_no_signal(&file(nudge_only)).unwrap();
    assert_eq!((got.nudges, got.run.len()), (1, 0));

    let run_only = "        no_signal:\n          run:\n            - [true]\n";
    let got = close_no_signal(&file(run_only)).unwrap();
    assert_eq!(got.nudge, None);
    assert_eq!(got.nudges, 0, "the cap defaults to 0");
    assert_eq!(got.run, vec![CommandSpec::new(&["true"])]);
}

#[test]
fn the_cap_may_be_any_whole_number_from_zero_to_three() {
    for n in 0..=3 {
        let text = format!(
            "        no_signal:\n          nudge: \"Finish.\"\n          nudges: {n}\n          run:\n            - [true]\n"
        );
        assert_eq!(close_no_signal(&file(&text)).unwrap().nudges, n);
    }
}

#[test]
fn a_cap_above_three_is_refused_with_the_stage_and_the_key() {
    let text = file(
        "        no_signal:\n          nudge: \"Finish.\"\n          nudges: 4\n          run:\n            - [true]\n",
    );
    let e = refused(&text);
    assert!(
        e.message.contains("`nudges`")
            && e.message.contains("stage `close`")
            && e.message.contains("0 to 3"),
        "{e}"
    );
    assert_eq!(e.line, line_of(&text, "nudges: 4"));
}

#[test]
fn a_cap_that_is_not_a_whole_number_is_refused() {
    for bad in ["-1", "two", "1.5", "\"2\""] {
        let text = file(&format!(
            "        no_signal:\n          nudge: \"Finish.\"\n          nudges: {bad}\n"
        ));
        let e = refused(&text);
        assert!(
            e.message.contains("`nudges`") && e.message.contains("stage `close`"),
            "{bad}: {e}"
        );
    }
}

#[test]
fn a_cap_above_zero_needs_a_nudge() {
    let text =
        file("        no_signal:\n          nudges: 1\n          run:\n            - [true]\n");
    let e = refused(&text);
    assert!(
        e.message.contains("`nudge`")
            && e.message.contains("stage `close`")
            && e.message.contains("`nudges`"),
        "{e}"
    );
}

#[test]
fn an_unknown_sub_key_is_refused_with_its_position() {
    let text = file(&format!("{FULL}          retries: 3\n"));
    let e = refused(&text);
    assert!(
        e.message.contains("unknown field `retries`")
            && e.message.contains("no_signal")
            && e.message.contains("stage `close`"),
        "{e}"
    );
    assert_eq!(e.line, line_of(&text, "retries: 3"));
}

#[test]
fn a_block_with_neither_a_nudge_nor_run_is_refused() {
    for body in [
        "        no_signal: {}\n",
        "        no_signal:\n          nudges: 0\n",
        // A nudge text with a cap of 0 cannot fire, so it is not a nudge.
        "        no_signal:\n          nudge: \"Finish.\"\n",
        "        no_signal:\n          nudge: \"Finish.\"\n          nudges: 0\n          run: []\n",
    ] {
        let e = refused(&file(body));
        assert!(
            e.message.contains("`no_signal`")
                && e.message.contains("stage `close`")
                && e.message.contains("`nudge`")
                && e.message.contains("`run`"),
            "{body}: {e}"
        );
    }
}

#[test]
fn no_signal_must_be_a_mapping() {
    let e = refused(&file("        no_signal: yes\n"));
    assert!(e.message.contains("no_signal"), "{e}");
}

#[test]
fn an_empty_nudge_is_refused() {
    let text = file("        no_signal:\n          nudge: \"\"\n          nudges: 1\n");
    let e = refused(&text);
    assert!(
        e.message.contains("`nudge`")
            && e.message.contains("stage `close`")
            && e.message.contains("must not be empty"),
        "{e}"
    );
}

#[test]
fn the_nudge_takes_the_placeholders_of_the_stage_message() {
    let ok = "        no_signal:\n          nudge: \"{param.repo} {unit.vars.branch} {run.id} {unit.id}\"\n          nudges: 1\n";
    assert!(close_no_signal(&file(ok)).is_some());

    let bad_param = "        no_signal:\n          nudge: \"{param.nope}\"\n          nudges: 1\n";
    let e = refused(&file(bad_param));
    assert!(e.message.contains("parameter `nope`"), "{e}");

    let bad_var =
        "        no_signal:\n          nudge: \"{unit.vars.nope}\"\n          nudges: 1\n";
    let e = refused(&file(bad_var));
    assert!(e.message.contains("variable `nope`"), "{e}");

    // An answer placeholder belongs to an `on_answer` hook only.
    let answer = "        no_signal:\n          nudge: \"{answer.text}\"\n          nudges: 1\n";
    let e = refused(&file(answer));
    assert!(e.message.contains("on_answer"), "{e}");
}

#[test]
fn the_run_commands_are_bounded_like_other_commands() {
    // A shell string is not a command.
    let e = refused(&file(
        "        no_signal:\n          run:\n            - \"echo hi\"\n",
    ));
    assert!(e.message.contains("argv list"), "{e}");

    // The timeout is from 1 to 600 seconds.
    for t in ["0", "601"] {
        let text = file(&format!(
            "        no_signal:\n          run:\n            - argv: [true]\n              timeout_seconds: {t}\n"
        ));
        let e = refused(&text);
        assert!(e.message.contains("`timeout_seconds`"), "{t}: {e}");
    }

    // The program may not come from a unit record, and `run` is a list.
    let e = refused(&file(
        "        no_signal:\n          run:\n            - [\"{unit.vars.branch}\"]\n",
    ));
    assert!(e.message.contains("program"), "{e}");
    let e = refused(&file("        no_signal:\n          run: [true]\n"));
    assert!(e.message.contains("argv"), "{e}");
}

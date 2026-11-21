//! AC-1 of `r2.s11.w4`: a `park_on` entry of the workflow file declares `options`. Each option has a
//! label, an answer text, a stage of the same track, or both, and the entry may mark one as
//! recommended. The texts are literal text, the file names no verb, and every refusal names the line
//! and the column of the text it blames (`docs/contracts/workflow-file.md`, "`park_on`").

use std::path::Path;

use pf_engine::status::redact;
use pf_workflow::package::Package;
use pf_workflow::{ParseError, WorkflowFile};

/// A made-up track `item` of two stages and a track `other` of one. `@STAGE@` is a line of the stage
/// `build` and `@ENTRY@` goes under its `park_on` entry.
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
@STAGE@
        done:
          - file: "{run.dir}/out/{unit.id}/report.md"
        park_on:
          - file: "{run.dir}/out/{unit.id}/hold.txt"
            reason: "held"
@ENTRY@
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
  other:
    stages:
      - name: elsewhere
        role: worker
        brief: [briefs/item.md]
        message: "Elsewhere."
        cwd: "{param.repo}"
        stall_minutes: 15
        first_turn_seconds: 180
        stop_file: "{run.dir}/out/{unit.id}/stop"
        done:
          - file: "{run.dir}/out/{unit.id}/elsewhere.md"
"#;

const HOOK: &str = r#"            on_answer:
              - [sh, "{run.package}/scripts/note.sh", "{answer.file}", "{answer.source}"]"#;
const FALLBACK: &str = r#"        on_answer:
          - [sh, "{run.package}/scripts/note.sh", "{answer.file}", "{answer.source}"]"#;

fn flow(stage: &str, entry: &str) -> String {
    BASE.replace("@STAGE@\n", &format!("{stage}\n"))
        .replace("@ENTRY@\n", &format!("{entry}\n"))
}

/// A file whose entry holds the hook and `options` (the lines under `options:`, at 14 spaces).
fn with_options(options: &str) -> String {
    flow("", &format!("{HOOK}\n            options:\n{options}"))
}

fn parse(text: &str) -> Result<WorkflowFile, ParseError> {
    WorkflowFile::parse(text)
}

fn refused(text: &str) -> ParseError {
    match parse(text) {
        Ok(_) => panic!("the file should be refused:\n{text}"),
        Err(e) => e,
    }
}

/// The line (counting from 1) of the first line that holds `needle`.
fn line_of(text: &str, needle: &str) -> usize {
    text.lines()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} is not in the text"))
        + 1
}

/// The error names the line of `needle`, a column, and `message`.
fn blames(text: &str, needle: &str, message: &str) {
    let e = refused(text);
    let line = line_of(text, needle);
    assert_eq!(e.line, line, "the line of {needle:?}: {e}");
    assert!(e.col >= 1, "{e}");
    assert!(e.message.contains(message), "{e}");
    assert!(
        e.to_string().starts_with(&format!("line {line}, column ")),
        "{e}"
    );
}

fn options_of(text: &str) -> Vec<pf_workflow::ParkOption> {
    let file = parse(text).unwrap_or_else(|e| panic!("{e}"));
    file.track("item").unwrap().stages[0].park_on[0]
        .options
        .clone()
}

const TWO: &str = r#"              - label: "Take the change back to the first stage"
                answer: "Do the first stage again with this change."
                to: build
                recommended: true
              - label: "Run this stage again"
                to: check
"#;

#[test]
fn an_entry_with_options_parses_in_file_order() {
    let options = options_of(&with_options(TWO));
    assert_eq!(options.len(), 2);
    assert_eq!(options[0].label, "Take the change back to the first stage");
    assert_eq!(
        options[0].answer.as_deref(),
        Some("Do the first stage again with this change.")
    );
    assert_eq!(options[0].to.as_deref(), Some("build"));
    assert!(options[0].recommended);
    assert_eq!(options[1].label, "Run this stage again");
    assert_eq!(options[1].answer, None);
    assert_eq!(options[1].to.as_deref(), Some("check"));
    assert!(!options[1].recommended, "the mark defaults to false");
}

#[test]
fn a_file_with_no_options_parses_as_before_with_an_empty_list() {
    assert!(options_of(&flow("", HOOK)).is_empty());
    // An `options` key makes the entry declare them; none at all declares none.
    assert!(options_of(&flow("", "")).is_empty());
}

#[test]
fn an_answer_option_loads_when_only_the_stage_declares_the_hook() {
    let text = flow(
        FALLBACK,
        "            options:\n              - label: \"Go on\"\n                answer: \"Go on.\"\n",
    );
    let options = options_of(&text);
    assert_eq!(options[0].answer.as_deref(), Some("Go on."));
}

#[test]
fn a_to_only_option_loads_with_no_hook_at_all() {
    let text = flow(
        "",
        "            options:\n              - label: \"Check it\"\n                to: check\n",
    );
    let options = options_of(&text);
    assert_eq!(options[0].answer, None);
    assert_eq!(options[0].to.as_deref(), Some("check"));
}

#[test]
fn an_answer_with_a_line_break_and_a_literal_brace_text_loads_as_written() {
    let text = with_options(
        "              - label: \"Use {\\\"a\\\": 1}\"\n                answer: \"First line.\\nSecond {x y} line with {} and {\\\"b\\\": 2}.\"\n",
    );
    let options = options_of(&text);
    assert_eq!(options[0].label, "Use {\"a\": 1}");
    assert_eq!(
        options[0].answer.as_deref(),
        Some("First line.\nSecond {x y} line with {} and {\"b\": 2}.")
    );
}

// ------------------------------------------------------------------ the refusals

#[test]
fn an_empty_list_is_refused() {
    let text = flow("", &format!("{HOOK}\n            options: []"));
    blames(&text, "options: []", "at least one option");
}

#[test]
fn four_options_are_read() {
    let mut list = String::new();
    for n in 1..=4 {
        list.push_str(&format!(
            "              - label: \"Option number {n}\"\n                to: check\n"
        ));
    }
    assert_eq!(options_of(&with_options(&list)).len(), 4);
}

#[test]
fn five_options_are_refused_at_the_fifth() {
    let mut list = String::new();
    for n in 1..=5 {
        list.push_str(&format!(
            "              - label: \"Option number {n}\"\n                to: check\n"
        ));
    }
    blames(&with_options(&list), "Option number 5", "at most 4");
}

#[test]
fn a_field_the_format_does_not_know_is_refused() {
    for field in ["why", "reason", "id", "effect", "ack", "verb"] {
        let list = format!(
            "              - label: \"Go\"\n                to: check\n                {field}: \"x\"\n"
        );
        let text = with_options(&list);
        let e = refused(&text);
        assert_eq!(e.line, line_of(&text, &format!("{field}: \"x\"")), "{e}");
        assert!(
            e.message.contains(&format!("unknown field `{field}`")),
            "{e}"
        );
    }
}

#[test]
fn an_option_with_neither_an_answer_nor_a_stage_is_refused() {
    let text = with_options(
        "              - label: \"Nothing to do\"\n                recommended: true\n",
    );
    blames(&text, "Nothing to do", "an `answer`, a `to`, or both");
}

#[test]
fn a_label_outside_its_bound_is_refused() {
    let text = with_options("              - label: \"\"\n                to: check\n");
    blames(&text, "label: \"\"", "must not be empty");
    let long = "x".repeat(81);
    let text = with_options(&format!(
        "              - label: \"{long}\"\n                to: check\n"
    ));
    blames(&text, &long, "holds 81 characters; the most is 80");
    // 80 characters is allowed.
    let ok = "x".repeat(80);
    assert_eq!(
        options_of(&with_options(&format!(
            "              - label: \"{ok}\"\n                to: check\n"
        )))[0]
            .label,
        ok
    );
    // A label is one line.
    let text = with_options("              - label: \"Two\\nlines\"\n                to: check\n");
    blames(&text, "Two", "one line");
}

#[test]
fn an_answer_outside_its_bound_is_refused() {
    let long = "y".repeat(2_001);
    let text = with_options(&format!(
        "              - label: \"Go\"\n                answer: \"{long}\"\n"
    ));
    blames(
        &text,
        &long[..40],
        "holds 2001 characters; the most is 2000",
    );
    let ok = "y".repeat(2_000);
    assert_eq!(
        options_of(&with_options(&format!(
            "              - label: \"Go\"\n                answer: \"{ok}\"\n"
        )))[0]
            .answer
            .as_deref(),
        Some(ok.as_str())
    );
    // A control character other than a line break is refused.
    let text = with_options("              - label: \"Go\"\n                answer: \"a\\tb\"\n");
    blames(&text, "answer: \"a", "control character");
    let text = with_options("              - label: \"Go\"\n                answer: \"\"\n");
    blames(&text, "answer: \"\"", "must not be empty");
}

#[test]
fn a_placeholder_in_a_label_or_an_answer_is_refused() {
    for hole in ["{unit.id}", "{param.x}", "{unit.vars.handoff_path}"] {
        let text = with_options(&format!(
            "              - label: \"Run {hole}\"\n                to: check\n"
        ));
        blames(&text, &format!("Run {hole}"), "literal text");
        let text = with_options(&format!(
            "              - label: \"Go\"\n                answer: \"Do {hole}.\"\n"
        ));
        blames(&text, &format!("Do {hole}"), "literal text");
    }
}

#[test]
fn a_text_the_redactor_would_change_is_refused() {
    let secret = "Authorization: Basic abcdef123456";
    assert_ne!(
        redact(secret),
        secret,
        "the case must be one the redactor changes"
    );
    let text = with_options(&format!(
        "              - label: \"Go\"\n                answer: \"{secret}\"\n"
    ));
    blames(&text, "Authorization", "looks like a secret");
    let text = with_options(&format!(
        "              - label: \"{secret}\"\n                to: check\n"
    ));
    blames(&text, "Authorization", "looks like a secret");
}

#[test]
fn a_stage_that_is_not_in_the_same_track_is_refused() {
    let text = with_options("              - label: \"Go\"\n                to: nowhere\n");
    blames(&text, "to: nowhere", "`nowhere`");
    // `elsewhere` is a stage of the track `other`.
    let text = with_options("              - label: \"Go\"\n                to: elsewhere\n");
    let e = refused(&text);
    assert_eq!(e.line, line_of(&text, "to: elsewhere"), "{e}");
    assert!(
        e.message.contains("`elsewhere`") && e.message.contains("track `item`"),
        "{e}"
    );
}

#[test]
fn two_marks_or_a_mark_that_is_not_a_boolean_are_refused() {
    let text = with_options(
        "              - label: \"One\"\n                to: check\n                recommended: true\n              - label: \"Two\"\n                to: check\n                recommended: true\n",
    );
    let e = refused(&text);
    // The second mark is the one blamed.
    let second = text
        .lines()
        .enumerate()
        .filter(|(_, l)| l.contains("recommended: true"))
        .map(|(i, _)| i + 1)
        .nth(1)
        .unwrap();
    assert_eq!(e.line, second, "{e}");
    assert!(e.message.contains("more than one"), "{e}");
    // A mark of false next to a mark of true is fine.
    let ok = with_options(
        "              - label: \"One\"\n                to: check\n                recommended: false\n              - label: \"Two\"\n                to: check\n                recommended: true\n",
    );
    assert!(!options_of(&ok)[0].recommended && options_of(&ok)[1].recommended);
    for bad in ["\"yes\"", "1", "\"true\"", "[true]"] {
        let text = with_options(&format!(
            "              - label: \"One\"\n                to: check\n                recommended: {bad}\n"
        ));
        blames(&text, &format!("recommended: {bad}"), "true or false");
    }
}

#[test]
fn an_answer_option_with_no_hook_on_the_entry_or_the_stage_is_refused() {
    let text = flow(
        "",
        "            options:\n              - label: \"Go\"\n                answer: \"Go on.\"\n",
    );
    blames(&text, "answer: \"Go on.\"", "`on_answer` hook");
}

#[test]
fn a_gaps_file_entry_takes_no_options() {
    let text = flow(
        "",
        &format!(
            "{HOOK}\n            options:\n              - label: \"Go\"\n                to: check\n"
        ),
    )
    .replace("hold.txt", "gaps.json");
    let e = refused(&text);
    assert!(e.message.contains("gaps"), "{e}");
    assert!(e.line >= line_of(&text, "options:"), "{e}");
}

#[test]
fn options_are_refused_everywhere_else() {
    let list = "options:\n  - label: \"Go\"\n    to: check";
    // On a stop_file mapping.
    let text = with_options(TWO).replace(
        "        stop_file: \"{run.dir}/out/{unit.id}/stop\"\n\n        done:",
        "        stop_file:\n          file: \"{run.dir}/out/{unit.id}/stop\"\n          options:\n            - label: \"Go\"\n              to: check\n        done:",
    );
    let e = refused(&text);
    assert!(e.message.contains("unknown field `options`"), "{e}");
    // At the stage.
    let text = flow(
        "        options:\n          - label: \"Go\"\n            to: check",
        "",
    );
    let e = refused(&text);
    assert!(e.message.contains("unknown field `options`"), "{e}");
    // At the track.
    let text = flow("", "").replace(
        "  item:\n    stages:",
        &format!("  item:\n    {}\n    stages:", list.replace('\n', "\n    ")),
    );
    let e = refused(&text);
    assert!(e.message.contains("unknown field `options`"), "{e}");
    // At the top level.
    let text = flow("", "").replace("name: demo-flow\n", &format!("name: demo-flow\n{list}\n"));
    let e = refused(&text);
    assert!(e.message.contains("unknown field `options`"), "{e}");
}

// ------------------------------------------------------------------ a package

fn package(dir: &Path, workflow: &str) {
    std::fs::create_dir_all(dir.join("briefs")).unwrap();
    std::fs::create_dir_all(dir.join("rules")).unwrap();
    std::fs::create_dir_all(dir.join("scripts")).unwrap();
    std::fs::write(dir.join("workflow.yaml"), workflow).unwrap();
    std::fs::write(dir.join("briefs/item.md"), "# item\n").unwrap();
    std::fs::write(
        dir.join("rules/worker.yaml"),
        "version: 1\nrole: worker\nallow:\n  - delete_own_scratch\n",
    )
    .unwrap();
    std::fs::write(dir.join("scripts/note.sh"), "exit 0\n").unwrap();
}

#[test]
fn a_package_with_options_loads_and_pins_the_same_files_as_one_without() {
    let root = tempfile::tempdir().unwrap();
    let plain = root.path().join("plain");
    package(&plain, &flow("", HOOK));
    let optioned = root.path().join("optioned");
    package(&optioned, &with_options(TWO));
    let plain = Package::load(&plain).unwrap_or_else(|e| panic!("{e}"));
    let optioned = Package::load(&optioned).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(plain.pinned_files(), optioned.pinned_files());
    assert_eq!(
        optioned.file.track("item").unwrap().stages[0].park_on[0]
            .options
            .len(),
        2
    );
}

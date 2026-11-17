//! AC-9: the arm summary names each stage that declares nudges, with its cap. A nudge is a turn,
//! not a launch, so the most-launches count does not change. The forge is a stub `gh`; no seat
//! launches.

mod common;

use common::*;
use serde_json::Value;

/// The item `close` stage of the fixture, with `nudges: 2` and a command.
fn close_nudges(nudges: u32) -> String {
    format!(
        "        no_signal:\n          nudge: \"Finish {{unit.id}}.\"\n          nudges: {nudges}\n          run:\n            - [true]\n"
    )
}

fn text_summary(sb: &Sandbox) -> String {
    let mut args = sb.arm_args();
    args.push("--dry-run".into());
    let out = sb.run(&args);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    stdout(&out)
}

fn json_summary(sb: &Sandbox) -> Value {
    let mut args = sb.arm_args();
    args.extend(["--dry-run".into(), "--json".into()]);
    let out = sb.run(&args);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    serde_json::from_str(&stdout(&out)).unwrap()
}

fn with_nudges(nudges: u32) -> Sandbox {
    Sandbox::with(&Options {
        close_extra: close_nudges(nudges),
        ..Options::default()
    })
}

#[test]
fn a_stage_that_declares_nudges_2_prints_its_cap() {
    let sb = with_nudges(2);
    let text = text_summary(&sb);
    let line = "close: up to 2 nudges on no done signal";
    assert!(
        text.lines().any(|l| l == line),
        "missing {line:?} in:\n{text}"
    );
    assert_eq!(
        text.matches("nudge").count(),
        1,
        "one line for the one stage:\n{text}"
    );
}

#[test]
fn one_nudge_reads_in_the_singular() {
    let text = text_summary(&with_nudges(1));
    let line = "close: up to 1 nudge on no done signal";
    assert!(
        text.lines().any(|l| l == line),
        "missing {line:?} in:\n{text}"
    );
}

#[test]
fn the_json_form_is_unchanged_and_names_no_nudge() {
    // The wire summary gains no field: the nudge lines are text, printed after the summary.
    let v = json_summary(&with_nudges(2));
    assert!(v.get("nudges").is_none(), "{v}");
    assert_eq!(v["schema_version"], 1);
}

#[test]
fn a_workflow_with_no_no_signal_prints_no_nudge_line() {
    let sb = Sandbox::new();
    let text = text_summary(&sb);
    assert!(!text.contains("nudge"), "{text}");
}

#[test]
fn a_stage_with_commands_and_no_nudges_prints_no_nudge_line() {
    let sb = Sandbox::with(&Options {
        close_extra: "        no_signal:\n          run:\n            - [true]\n".to_string(),
        ..Options::default()
    });
    assert!(!text_summary(&sb).contains("nudge"));
}

#[test]
fn the_most_launches_count_is_the_same_with_and_without_the_key() {
    let plain = json_summary(&Sandbox::new());
    let nudging = json_summary(&with_nudges(3));
    assert_eq!(plain["most_launches"], 18);
    assert_eq!(nudging["most_launches"], plain["most_launches"]);
    // Additive: the schema version stays.
    assert_eq!(nudging["schema_version"], 1);
    assert_eq!(plain["schema_version"], 1);
    // The count line of the text form is the same too.
    let line = |sb: &Sandbox| {
        text_summary(sb)
            .lines()
            .find(|l| l.starts_with("most launches:"))
            .unwrap()
            .to_string()
    };
    assert_eq!(line(&Sandbox::new()), line(&with_nudges(3)));
}

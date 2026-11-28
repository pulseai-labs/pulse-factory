//! AC-8 of `r2.s3.w2`: `authority.clear_acts` (`docs/contracts/cli.md`, `settings`; ADR-0016,
//! amended). The key is a list of act names, merges by `intersect` (the built-in list of six cut by
//! every stored value on the chain), is set at a project, an agent, or a project and agent, never at
//! the server, reads `next_turn` on the wire, and `settle` reads it fresh at each act.
//!
//! The controller runs in the test process on the fake adapter, with a recording chat.

mod common;
#[path = "common/settle_rig.rs"]
mod settle_rig;

use common::chat::*;
use common::*;
use serde_json::Value;
use settle_rig::*;

const KEY: &str = "authority.clear_acts";

fn get(rig: &SRig, scope: &[&str]) -> Value {
    let mut args = vec!["settings", "get", KEY];
    args.extend_from_slice(scope);
    cli_json(&rig.live, &args)["settings"][0].clone()
}

fn set(rig: &SRig, value: &str, scope: &[&str]) -> std::process::Output {
    let mut args = vec!["settings", "set", KEY, value];
    args.extend_from_slice(scope);
    rig.live.cli(&args)
}

#[test]
fn the_built_in_resolves_to_all_six_acts_and_reads_next_turn() {
    let rig = SRig::new(&["u1"], Some(10));
    let record = get(
        &rig,
        &["--project", &rig.project, "--agent", "orchestrator"],
    );
    assert_eq!(
        record["value"],
        serde_json::json!([
            "clear",
            "resume",
            "rerun_check",
            "redispatch",
            "resolve_conflict",
            "close_stale"
        ])
    );
    assert_eq!(record["merge"], "intersect");
    assert_eq!(record["applies"], "next_turn");
    assert_eq!(record["from"]["kind"], "built_in");
}

#[test]
fn a_project_value_and_an_agent_value_resolve_to_their_intersection() {
    let rig = SRig::new(&["u1"], Some(10));
    let out = set(&rig, r#"["clear","resume"]"#, &["--project", &rig.project]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let out = set(
        &rig,
        r#"["clear","redispatch"]"#,
        &["--agent", "orchestrator"],
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let record = get(
        &rig,
        &["--project", &rig.project, "--agent", "orchestrator"],
    );
    assert_eq!(record["value"], serde_json::json!(["clear"]), "{record}");
    assert_eq!(
        record["layers"].as_array().unwrap().len(),
        2,
        "every stored row"
    );
    assert!(
        record["masked"].as_array().is_none_or(|m| m.is_empty()),
        "no row hides another: {record}"
    );
    // `from` names the most specific stored row: the agent's, as the chain reads agent after project.
    assert_ne!(record["from"]["kind"], "built_in");
}

#[test]
fn an_unknown_act_name_and_a_server_scope_write_are_refused() {
    let rig = SRig::new(&["u1"], Some(10));
    let out = set(&rig, r#"["clear","bogus"]"#, &["--project", &rig.project]);
    assert_eq!(code(&out), 1);
    let err = stderr(&out);
    assert!(
        err.contains("clear, resume, rerun_check, redispatch, resolve_conflict, close_stale"),
        "the refusal names the six: {err}"
    );
    let out = set(&rig, r#"["clear"]"#, &[]);
    assert_eq!(code(&out), 1);
    assert!(
        stderr(&out)
            .contains("authority.clear_acts is set at a project, an agent, or a project and agent"),
        "{}",
        stderr(&out)
    );
    // An empty list is allowed and admits no act.
    let out = set(&rig, "[]", &["--project", &rig.project]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let record = get(
        &rig,
        &["--project", &rig.project, "--agent", "orchestrator"],
    );
    assert_eq!(record["value"], serde_json::json!([]));
}

#[test]
fn a_settle_of_an_act_the_resolved_list_lacks_is_refused_and_admitted_after_the_operator_widens_it()
{
    let rig = SRig::new(&["u1"], Some(10));
    let (id, rev) = rig.request("u1");
    let out = set(&rig, r#"["resume"]"#, &["--project", &rig.project]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = why(&rig.settle(&id, rev, "clear", None, WHY));
    assert!(
        text.contains(&format!(
            "the act clear is not in authority.clear_acts (from project:{})",
            rig.project
        )),
        "{text}"
    );
    assert_eq!(rig.admissions("settle"), 0);
    // The operator widens the value: the same settle is admitted on the same revision.
    let out = set(&rig, r#"["clear","resume"]"#, &["--project", &rig.project]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let resp = rig.settle(&id, rev, "clear", None, WHY);
    assert_eq!(resp["ok"], true, "{resp}");
}

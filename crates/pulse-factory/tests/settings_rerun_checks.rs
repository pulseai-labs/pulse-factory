//! AC-12 of `r2.s3.w3`: `authority.rerun_checks_per_head` (`docs/contracts/cli.md`, `settings`;
//! ADR-0016, amended). The key is a whole number from 0 to 5 with the built-in 1. It merges by
//! `override`, is written at the server, a project, an agent, or a project and agent, reads
//! `next_turn` on the wire, and `settle` reads it fresh at each `rerun_check` (the settle half is
//! `settle_rerun.rs`).
//!
//! The controller runs in the test process on the fake adapter, with a recording chat.

mod common;
#[path = "common/settle_rig.rs"]
mod settle_rig;

use common::chat::*;
use common::*;
use pulse_factory::controller::settings::{AtResume, at_resume_of};
use serde_json::Value;
use settle_rig::*;

const KEY: &str = "authority.rerun_checks_per_head";

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

fn unset(rig: &SRig, scope: &[&str]) {
    let mut args = vec!["settings", "unset", KEY];
    args.extend_from_slice(scope);
    let out = rig.live.cli(&args);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
}

fn resolved(rig: &SRig) -> Value {
    get(rig, &["--project", &rig.project, "--agent", "orchestrator"])
}

#[test]
fn the_built_in_is_one_and_the_key_merges_by_override_and_reads_next_turn() {
    let rig = SRig::new(&["u1"], Some(10));
    let record = resolved(&rig);
    assert_eq!(record["value"], 1, "{record}");
    assert_eq!(record["built_in"], 1);
    assert_eq!(record["from"]["kind"], "built_in");
    assert_eq!(record["merge"], "override");
    assert_eq!(record["applies"], "next_turn");
    assert_eq!(at_resume_of(KEY), Some(AtResume::Current));
    // `settings get` lists it, right after `authority.clear_acts`.
    let all = cli_json(&rig.live, &["settings", "get"]);
    let keys: Vec<&str> = all["settings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["key"].as_str().unwrap())
        .collect();
    let at = keys
        .iter()
        .position(|k| *k == "authority.clear_acts")
        .unwrap();
    assert_eq!(keys[at + 1], KEY, "{keys:?}");
}

#[test]
fn zero_to_five_are_accepted_and_six_and_a_text_are_refused_with_the_rule() {
    let rig = SRig::new(&["u1"], Some(10));
    for n in 0..=5 {
        let out = set(&rig, &n.to_string(), &["--project", &rig.project]);
        assert_eq!(code(&out), 0, "{n}: {}", stderr(&out));
        assert_eq!(resolved(&rig)["value"], n);
    }
    for bad in ["6", "-1", "\"two\"", "1.5", "[1]", "true"] {
        let out = set(&rig, bad, &["--project", &rig.project]);
        assert_eq!(code(&out), 1, "{bad} must be refused");
        assert!(
            stderr(&out).contains(&format!("{KEY} is a whole number from 0 to 5")),
            "{bad}: {}",
            stderr(&out)
        );
    }
    assert_eq!(
        resolved(&rig)["value"],
        5,
        "a refused write changes nothing"
    );
}

#[test]
fn each_of_the_four_scopes_takes_a_value_and_the_most_specific_wins() {
    let rig = SRig::new(&["u1"], Some(10));
    let project = ["--project", rig.project.as_str()];
    let agent = ["--agent", "orchestrator"];
    let both = ["--project", rig.project.as_str(), "--agent", "orchestrator"];
    for (scope, value) in [
        (&[][..], "2"),
        (&project[..], "3"),
        (&agent[..], "4"),
        (&both[..], "5"),
    ] {
        let out = set(&rig, value, scope);
        assert_eq!(code(&out), 0, "{scope:?}: {}", stderr(&out));
    }
    let record = resolved(&rig);
    assert_eq!(record["value"], 5, "{record}");
    assert_eq!(
        record["layers"].as_array().unwrap().len(),
        4,
        "every stored row: {record}"
    );
    // The chain reads "project and agent, project, agent, server": each unset takes the next row.
    unset(&rig, &both);
    assert_eq!(resolved(&rig)["value"], 3);
    unset(&rig, &project);
    assert_eq!(resolved(&rig)["value"], 4);
    unset(&rig, &agent);
    assert_eq!(resolved(&rig)["value"], 2);
    unset(&rig, &[]);
    let record = resolved(&rig);
    assert_eq!(record["value"], 1);
    assert_eq!(record["from"]["kind"], "built_in");
}

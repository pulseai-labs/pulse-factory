//! AC-9 of `r2.s3.w2` (#48 F7): `project add` and `project remove` pass the caller check that the chat
//! verbs pass. A caller below a live chat process, a caller that holds the chat marker and a caller
//! that holds a seat token are each refused, on `project add` and on `project remove` without
//! `end_sessions`, and nothing is written. The operator's CLI is admitted (`docs/contracts/cli.md`,
//! `project`; ADR-0005, amended).
//!
//! The controller runs in the test process with a recording `FakeChat`. A caller that must be below
//! a chat, or carry a marker or a seat token, is this test binary as a helper (`settle_rig`).

mod common;
#[path = "common/settle_rig.rs"]
mod settle_rig;

use common::chat::*;
use common::*;
use pf_engine::port::store::{CONTROLLER_SCOPE, Store};
use serde_json::{Value, json};
use settle_rig::*;

/// What a refused call must leave as it was: the projects, the controller's admissions and the
/// controller's ledger.
fn state_of(rig: &SRig) -> (Vec<String>, usize, usize) {
    let store = rig.live.controller.store();
    (
        store
            .projects()
            .unwrap()
            .iter()
            .map(|p| format!("{p:?}"))
            .collect(),
        store.admissions(CONTROLLER_SCOPE).unwrap().len(),
        controller_ledger(&rig.live).len(),
    )
}

fn refused_with(resp: &Value, verb: &str, because: &str) {
    assert_eq!(resp["ok"], false, "{verb}: {resp}");
    let why = resp["error"].as_str().unwrap_or_default();
    assert!(
        why.starts_with(&format!("{verb} refused:")) && why.contains(because),
        "{verb}: {why}"
    );
}

#[test]
fn a_chat_a_marker_and_a_seat_are_refused_on_project_add_and_project_remove_and_write_nothing() {
    let rig = SRig::new(&["u1"], Some(10));
    // A project with no session, which a remove would take away, and a root that an add would take.
    let plain = add_project(&rig.live, &repo(&rig.live, "beta"));
    let new_root = repo(&rig.live, "gamma").display().to_string();
    let before = state_of(&rig);
    for (verb, args) in [
        (
            "project_add",
            json!({ "root": new_root, "command_id": "pa-1" }),
        ),
        (
            "project_remove",
            json!({ "id": plain, "command_id": "pr-1" }),
        ),
    ] {
        // A caller below the live chat process.
        let resp = rig.call_as(true, &[], verb, &args);
        refused_with(&resp, verb, "descends from, chat process");
        // A caller that holds the chat marker and no chat ancestor.
        let resp = rig.call_as(false, &[(MARKER, &rig.session)], verb, &args);
        refused_with(&resp, verb, MARKER);
        // A caller that holds a seat token.
        let resp = rig.call_as(
            false,
            &[("PF_SEAT_TOKEN", "seat-token-value-1234")],
            verb,
            &args,
        );
        refused_with(&resp, verb, "PF_SEAT_TOKEN");
    }
    assert_eq!(state_of(&rig), before, "no project, admission or line");
}

#[test]
fn a_caller_below_a_chat_is_refused_with_end_sessions_too() {
    // `project_remove` with `end_sessions` was already checked: it stays so.
    let rig = SRig::new(&["u1"], Some(10));
    let before = state_of(&rig);
    let resp = rig.call_as(
        true,
        &[],
        "project_remove",
        &json!({ "id": rig.project, "end_sessions": true, "command_id": "pr-2" }),
    );
    refused_with(&resp, "project_remove", "descends from, chat process");
    assert_eq!(state_of(&rig), before);
}

#[test]
fn the_operators_cli_is_admitted_on_both_verbs() {
    let rig = SRig::new(&["u1"], Some(10));
    let before = state_of(&rig);
    // The CLI runs below no chat and holds no marker or token.
    let added = add_project(&rig.live, &repo(&rig.live, "beta"));
    let out = rig.live.cli(&["project", "remove", &added]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_ne!(state_of(&rig), before, "the admitted calls wrote");
    let row = rig.live.controller.store().project(&added).unwrap();
    let row = row.expect("the row stays");
    assert!(row.removed_at.is_some(), "the project is removed");
}

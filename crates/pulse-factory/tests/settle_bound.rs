//! AC-6 of `r2.s3.w2`: the run holds the armed launch count that `arm` shows, and no act of the clear
//! grant passes it (`docs/contracts/requests.md`, "Settle"; ADR-0003, amended; `seat-launch-spend`,
//! human confirm). `arm` stores the count once, in one controller command, after the run row exists
//! and before it answers. A settle that would pass the count is refused with the request open, and the
//! claim's `clear` is not bound.
//!
//! The controller runs in the test process on the fake adapter.

mod common;
#[path = "common/settle_rig.rs"]
mod settle_rig;

use common::chat::*;
use common::*;
use pf_engine::port::store::Store;
use serde_json::json;
use settle_rig::*;

const HOUR_MS: u64 = 3_600_000;

#[test]
fn arm_stores_the_count_its_summary_shows_and_its_ledger_line_names_it() {
    // A real controller process and the real `arm`, so the count is the one `build_plan` works out.
    let sb = Sandbox::new();
    let _ctl = Ctl::start(&sb, HOUR_MS);
    let mut dry = sb.arm_args();
    dry.extend(["--dry-run".into(), "--json".into()]);
    let out = sb.run(&dry);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let summary: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    let most = summary["most_launches"]
        .as_u64()
        .expect("the summary shows most launches");
    let mut args = sb.arm_args();
    args.push("--yes".into());
    let out = sb.run(&args);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let run = "s1-1";
    let conn = rusqlite::Connection::open(sb.state.join("store.sqlite")).unwrap();
    let held: Option<i64> = conn
        .query_row("SELECT launch_bound FROM runs WHERE id = ?1", [run], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        held,
        Some(most as i64),
        "the run holds the count that arm showed"
    );
    let ledger = std::fs::read_to_string(sb.state.join("controller.ledger")).unwrap();
    let lines: Vec<&str> = ledger
        .lines()
        .filter(|l| l.contains(" run_launch_bound "))
        .collect();
    assert_eq!(lines.len(), 1, "{ledger}");
    assert!(
        lines[0].ends_with(&format!(
            "run_launch_bound {run} most_launches={most} command=run-launch-bound-{run}"
        )),
        "{ledger}"
    );
}

#[test]
fn a_plan_with_unknown_most_launches_stores_none_and_a_launching_settle_is_refused() {
    // `ArmPlan::default` carries no count, as a plan whose workflow has no `plan_check`.
    let rig = SRig::new(&["u1"], None);
    let row = rig.live.controller.store().run(&rig.run).unwrap().unwrap();
    assert_eq!(row.launch_bound, None);
    assert!(
        ledger_kind(&rig.live, "run_launch_bound").is_empty(),
        "nothing was written"
    );
    let (id, rev) = rig.request("u1");
    let text = why(&rig.settle(&id, rev, "clear", None, WHY));
    assert!(
        text.contains(&format!(
            "run {} holds no armed launch count, so the grant cannot launch in it",
            rig.run
        )),
        "{text}"
    );
    assert_eq!(rig.admissions("settle"), 0);
}

fn claim(rig: &SRig) -> String {
    let resp = pf_ipc::call(&rig.live.sb.state, "claim", json!({ "holder": "ann@host" })).unwrap();
    assert!(resp.ok, "{:?}", resp.error);
    let path = rig.live.sb.root.path().join("claim-token-file");
    std::fs::write(&path, resp.result["token"].as_str().unwrap()).unwrap();
    path.display().to_string()
}

#[test]
fn the_claims_clear_uses_up_the_room_and_the_next_launching_settle_is_refused_at_the_bound() {
    // Two units, parked; the room is for one more launch.
    let rig = SRig::new(&["u1", "u2"], None);
    let so_far = rig.admissions("launch") as u64;
    set_bound(&rig.live, &rig.run, so_far + 1);
    let token = claim(&rig);
    // The claim's clear of the first unit resumes it: it counts, though it has not launched.
    let out = rig
        .live
        .cli(&["clear", &rig.run, "u1", "--claim-token-file", &token]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let (id, rev) = rig.request("u2");
    let text = why(&rig.settle(&id, rev, "clear", None, WHY));
    assert!(
        text.contains(&format!(
            "have reached its armed launch count ({}); the grant cannot launch more in this run; the operator can still clear",
            so_far + 1
        )),
        "{text}"
    );
    assert!(text.contains("launches so far"), "{text}");
    // The request stays open and the unit keeps its park.
    assert_eq!(rig.live.unit_status("u2"), "parked");
    assert_eq!(rig.admissions("settle"), 0);
    // The claim's clear is not bound: it is admitted at the count.
    let out = rig
        .live
        .cli(&["clear", &rig.run, "u2", "--claim-token-file", &token]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    // After the ticks the run's admitted launches pass the count by the claim's clear only.
    rig.tick();
    rig.tick();
    assert_eq!(rig.admissions("settle"), 0, "no grant act ran");
}

#[test]
fn one_below_the_count_is_admitted_and_the_unit_counts_once() {
    let rig = SRig::new(&["u1"], None);
    let so_far = rig.admissions("launch") as u64;
    set_bound(&rig.live, &rig.run, so_far + 1);
    let (id, rev) = rig.request("u1");
    let resp = rig.settle(&id, rev, "clear", None, WHY);
    assert_eq!(resp["ok"], true, "{resp}");
    rig.tick();
    rig.tick();
    let launched = rig.admissions("launch") as u64;
    assert!(
        launched <= so_far + 1,
        "the grant's act did not pass the count: {launched}"
    );
}

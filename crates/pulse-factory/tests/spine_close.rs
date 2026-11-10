//! AC-3: the spine close and the merge ack, on a live controller with a `FakeAdapter` and a
//! `BareForge`. Nothing here starts a seat process (but the shells the caller check needs), spends
//! or reaches the network.
//!
//! The run: two items merge into the spine branch; the `spine` seed starts once `rounds.done`
//! exists and every item is done; its seat prints the spine PR; the merge rule `claim_ack` opens a
//! `merge_ack` request; the claim holder acks it through the socket, and the PR merges into `main`
//! at the acked SHA; the run waits for the `end_when` file and ends with its reason.

mod common;

use std::process::Command;

use common::ctl::*;
use common::spine::*;
use common::*;
use pf_adapter_fake::{Scenario, Step};
use pf_engine::port::store::{RequestKind, RequestStatus};
use pf_engine::status::Status;
use pf_forge_bare::PrChecks;
use serde_json::Value;

/// Run the CLI as the child of a shell that the adapter reports as a seat process, when `as_seat`
/// is set. The shell waits for a go file, so the PID is registered before the CLI runs.
fn cli_below_a_shell(live: &Live, as_seat: bool, args: &[&str]) -> (i32, String) {
    let dir = live.sb.root.path().join("below");
    std::fs::create_dir_all(&dir).unwrap();
    let go = dir.join("go");
    let _ = std::fs::remove_file(&go);
    let _ = std::fs::remove_file(dir.join("rc"));
    let script = r#"while [ ! -e "$GO" ]; do sleep 0.02; done
"$BIN" "$@" >"$DIR/out" 2>"$DIR/err"
echo $? >"$DIR/rc""#;
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .arg("sh")
        .args(args)
        .env_clear()
        .env("PATH", live.sb.path_env())
        .env("HOME", live.sb.root.path())
        .env("PF_STATE_DIR", &live.sb.state)
        .env("PF_SHARE_DIR", &live.sb.share)
        .env("GO", &go)
        .env("DIR", &dir)
        .env("BIN", BIN)
        .spawn()
        .unwrap();
    let refs = live.session_refs();
    assert!(!refs.is_empty(), "a tracked seat needs a session row");
    if as_seat {
        live.ports
            .pids
            .lock()
            .unwrap()
            .insert(refs[0].0.clone(), child.id());
    }
    std::fs::write(&go, "").unwrap();
    child.wait().unwrap();
    live.ports.pids.lock().unwrap().clear();
    let rc = std::fs::read_to_string(dir.join("rc")).unwrap();
    let err = std::fs::read_to_string(dir.join("err")).unwrap();
    (rc.trim().parse().unwrap(), err)
}

fn json_of(out: &std::process::Output) -> Value {
    serde_json::from_str(&stdout(out)).unwrap_or_else(|e| panic!("{e}: {}", stdout(out)))
}

#[test]
fn the_spine_seed_starts_after_rounds_done_and_every_item_merges() {
    let s = Spine::start();
    s.live
        .ports
        .fake
        .script("lane", Scenario::Script(vec![cost(1_000), Step::TurnEnd]));
    s.live
        .ports
        .fake
        .script("lane.2", Scenario::Script(vec![cost(2_000), Step::TurnEnd]));
    s.item("u1", 1, 1);
    s.item("u2", 2, 2);
    s.tick();
    assert!(!s.has_unit("spine"), "no item is done, and no rounds.done");

    // The first item merges, the file is absent: the seed waits.
    s.finish_item("u1", 1);
    assert_eq!(s.status_of("u1"), Status::Done);
    s.tick();
    assert!(!s.has_unit("spine"), "rounds.done is missing");

    // The file is there, and the second item still runs: the seed waits.
    s.touch(&s.run_dir().join("rounds.done"));
    s.tick();
    assert_eq!(s.status_of("u2"), Status::Running);
    assert!(!s.has_unit("spine"), "an item is still running");

    // The second item merges: the seed is admitted once.
    s.finish_item("u2", 2);
    assert_eq!(s.status_of("u2"), Status::Done);
    s.tick();
    assert!(s.has_unit("spine"));
    let admitted = s.ledger().matches("admit").count();
    s.tick();
    s.tick();
    assert_eq!(s.ledger().matches("admit").count(), admitted, "once");
}

#[test]
fn the_spine_stage_launches_as_the_close_role_on_the_spine_branch() {
    let s = Spine::start();
    s.item("u1", 1, 1);
    s.finish_item("u1", 1);
    s.touch(&s.run_dir().join("rounds.done"));
    s.tick();
    assert!(s.has_unit("spine"));
    s.tick();
    let plans = s.live.ports.plans.lock().unwrap().clone();
    let plan = plans
        .iter()
        .find(|p| p.unit_id == "spine")
        .expect("the spine stage launched");
    assert_eq!(plan.stage, "spine_close");
    assert_eq!(plan.role, "close");
    assert!(
        plan.settings["rules_file"].ends_with("package/rules/close.yaml"),
        "{:?}",
        plan.settings["rules_file"]
    );
    assert_eq!(plan.settings["own_branch"], SPINE_BRANCH);
    assert_eq!(plan.settings["push_branch"], SPINE_BRANCH);
    assert_eq!(
        plan.settings["report_dir"],
        s.live.sb.spine_dir.display().to_string()
    );
}

#[test]
fn ack_over_the_socket_merges_the_spine_pr_and_the_run_ends_on_its_end_when() {
    let s = Spine::start();
    let fake = &s.live.ports.fake;
    fake.script("lane", Scenario::Script(vec![cost(1_000), Step::TurnEnd]));
    fake.script("lane.2", Scenario::Script(vec![cost(2_000), Step::TurnEnd]));
    fake.script("spine", Scenario::Script(vec![cost(7_000), Step::TurnEnd]));
    // The lane unit comes first: the units its seat adds come in after it.
    s.tick();
    s.item("u1", 1, 1);
    s.item("u2", 2, 2);
    s.tick();
    s.finish_item("u1", 1);
    s.finish_item("u2", 2);
    // The first lane unit ends, the seed runs it again, and the file ends the rounds.
    s.pass("lane", &s.out("lane", "lane.done"));
    s.tick();
    assert!(s.has_unit("lane.2"));
    s.touch(&s.run_dir().join("rounds.done"));
    s.tick();
    assert!(s.has_unit("spine"));
    s.tick();

    // The spine seat prints its PR and ends: the PR is `spine/s1` into `main`.
    s.forge
        .write_pr(9, SPINE_BRANCH, "main", PrChecks::Green)
        .unwrap();
    let head = s.head(SPINE_BRANCH);
    s.touch(&s.out("spine", "pr.url"));
    std::fs::write(
        s.out("spine", "pr.url"),
        format!("file://{}#9\n", s.forge.repo()),
    )
    .unwrap();
    s.live.ports.fake.finish(&s.session_of("spine"));
    s.tick();

    // One `merge_ack` request waits, and `show` names the PR, the base and the head SHA.
    let open = s.requests(RequestKind::MergeAck);
    assert_eq!(open.len(), 1, "{open:?}");
    let id = open[0].id.clone();
    s.tick();
    assert_eq!(
        s.requests(RequestKind::MergeAck).len(),
        1,
        "a tick opens no second"
    );
    let shown = json_of(&s.live.cli(&["show", &id, "--json"]));
    assert_eq!(shown["kind"], "merge_ack");
    assert_eq!(shown["verbs"], serde_json::json!(["ack"]));
    assert_eq!(shown["body"]["base"], "main");
    assert_eq!(shown["body"]["head_branch"], SPINE_BRANCH);
    assert_eq!(shown["body"]["head_sha"], head.as_str());
    let text = stdout(&s.live.cli(&["show", &id]));
    for want in [
        "base: main".to_string(),
        format!("head branch: {SPINE_BRANCH}"),
        format!("head sha: {head}"),
        format!("pulse-factory ack {id} --rev 1"),
    ] {
        assert!(text.contains(&want), "show lacks {want:?}:\n{text}");
    }
    assert!(!text.contains("pulse-factory answer"), "{text}");
    let listed = stdout(&s.live.cli(&["requests"]));
    assert!(
        listed.contains("spine merge waits on the claim's ack:") && listed.contains(&head[..7]),
        "{listed}"
    );
    let main_before = s.head("main");

    // The operator claims. A seat, and a child of a seat, cannot ack. `answer` and `clear` on the
    // request are refused too, naming `ack`.
    assert_eq!(code(&s.live.cli(&["claim", "--holder", "ann@host"])), 0);
    let token = s.token();
    let (rc, err) = cli_below_a_shell(&s.live, true, &["ack", &id, "--rev", "1"]);
    assert_eq!(rc, 1, "{err}");
    assert!(
        err.contains("ack refused") && err.contains("seat process"),
        "{err}"
    );
    assert!(!err.contains(&token));
    let seat = s
        .live
        .sb
        .cmd(&["ack", &id, "--rev", "1"])
        .env("PF_SEAT_TOKEN", "seat-token-value-1234")
        .output()
        .unwrap();
    assert_eq!(code(&seat), 1);
    assert!(stderr(&seat).contains("PF_SEAT_TOKEN is set"));
    let answer = s
        .live
        .cli(&["answer", &id, "--rev", "1", "--text", "merge it"]);
    assert_eq!(code(&answer), 1);
    assert!(stderr(&answer).contains("ack"), "{}", stderr(&answer));
    let clear = s
        .live
        .cli(&["clear", &s.live.handle().info.run_id, "spine"]);
    assert_eq!(code(&clear), 1);
    assert!(stderr(&clear).contains("ack"), "{}", stderr(&clear));
    assert_eq!(
        s.requests(RequestKind::MergeAck)[0].status,
        RequestStatus::Open
    );
    assert_eq!(s.head("main"), main_before, "nothing merged yet");

    // The claim holder acks at the current revision: the PR merges into `main` at the acked SHA.
    let ack = s.live.cli(&["ack", &id, "--rev", "1", "--json"]);
    assert_eq!(code(&ack), 0, "{}", stderr(&ack));
    let acked = json_of(&ack);
    assert_eq!(acked["merged"], true, "{acked}");
    assert_eq!(acked["head_sha"], head.as_str());
    assert_eq!(acked["generation"], 1);
    assert_ne!(s.head("main"), main_before);
    let main = s.head("main");
    git(
        &s.live.bare,
        &["merge-base", "--is-ancestor", head.as_str(), main.as_str()],
    );
    assert_eq!(s.status_of("spine"), Status::Done);
    assert!(
        std::fs::read_to_string(s.run_dir().join("after_merge.log"))
            .unwrap()
            .contains("spine")
    );
    let ledger = s.ledger();
    assert!(
        ledger.contains(&format!("ack {id}@1 by claim 1: verb=ack unit=spine"))
            && ledger.contains(&format!("sha={head}")),
        "{ledger}"
    );
    assert!(
        ledger.contains(&format!(
            "merge of unit spine: repo={} branch={SPINE_BRANCH} sha={head} outcome=landed",
            s.forge.repo()
        )),
        "{ledger}"
    );

    // The run is still active: the second pass has not run. The tick says what it waits for.
    s.pass("lane.2", &s.out("lane.2", "lane.done"));
    s.tick();
    let status = s.live.sb.status_json("");
    assert_eq!(status["state"], "active", "{status}");
    assert!(
        status["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n == "the spine PR is merged: close s1 in the claim session"),
        "{}",
        status["notes"]
    );

    // The end file is there: the run ends with the file's reason.
    s.touch(&s.run_dir().join("closed.flag"));
    s.tick();
    let status = s.live.sb.status_json("");
    assert_eq!(status["state"], "ended");
    assert_eq!(status["end_reason"], "spine s1 closed");

    // `status --json` on the ended run lists every session's cost and the total.
    let sessions = status["sessions"].as_array().unwrap();
    let spine_session = sessions
        .iter()
        .find(|x| x["unit"] == "spine")
        .expect("the spine session is listed");
    assert_eq!(spine_session["role"], "close");
    assert_eq!(spine_session["stage"], "spine_close");
    assert_eq!(spine_session["cost_micros"], 7_000);
    let sum: u64 = sessions
        .iter()
        .map(|x| x["cost_micros"].as_u64().unwrap())
        .sum();
    assert_eq!(status["total"]["cost_micros"].as_u64().unwrap(), sum);
    assert!(sum >= 7_000 + 1_000 + 2_000, "total {sum}");
    for x in sessions {
        assert!(x.get("cost").is_some() && x.get("tokens").is_some(), "{x}");
    }

    // The claim token is in no ledger line, no status output and no request body.
    let mut seen = vec![s.ledger(), status.to_string()];
    seen.push(stdout(&s.live.cli(&["status"])));
    let all = json_of(&s.live.cli(&["requests", "--all", "--json"]));
    seen.push(all.to_string());
    for r in all["requests"].as_array().unwrap() {
        let id = r["id"].as_str().unwrap();
        seen.push(stdout(&s.live.cli(&["show", id, "--json"])));
        seen.push(stdout(&s.live.cli(&["show", id])));
    }
    for text in seen {
        assert!(!text.contains(&token), "the token leaked into:\n{text}");
    }
}

//! AC-7 of `r1.s4.w2`: a paired device arms the run of a start card, over the real API. A live
//! controller with a recording `FakeChat`, the API on `127.0.0.1:0`, two devices paired over pinned TLS,
//! the paper env with a bare forge, and the stream read with a WebSocket client. A proposal is made as
//! in `start_propose.rs`: the CLI runs below a chat shell. Nothing here starts the real `claude`,
//! spends a token or reaches a network beyond `127.0.0.1`.

mod common;

use std::sync::atomic::Ordering;
use std::time::Duration;

use common::chat::*;
use common::start::*;
use common::*;
use serde_json::{Value, json};

const D1: &str = "D-1";

/// A rig with the API and two devices. `tweak` adjusts the controller's config.
fn rig(
    sessions: usize,
    tweak: impl FnOnce(&mut pulse_factory::controller::Config) + Send + 'static,
) -> (Start, String, String) {
    let s = Start::with(StartOptions {
        sessions,
        api: true,
        tweak: Box::new(tweak),
    });
    let d1 = s.pair("phone");
    let d2 = s.pair("tablet");
    (s, d1, d2)
}

/// A proposal from session `n` (0-based): its ID and plan hash.
fn propose_from(s: &Start, n: usize, command: &str) -> (String, String) {
    let reply = s.propose(n, "s1", &["--command-id", command]);
    (
        reply["proposal"].as_str().unwrap().to_string(),
        reply["plan_sha256"].as_str().unwrap().to_string(),
    )
}

fn refusal_of(a: &Answer) -> String {
    a.body["refusal"].as_str().unwrap_or_default().to_string()
}

/// The `api.ledger` lines for the verb `arm` that end in `outcome=refused refusal=<word>`.
fn api_refusals(s: &Start, verb: &str, word: &str) -> Vec<String> {
    s.api_ledger()
        .into_iter()
        .filter(|l| {
            l.contains(&format!("verb={verb} "))
                && l.contains(&format!("outcome=refused refusal={word}"))
        })
        .collect()
}

fn no_run_directory(s: &Start) {
    let runs = s.live.sb.state.join("runs");
    let left: Vec<String> = std::fs::read_dir(&runs)
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    assert!(left.is_empty(), "run directories are left: {left:?}");
}

fn state_of(s: &Start, proposal: &str) -> String {
    s.live.controller.proposal_state(proposal).unwrap()
}

fn snapshot(s: &Start, proposal: &str) -> std::path::PathBuf {
    s.live
        .sb
        .state
        .join("proposals")
        .join(proposal)
        .join("package")
}

/// The `workflow.yaml` of the package that the share directory holds.
fn share_workflow(s: &Start) -> std::path::PathBuf {
    s.share
        .join("workflows")
        .join(PACKAGE)
        .join("workflow.yaml")
}

fn replace_in(file: &std::path::Path, old: &str, new: &str) {
    let text = std::fs::read_to_string(file).unwrap();
    assert!(
        text.contains(old),
        "{} holds no `{old}`:\n{text}",
        file.display()
    );
    std::fs::write(file, text.replace(old, new)).unwrap();
}

/// The `plan_check` line of the installed package, as `install_package` wrote it.
fn plan_check_line(s: &Start) -> String {
    format!(
        "plan_check: [sh, -c, \"touch {}; echo 1\"]",
        s.marker.display()
    )
}

fn content_of(dir: &std::path::Path) -> String {
    let package = pf_workflow::package::Package::load(dir).unwrap();
    pf_workflow::pin::pin_package(&package).unwrap().content
}

fn launches_after_ticks(s: &Start, run: &str) -> usize {
    for _ in 0..5 {
        s.live.controller.tick_all();
        if !s.launches(run).is_empty() {
            break;
        }
    }
    s.launches(run).len()
}

// ---------------------------------------------------------------------- issue, arm, replay

#[test]
fn a_device_arms_a_start_card_and_every_record_agrees() {
    let (s, d1, d2) = rig(2, |_| {});
    let claims_before = s.claims();
    let (first, first_hash) = propose_from(&s, 0, "p1");
    // A second proposal withdraws the first.
    let (proposal, hash) = propose_from(&s, 0, "p2");

    // Issue: a wrong hash, an unknown proposal and a withdrawn one are each `subject_moved`.
    for (what, id, h) in [
        ("wrong-hash", proposal.clone(), "0".repeat(64)),
        ("unknown", "sp-0000000000000000".to_string(), hash.clone()),
        ("withdrawn", first.clone(), first_hash.clone()),
    ] {
        let r = s.confirm_arm(&d1, &id, &h, &format!("c-{what}"));
        assert_eq!(r.status, 422, "{what}: {}", r.body);
        assert_eq!(refusal_of(&r), "subject_moved", "{what}: {}", r.body);
    }
    assert_eq!(api_refusals(&s, "confirm_code", "subject_moved").len(), 3);

    // The stream of S-1 as D-1.
    let mut socket = s.stream(&d1, "?session=S-1&after=0");

    // A code for the open proposal: no claim generation, and the card's facts.
    let issued = s.confirm_arm(&d1, &proposal, &hash, "c-issue");
    assert_eq!(issued.status, 200, "{}", issued.body);
    let code = issued.body["code"].as_str().unwrap().to_string();
    assert!(code.starts_with("pfc1-"), "{code}");
    assert!(issued.body["generation"].is_null(), "{}", issued.body);
    assert!(issued.body["holder"].is_null(), "{}", issued.body);
    assert_eq!(issued.body["start"]["proposal"], proposal.as_str());
    assert_eq!(issued.body["start"]["session"], "S-1");
    assert_eq!(issued.body["start"]["project_id"], s.projects[0].as_str());
    assert_eq!(issued.body["start"]["spine"], "s1");
    assert_eq!(issued.body["start"]["plan_sha256"], hash.as_str());
    assert_eq!(issued.body["subject"]["kind"], "start");
    assert_eq!(issued.body["subject"]["request"], proposal.as_str());
    assert_eq!(s.claims(), claims_before, "the issue moved no claim");
    assert!(s.runs().is_empty(), "the issue arms nothing");

    // Spend it.
    let armed = s.arm_with(&d1, &proposal, &hash, &code, "a-1");
    assert_eq!(armed.status, 200, "{}", armed.body);
    assert_eq!(armed.body["replayed"], false);
    assert_eq!(armed.body["device"], D1);
    assert_eq!(armed.body["proposal"], proposal.as_str());
    let run = armed.body["run_id"].as_str().unwrap().to_string();
    assert_eq!(armed.body["project_id"], s.projects[0].as_str());
    assert_eq!(state_of(&s, &proposal), format!("armed:{run}"));

    // `runs` and `status --json` name the project, and the run is on the paper env.
    let runs = s.runs();
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0]["id"], run.as_str());
    assert_eq!(runs[0]["project_id"], s.projects[0].as_str());
    let status = cli_json(&s.live, &["status"]);
    assert_eq!(status["run"], run.as_str(), "{status}");
    assert_eq!(status["project_id"], s.projects[0].as_str(), "{status}");
    assert_eq!(status["env"], "paper", "{status}");
    let launches = launches_after_ticks(&s, &run);
    assert!(launches >= 1, "the first launch admission does not exist");

    // The stream shows the card's `armed` result with the run and the device.
    let frames = read_until(&mut socket, "the armed result", |f| {
        f.iter().any(|x| {
            x["type"] == "item"
                && x["item"]["body"]["kind"] == "start_result"
                && x["item"]["body"]["result"] == "armed"
        })
    });
    let result = frames
        .iter()
        .find(|x| x["item"]["body"]["result"] == "armed")
        .unwrap();
    assert_eq!(result["item"]["body"]["outcome"], "answered");
    assert_eq!(result["item"]["body"]["run"], run.as_str());
    assert_eq!(result["item"]["body"]["device"], D1);
    let card_seq = s
        .bodies(0, "start_card")
        .iter()
        .find(|(_, b)| b["proposal"] == proposal.as_str())
        .map(|(seq, _)| *seq)
        .unwrap();
    assert_eq!(result["item"]["answers_seq"], card_seq);

    // The ledgers.
    let confirm = s.ledger_with(&format!("confirm_code D-1 proposal={proposal} verb=arm"));
    assert_eq!(confirm.len(), 1, "{confirm:?}");
    let arm_lines = s.ledger_with(&format!("device_arm D-1 proposal={proposal}"));
    assert_eq!(arm_lines.len(), 1, "{arm_lines:?}");
    assert!(arm_lines[0].contains("admitted"), "{}", arm_lines[0]);
    let linked = s.ledger_with(&format!("run_project {run} project=P-1"));
    assert_eq!(linked.len(), 1, "{linked:?}");
    let api = s.api_ledger();
    assert!(
        api.iter()
            .any(|l| l.contains("verb=confirm_code device=D-1") && l.contains("outcome=ok")),
        "{api:#?}"
    );
    assert!(
        api.iter()
            .any(|l| l.contains("verb=arm device=D-1") && l.contains("status=200 outcome=ok")),
        "{api:#?}"
    );

    // A replay of the command ID names the same run and arms nothing again.
    let again = s.arm_with(&d1, &proposal, &hash, &code, "a-1");
    assert_eq!(again.status, 200, "{}", again.body);
    assert_eq!(again.body["replayed"], true, "{}", again.body);
    assert_eq!(again.body["run_id"], run.as_str(), "{}", again.body);
    assert_eq!(s.runs().len(), 1);
    assert_eq!(s.launches(&run).len(), launches, "no launch is duplicated");

    // A new code for an armed proposal: `subject_moved` at issue.
    let late = s.confirm_arm(&d1, &proposal, &hash, "c-late");
    assert_eq!(late.status, 422, "{}", late.body);
    assert_eq!(refusal_of(&late), "subject_moved");
    assert!(
        late.body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("armed"),
        "{}",
        late.body
    );

    // The arm took no claim.
    assert_eq!(s.claims(), claims_before);

    // No secret in any record.
    let surface = s.secret_surface();
    for secret in [&code, &d1, &d2] {
        assert!(!surface.contains(secret.as_str()), "a secret is recorded");
    }
}

#[test]
fn an_arm_while_the_cli_holds_the_claim_leaves_the_claim_with_the_cli() {
    let (s, d1, _d2) = rig(1, |_| {});
    // A claim needs a run: the first card arms one, and the CLI claims it.
    let (first, first_hash) = propose_from(&s, 0, "p1");
    let c1 = s.code_for(&d1, &first, &first_hash, "c1");
    let armed = s.arm_with(&d1, &first, &first_hash, &c1, "a1");
    assert_eq!(armed.status, 200, "{}", armed.body);
    let out = s.live.cli(&["claim", "--holder", "ann@host"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let claims = s.claims();
    assert_eq!(claims.len(), 1);
    // The run ends, and a second card arms while the CLI holds the claim.
    s.live.handle().engine.end_run("done").unwrap();
    let (second, second_hash) = propose_from(&s, 0, "p2");
    let c2 = s.code_for(&d1, &second, &second_hash, "c2");
    let armed = s.arm_with(&d1, &second, &second_hash, &c2, "a2");
    assert_eq!(armed.status, 200, "{}", armed.body);
    assert_eq!(s.claims(), claims, "the arm left the claim with the CLI");
    // The CLI's claim still works after it.
    let out = s.live.cli(&["claim", "--holder", "ann@host"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
}

// ---------------------------------------------------------------------- refusals

#[test]
fn a_foreign_code_and_two_devices_at_once_arm_nothing_or_one_run() {
    let (s, d1, d2) = rig(1, |_| {});
    let (proposal, hash) = propose_from(&s, 0, "p1");

    // `D-2` spends `D-1`'s code.
    let code1 = s.code_for(&d1, &proposal, &hash, "c1");
    let foreign = s.arm_with(&d2, &proposal, &hash, &code1, "a-foreign");
    assert_eq!(foreign.status, 422, "{}", foreign.body);
    assert_eq!(refusal_of(&foreign), "code_foreign", "{}", foreign.body);
    assert_eq!(api_refusals(&s, "arm", "code_foreign").len(), 1);
    assert!(s.runs().is_empty());
    assert_eq!(state_of(&s, &proposal), "open");
    let refused = s.ledger_with("device_arm refused");
    assert!(!refused.is_empty(), "the refusal is audited");
    assert!(
        s.ledger_with("device_arm D-2").is_empty()
            || !s
                .ledger_with("admitted")
                .iter()
                .any(|l| l.contains("device_arm"))
    );

    // Two devices, each with a code for one proposal, spend at once: exactly one run.
    let a = s.code_for(&d1, &proposal, &hash, "c-a");
    let b = s.code_for(&d2, &proposal, &hash, "c-b");
    let (addr, pin) = s.endpoint();
    let body = |code: &str, command: &str| {
        json!({"proposal": proposal, "plan_sha256": hash, "code": code, "command_id": command})
            .to_string()
    };
    let barrier = std::sync::Barrier::new(2);
    let (ra, rb) = std::thread::scope(|scope| {
        let ta = scope.spawn(|| {
            barrier.wait();
            http_at(
                addr,
                pin,
                "POST",
                "/v1/verbs/arm",
                Some(&d1),
                Some(&body(&a, "a-a")),
            )
        });
        let tb = scope.spawn(|| {
            barrier.wait();
            http_at(
                addr,
                pin,
                "POST",
                "/v1/verbs/arm",
                Some(&d2),
                Some(&body(&b, "a-b")),
            )
        });
        (ta.join().unwrap(), tb.join().unwrap())
    });
    let wins: Vec<&Answer> = [&ra, &rb].into_iter().filter(|r| r.status == 200).collect();
    let losses: Vec<&Answer> = [&ra, &rb].into_iter().filter(|r| r.status == 422).collect();
    assert_eq!(wins.len(), 1, "{} / {}", ra.body, rb.body);
    assert_eq!(losses.len(), 1, "{} / {}", ra.body, rb.body);
    assert_eq!(refusal_of(losses[0]), "subject_moved", "{}", losses[0].body);
    assert_eq!(s.runs().len(), 1, "exactly one run");
    assert_eq!(api_refusals(&s, "arm", "subject_moved").len(), 1);
}

#[test]
fn a_code_past_its_time_is_refused_and_arms_nothing() {
    let (s, d1, _d2) = rig(1, |cfg| cfg.confirm_ttl = Duration::from_secs(1));
    let (proposal, hash) = propose_from(&s, 0, "p1");
    let code_text = s.code_for(&d1, &proposal, &hash, "c1");
    std::thread::sleep(Duration::from_millis(1300));
    let r = s.arm_with(&d1, &proposal, &hash, &code_text, "a1");
    assert_eq!(r.status, 422, "{}", r.body);
    assert_eq!(refusal_of(&r), "code_expired", "{}", r.body);
    assert_eq!(api_refusals(&s, "arm", "code_expired").len(), 1);
    assert!(s.runs().is_empty());
    assert_eq!(state_of(&s, &proposal), "open");
}

#[test]
fn an_arm_code_and_a_request_code_each_refuse_on_the_other_verb() {
    let (s, d1, _d2) = rig(1, |_| {});
    let (proposal, hash) = propose_from(&s, 0, "p1");
    let arm_code = s.code_for(&d1, &proposal, &hash, "c-arm");

    // An earlier run with a parked unit gives a request. The proposal stays open.
    s.live.arm_with_units(&["u1"]);
    s.live.park_unit("u1", "stuck");
    let listed = s.verb(&d1, "requests", json!({}));
    assert_eq!(listed.status, 200, "{}", listed.body);
    let request = listed.body["requests"][0].clone();
    let id = request["id"].as_str().unwrap().to_string();
    let rev = request["revision"].as_u64().unwrap();
    let issued = s.verb(
        &d1,
        "confirm_code",
        json!({"subject": {"kind": "request", "request": id, "revision": rev, "verb": "clear"},
               "command_id": "c-request"}),
    );
    assert_eq!(issued.status, 200, "{}", issued.body);
    let request_code = issued.body["code"].as_str().unwrap().to_string();
    let claims = s.claims();

    // A request code spent on `arm`.
    let r = s.arm_with(&d1, &proposal, &hash, &request_code, "a-request-code");
    assert_eq!(r.status, 422, "{}", r.body);
    assert_eq!(refusal_of(&r), "code_subject", "{}", r.body);
    // An arm code spent on `answer`.
    let r = s.verb(
        &d1,
        "answer",
        json!({"request": id, "rev": rev, "text": "x", "code": arm_code, "command_id": "a-answer"}),
    );
    assert_eq!(r.status, 422, "{}", r.body);
    assert_eq!(refusal_of(&r), "code_subject", "{}", r.body);
    assert_eq!(api_refusals(&s, "arm", "code_subject").len(), 1);
    assert_eq!(api_refusals(&s, "answer", "code_subject").len(), 1);
    assert_eq!(s.claims(), claims, "no refusal moved a claim");
    assert_eq!(state_of(&s, &proposal), "open");
}

// ---------------------------------------------------------------------- the snapshot

#[test]
fn a_package_swapped_in_the_share_after_the_card_arms_the_card_and_runs_none_of_it() {
    let (s, d1, _d2) = rig(1, |_| {});
    let (proposal, hash) = propose_from(&s, 0, "p1");
    let recorded = content_of(&snapshot(&s, &proposal));
    let swapped = s.share.join("swapped-ran");
    // The package in the share directory changes: a `plan_check` that would touch a file, and a
    // role's model.
    replace_in(
        &share_workflow(&s),
        &plan_check_line(&s),
        &format!(
            "plan_check: [sh, -c, \"touch {}; echo 1\"]",
            swapped.display()
        ),
    );
    replace_in(
        &share_workflow(&s),
        "model: paper/lane",
        "model: paper/swapped",
    );
    let _ = std::fs::remove_file(&s.marker);

    let code_text = s.code_for(&d1, &proposal, &hash, "c1");
    let armed = s.arm_with(&d1, &proposal, &hash, &code_text, "a1");
    assert_eq!(armed.status, 200, "{}", armed.body);
    let run = armed.body["run_id"].as_str().unwrap().to_string();
    s.live.controller.tick_all();
    assert!(
        !swapped.exists(),
        "the swapped plan_check ran after the card was posted"
    );
    // The run's pin is the card's recorded content, and its roles are the card's.
    let run_dir = s.live.sb.state.join("runs").join(&run);
    let manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("manifest.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["package"]["content"], recorded.as_str());
    let yaml = std::fs::read_to_string(run_dir.join("package/workflow.yaml")).unwrap();
    assert!(yaml.contains("model: paper/lane"), "{yaml}");
    assert!(!yaml.contains("paper/swapped"), "{yaml}");
}

#[test]
fn a_snapshot_that_changed_is_subject_moved_and_nothing_of_it_runs() {
    let (s, d1, _d2) = rig(1, |_| {});
    let (proposal, hash) = propose_from(&s, 0, "p1");
    let tampered = s.share.join("tampered-ran");
    replace_in(
        &snapshot(&s, &proposal).join("workflow.yaml"),
        &plan_check_line(&s),
        &format!(
            "plan_check: [sh, -c, \"touch {}; echo 1\"]",
            tampered.display()
        ),
    );
    let code_text = s.code_for(&d1, &proposal, &hash, "c1");
    let r = s.arm_with(&d1, &proposal, &hash, &code_text, "a1");
    assert_eq!(r.status, 422, "{}", r.body);
    assert_eq!(refusal_of(&r), "subject_moved", "{}", r.body);
    assert!(
        r.body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("the package snapshot changed"),
        "{}",
        r.body
    );
    assert!(!tampered.exists(), "the tampered plan_check ran");
    assert!(s.runs().is_empty());
    no_run_directory(&s);
    assert!(
        s.ledger_with("device_arm D-1")
            .iter()
            .all(|l| !l.contains("admitted")),
        "no device_arm admitted line"
    );
    let results = s.bodies(0, "start_result");
    assert_eq!(results.len(), 1, "{results:?}");
    assert_eq!(results[0].1["result"], "failed");
    assert!(state_of(&s, &proposal).starts_with("failed:"));
}

#[test]
fn a_plan_whose_items_moved_is_subject_moved_naming_items() {
    let (s, d1, _d2) = rig(1, |_| {});
    // A `plan_check` that reads a count file: 1 at the proposal, 2 at the spend.
    let count = s.share.join("count");
    std::fs::write(&count, "1\n").unwrap();
    replace_in(
        &share_workflow(&s),
        &plan_check_line(&s),
        &format!("plan_check: [sh, -c, \"cat {}\"]", count.display()),
    );
    let (proposal, hash) = propose_from(&s, 0, "p1");
    std::fs::write(&count, "2\n").unwrap();
    let code_text = s.code_for(&d1, &proposal, &hash, "c1");
    let r = s.arm_with(&d1, &proposal, &hash, &code_text, "a1");
    assert_eq!(r.status, 422, "{}", r.body);
    assert_eq!(refusal_of(&r), "subject_moved", "{}", r.body);
    assert!(
        r.body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("items"),
        "{}",
        r.body
    );
    assert!(s.runs().is_empty());
    let results = s.bodies(0, "start_result");
    assert_eq!(results.len(), 1, "{results:?}");
    assert_eq!(results[0].1["result"], "failed");
    assert!(
        results[0].1["reason"].as_str().unwrap().contains("items"),
        "{:?}",
        results[0].1
    );
}

#[test]
fn an_arm_with_an_expected_package_that_differs_leaves_no_run() {
    let s = Start::new();
    let mut plan = s.live.plan();
    plan.expect_package = Some("x".to_string());
    let err = s.live.controller.arm(plan).unwrap_err();
    assert!(err.contains("differs"), "{err}");
    assert!(s.runs().is_empty());
    no_run_directory(&s);
}

// ---------------------------------------------------------------------- failure past the admission

#[test]
fn a_run_project_link_that_fails_leaves_no_run_and_a_failed_card() {
    let (s, d1, _d2) = rig(1, |_| {});
    let (proposal, hash) = propose_from(&s, 0, "p1");
    let code_text = s.code_for(&d1, &proposal, &hash, "c1");
    s.live
        .controller
        .start_faults()
        .run_project
        .store(true, Ordering::SeqCst);
    let r = s.arm_with(&d1, &proposal, &hash, &code_text, "a1");
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(
        r.body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("arm refused"),
        "{}",
        r.body
    );
    let admitted = s.ledger_with(&format!("device_arm D-1 proposal={proposal}"));
    assert_eq!(admitted.len(), 1, "{admitted:?}");
    assert!(admitted[0].contains("admitted"), "{}", admitted[0]);
    assert!(
        s.ledger_with("run_project ").is_empty(),
        "no run_project line"
    );
    assert!(s.runs().is_empty(), "{:?}", s.runs());
    no_run_directory(&s);
    assert!(state_of(&s, &proposal).starts_with("failed:"));
    let results = s.bodies(0, "start_result");
    assert_eq!(results.len(), 1, "{results:?}");
    assert_eq!(results[0].1["result"], "failed");
    assert!(results[0].1["reason"].as_str().unwrap().contains("project"));
    // A new code for it is `subject_moved`.
    let again = s.confirm_arm(&d1, &proposal, &hash, "c2");
    assert_eq!(again.status, 422, "{}", again.body);
    assert_eq!(refusal_of(&again), "subject_moved");
    assert!(
        again.body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("failed"),
        "{}",
        again.body
    );
}

#[test]
fn a_start_result_that_fails_to_append_is_tried_again_and_a_restart_recovers_it() {
    let (s, d1, _d2) = rig(1, |_| {});
    // One injected failure: the run exists, the proposal is `Armed`, the next tick appends.
    let (proposal, hash) = propose_from(&s, 0, "p1");
    let code_text = s.code_for(&d1, &proposal, &hash, "c1");
    s.live
        .controller
        .start_faults()
        .result_append
        .store(1, Ordering::SeqCst);
    let armed = s.arm_with(&d1, &proposal, &hash, &code_text, "a1");
    assert_eq!(armed.status, 200, "{}", armed.body);
    let run = armed.body["run_id"].as_str().unwrap().to_string();
    assert_eq!(state_of(&s, &proposal), format!("armed:{run}"));
    assert!(s.bodies(0, "start_result").is_empty(), "the append failed");
    let again = s.confirm_arm(&d1, &proposal, &hash, "c-again");
    assert_eq!(again.status, 422, "{}", again.body);
    assert_eq!(refusal_of(&again), "subject_moved");
    assert!(
        again.body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("already armed"),
        "{}",
        again.body
    );
    assert_eq!(s.runs().len(), 1, "never a second arm");
    s.live.controller.tick_all();
    let results = s.bodies(0, "start_result");
    assert_eq!(results.len(), 1, "{results:?}");
    assert_eq!(results[0].1["result"], "armed");
    assert_eq!(results[0].1["run"], run.as_str());
}

#[test]
fn two_failed_appends_and_a_restart_give_the_card_armed_not_withdrawn() {
    let (s, d1, _d2) = rig(1, |_| {});
    // Two injected failures, then a controller reopened on the same state directory: the card gets
    // `armed` from the `run_project` admission, never `withdrawn`.
    let (second, second_hash) = propose_from(&s, 0, "p2");
    let second_code = s.code_for(&d1, &second, &second_hash, "c2");
    s.live
        .controller
        .start_faults()
        .result_append
        .store(2, Ordering::SeqCst);
    let armed = s.arm_with(&d1, &second, &second_hash, &second_code, "a2");
    assert_eq!(armed.status, 200, "{}", armed.body);
    let run2 = armed.body["run_id"].as_str().unwrap().to_string();
    s.live.controller.tick_all(); // the second injected failure
    assert!(s.bodies(0, "start_result").is_empty(), "still no result");
    s.live.handle().engine.end_run("done").unwrap();
    let mut cfg =
        pulse_factory::controller::Config::new(s.live.sb.state.clone(), 3_600_000, BIN.into());
    cfg.chat_share_dir = s.share.clone();
    let _reopened = pulse_factory::controller::Controller::open(cfg, s.live.ports.clone()).unwrap();
    let results = s.bodies(0, "start_result");
    assert_eq!(results.len(), 1, "{results:?}");
    assert_eq!(results[0].1["result"], "armed", "{:?}", results[0].1);
    assert_eq!(results[0].1["run"], run2.as_str());
}

#[test]
fn a_transient_cause_reopens_the_proposal_and_a_fresh_code_arms_once_the_earlier_run_ends() {
    let (s, d1, _d2) = rig(1, |_| {});
    let (proposal, hash) = propose_from(&s, 0, "p1");
    let code_text = s.code_for(&d1, &proposal, &hash, "c1");
    // An earlier run, armed by the operator's path, is active.
    let earlier = s.live.arm();
    let r = s.arm_with(&d1, &proposal, &hash, &code_text, "a1");
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(
        r.body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("not at an end state"),
        "{}",
        r.body
    );
    let lines = s.ledger_with("device_arm");
    assert!(
        lines.iter().any(|l| l.contains("refused")),
        "a refused device_arm line: {lines:?}"
    );
    assert!(
        lines.iter().all(|l| !l.contains("admitted")),
        "no admitted line: {lines:?}"
    );
    assert_eq!(state_of(&s, &proposal), "open");
    assert!(s.bodies(0, "start_result").is_empty(), "no result");

    // The earlier run ends, and a fresh code arms once.
    s.live.handle().engine.end_run("done").unwrap();
    let fresh = s.code_for(&d1, &proposal, &hash, "c2");
    let armed = s.arm_with(&d1, &proposal, &hash, &fresh, "a2");
    assert_eq!(armed.status, 200, "{}", armed.body);
    assert_ne!(armed.body["run_id"], earlier.as_str());
}

// ---------------------------------------------------------------------- revoke, halt, the socket

#[test]
fn a_revoked_device_is_refused_on_confirm_code_and_arm() {
    let (s, d1, d2) = rig(1, |_| {});
    let (proposal, hash) = propose_from(&s, 0, "p1");
    let held = s.code_for(&d2, &proposal, &hash, "c-held");
    let out = s.live.cli(&["device", "revoke", "D-2"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let r = s.confirm_arm(&d2, &proposal, &hash, "c-after");
    assert_eq!(r.status, 401, "{}", r.body);
    let r = s.arm_with(&d2, &proposal, &hash, &held, "a-after");
    assert_eq!(r.status, 401, "{}", r.body);
    assert!(s.runs().is_empty());
    // The other device is not affected.
    let code_text = s.code_for(&d1, &proposal, &hash, "c-d1");
    let armed = s.arm_with(&d1, &proposal, &hash, &code_text, "a-d1");
    assert_eq!(armed.status, 200, "{}", armed.body);
}

#[test]
fn halt_halts_the_run_and_withdraws_an_open_proposal_before_the_chat_stops() {
    let (s, d1, _d2) = rig(2, |_| {});
    let (first, first_hash) = propose_from(&s, 0, "p1");
    let (second, _) = propose_from(&s, 1, "p2");
    let code_text = s.code_for(&d1, &first, &first_hash, "c1");
    let armed = s.arm_with(&d1, &first, &first_hash, &code_text, "a1");
    assert_eq!(armed.status, 200, "{}", armed.body);

    let halted = cli_json(&s.live, &["halt", "--reason", "test"]);
    assert_eq!(halted["halted"], true, "{halted}");
    let withdrawn: Vec<&str> = halted["proposals_withdrawn"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(withdrawn.contains(&second.as_str()), "{halted}");
    let results = s.bodies(1, "start_result");
    assert_eq!(results.len(), 1, "{results:?}");
    assert_eq!(results[0].1["result"], "withdrawn");
    assert_eq!(results[0].1["reason"], "halted", "not the session's end");
    // The withdrawal comes before any notice of the stopped chat.
    let result_seq = results[0].0;
    for (seq, _) in s.bodies(1, "notice") {
        assert!(
            result_seq < seq,
            "the notice {seq} came before {result_seq}"
        );
    }
}

#[test]
fn halt_with_no_run_withdraws_an_open_proposal_too() {
    let s = Start::new();
    let (proposal, _) = propose_from(&s, 0, "p1");
    let halted = cli_json(&s.live, &["halt", "--reason", "test"]);
    assert!(halted["run"].is_null(), "{halted}");
    assert_eq!(halted["proposals_withdrawn"], json!([proposal]));
    let results = s.bodies(0, "start_result");
    assert_eq!(results[0].1["reason"], "halted");
}

#[test]
fn arm_on_the_socket_with_a_code_and_no_plan_is_refused() {
    let s = Start::new();
    let r = pf_ipc::call(&s.live.sb.state, "arm", json!({"code": "pfc1-0"})).unwrap();
    assert!(!r.ok, "{r:?}");
    assert!(
        r.error
            .unwrap_or_default()
            .contains("confirm codes are for paired devices"),
        "refused"
    );
}

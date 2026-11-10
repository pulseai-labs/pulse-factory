//! AC-6 of `r1.s4.w2`: `propose` from the Orchestrator chat posts a start card and starts nothing.
//! The controller finds the calling chat by the ancestry of the caller (the chat marker only
//! refuses), copies the package into a snapshot, builds the plan in its own process, checks it with
//! the dry run `arm` makes, audits every call, and bounds the builds per session and minute.
//!
//! The controller runs in the test process with a recording `FakeChat`. A chat is a real shell whose
//! PID the test registers in `Rec.pids`, and the CLI runs below it. Nothing starts the real `claude`,
//! spends a token or reaches a network beyond `127.0.0.1`.

mod common;

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use common::chat::*;
use common::start::*;
use common::*;
use pf_engine::port::store::{CONTROLLER_SCOPE, Store};
use pf_ipc::{Handler, Request};
use serde_json::{Value, json};

/// Not a test: the process that `call_below` starts. Run on its own it does nothing.
#[test]
fn helper_calls_the_controller() {
    let Ok(state) = std::env::var("PF_TEST_HELPER_STATE") else {
        return;
    };
    let verb = std::env::var("PF_TEST_HELPER_VERB").unwrap();
    let args: Value = serde_json::from_str(&std::env::var("PF_TEST_HELPER_ARGS").unwrap()).unwrap();
    let resp = pf_ipc::call(Path::new(&state), &verb, args).unwrap();
    println!("HELPER_RESPONSE:{}", serde_json::to_string(&resp).unwrap());
}

fn proposals_left(s: &Start) -> Vec<String> {
    std::fs::read_dir(s.live.sb.state.join("proposals"))
        .map(|d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn refused_lines(s: &Start) -> Vec<String> {
    s.ledger_with("propose refused")
}

fn is_hex(text: &str, n: usize) -> bool {
    text.len() == n
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `arm s1 --dry-run --json` from the operator's CLI, with the same inputs.
fn dry_run(s: &Start) -> Value {
    let out = Command::new(BIN)
        .args(s.arm_dry_run_args("s1"))
        .env_clear()
        .env("PATH", s.live.sb.path_env())
        .env("HOME", s.live.sb.root.path())
        .env("PF_STATE_DIR", &s.live.sb.state)
        .env("PF_SHARE_DIR", &s.share)
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    serde_json::from_str(&stdout(&out)).unwrap()
}

#[test]
fn propose_posts_a_card_with_the_dry_run_plan_and_arms_nothing() {
    let s = Start::new();
    let mut frames = s.live.controller.subscribe_frames();
    let reply = s.propose(0, "s1", &[]);
    let proposal = reply["proposal"].as_str().unwrap().to_string();
    assert!(
        proposal.starts_with("sp-") && is_hex(&proposal[3..], 16),
        "{proposal}"
    );
    assert_eq!(reply["session"], "S-1");
    assert_eq!(reply["project_id"], s.projects[0]);
    let hash = reply["plan_sha256"].as_str().unwrap();
    assert!(is_hex(hash, 64), "{hash}");
    assert_eq!(reply["replayed"], false);

    // One card, with the same proposal and hash, and an `item` frame went out on the stream.
    let cards = s.bodies(0, "start_card");
    assert_eq!(cards.len(), 1, "{cards:?}");
    let card = &cards[0].1;
    assert_eq!(card["proposal"], proposal.as_str());
    assert_eq!(card["plan_sha256"], hash);
    let mut saw_frame = false;
    while let Ok(frame) = frames.try_recv() {
        let v = serde_json::to_value(&frame).unwrap();
        if v["type"] == "item" && v["item"]["body"]["kind"] == "start_card" {
            assert_eq!(v["item"]["body"]["proposal"], proposal.as_str());
            saw_frame = true;
        }
    }
    assert!(saw_frame, "no item frame for the card");

    // The summary is the summary of the operator's dry run.
    let dry = dry_run(&s);
    assert_eq!(
        card["summary"], dry,
        "the card's summary equals arm --dry-run"
    );
    assert_eq!(card["summary"]["items"], 1);
    assert_eq!(card["summary"]["roles"].as_array().unwrap().len(), 6);
    assert!(card["summary"]["most_launches"].is_u64());
    let sentences: Vec<String> = card["summary"]["merges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["sentence"].as_str().unwrap().to_string())
        .collect();
    assert!(
        sentences.iter().any(|t| t.contains("item PRs merge into")),
        "{sentences:?}"
    );
    assert!(
        sentences
            .iter()
            .any(|t| t.contains("only on the claim's ack")),
        "{sentences:?}"
    );
    assert_eq!(card["ledger"], s.ledger.display().to_string());
    assert_eq!(card["plugin_roots"], json!([]));
    assert_eq!(card["params"]["spine_branch"], "spine-s1");
    assert_eq!(card["params"].as_object().unwrap().len(), 4);

    // Nothing is armed: no run, no session row, no launch admission.
    assert!(s.runs().is_empty());
    assert!(s.live.controller.handles().is_empty());
    assert_eq!(s.live.ports.plans.lock().unwrap().len(), 0);

    // The snapshot holds the package's pinned files.
    let snap = s
        .live
        .sb
        .state
        .join("proposals")
        .join(&proposal)
        .join("package");
    assert!(snap.join("workflow.yaml").is_file(), "{}", snap.display());
    assert!(snap.join("scripts/rule.sh").is_file());
    assert!(snap.join("briefs/common.md").is_file());

    // The line, and `session items`.
    let lines = s.ledger_with(&format!("propose S-1 proposal={proposal}"));
    assert_eq!(lines.len(), 1, "{lines:?}");
    for part in [
        "project=P-1",
        "spine=s1",
        "workflow=paper-spine-d20",
        "items=1",
        "plan=",
    ] {
        assert!(lines[0].contains(part), "{part}: {}", lines[0]);
    }
    let items = cli_json(&s.live, &["session", "items", "S-1"]);
    assert!(
        items["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["body"]["kind"] == "start_card")
    );
    let out = s.live.cli(&["session", "items", "S-1"]);
    assert!(
        stdout(&out).contains(&format!(
            "start s1 proposal={proposal} items=1 most_launches="
        )),
        "{}",
        stdout(&out)
    );

    // The persona: `propose`, and never `arm`.
    let persona = s.rec.last_open().instructions;
    assert!(persona.contains("pulse-factory propose"), "{persona}");
    assert!(persona.contains("never run pulse-factory arm"), "{persona}");

    // `settings get` lists three keys.
    let settings = cli_json(&s.live, &["settings", "get"]);
    assert_eq!(
        settings["settings"].as_array().unwrap().len(),
        3,
        "{settings}"
    );
}

#[test]
fn the_caller_is_found_by_ancestry_and_the_marker_only_refuses() {
    let s = Start::with(StartOptions {
        sessions: 2,
        ..StartOptions::default()
    });
    let before = refused_lines(&s);
    assert!(before.is_empty());
    let mut expected = 0;

    // A child detached by a double fork keeps the marker and runs under no live chat process.
    let mut args = s.propose_args("s1");
    args.extend(["--command-id".to_string(), "mine".to_string()]);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let ran = s.below(0, Style::DoubleFork, true, Some("S-1"), BIN, &args, &[]);
    assert_eq!(ran.rc, 1, "{}", ran.err);
    assert!(
        ran.err
            .contains("holds PULSE_FACTORY_CHAT but runs under no live chat process"),
        "{}",
        ran.err
    );
    assert!(
        !ran.chain.contains(&ran.shell),
        "the child's chain {:?} holds the chat shell {}",
        ran.chain,
        ran.shell
    );
    expected += 1;

    // A marker that names S-2 while the walk finds S-1.
    let ran = s.below(0, Style::Direct, true, Some("S-2"), BIN, &args, &[]);
    assert_eq!(ran.rc, 1, "{}", ran.err);
    assert!(
        ran.err
            .contains("the chat marker names S-2, but the caller runs under chat process"),
        "{}",
        ran.err
    );
    expected += 1;

    // A marker alone, naming a live session, from a shell that is not registered.
    let ran = s.below(0, Style::Direct, false, Some("S-1"), BIN, &args, &[]);
    assert_eq!(ran.rc, 1, "{}", ran.err);
    assert!(
        ran.err.contains("runs under no live chat process"),
        "{}",
        ran.err
    );
    expected += 1;

    // A caller under no chat.
    let ran = s.below(0, Style::Direct, false, None, BIN, &args, &[]);
    assert_eq!(ran.rc, 1, "{}", ran.err);
    assert!(
        ran.err.contains("runs under no chat session"),
        "{}",
        ran.err
    );
    expected += 1;

    // A caller with PF_SEAT_TOKEN, by the raw call (the CLI refuses it first).
    let ran = s.call_below(
        0,
        Style::Direct,
        true,
        Some("S-1"),
        "propose",
        &json!({"inputs": s.inputs("s1"), "command_id": "mine"}),
    );
    // No token yet: the raw call is a chat's own, and it is accepted. A seat token comes next.
    assert!(ran.response()["ok"].as_bool().unwrap(), "{}", ran.out);
    assert_eq!(s.bodies(0, "start_card").len(), 1);
    let seat = s.below(
        0,
        Style::Direct,
        true,
        Some("S-1"),
        std::env::current_exe().unwrap().to_str().unwrap(),
        &[
            "--exact",
            "helper_calls_the_controller",
            "--nocapture",
            "--test-threads=1",
        ],
        &[
            (
                "PF_TEST_HELPER_STATE",
                s.live.sb.state.display().to_string(),
            ),
            ("PF_TEST_HELPER_VERB", "propose".to_string()),
            (
                "PF_TEST_HELPER_ARGS",
                json!({"inputs": s.inputs("s1"), "command_id": "seat1"}).to_string(),
            ),
            ("PF_SEAT_TOKEN", "tok".to_string()),
        ],
    );
    let reply = seat.response();
    assert_eq!(reply["ok"], false, "{reply}");
    assert!(
        reply["error"].as_str().unwrap().contains("PF_SEAT_TOKEN"),
        "{reply}"
    );
    expected += 1;

    // `propose` through `Controller::handle`, with no peer.
    let resp = s.live.controller.handle(Request::new(
        "propose",
        json!({"inputs": s.inputs("s1"), "command_id": "nopeer"}),
    ));
    assert!(!resp.ok);
    assert!(
        resp.error
            .unwrap()
            .contains("the controller cannot tell which process called"),
        "no peer"
    );

    // Exactly one refused admission line for each refusal that reached the controller with a peer, each
    // under a `propose-refused-` command ID, with the caller's key; the caller's own ID stays unused
    // (the admitted `mine` above was accepted after the refusals used no ID of theirs).
    let lines = refused_lines(&s);
    assert_eq!(lines.len(), expected, "{lines:#?}");
    for line in &lines {
        assert!(line.contains("command=propose-refused-"), "{line}");
        assert!(line.contains(" caller="), "{line}");
    }
    assert!(
        lines.iter().any(|l| l.contains("caller=seat-token")),
        "{lines:#?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("caller=chat:S-1")),
        "{lines:#?}"
    );
    // Only the one admitted call posted a card.
    assert_eq!(s.bodies(0, "start_card").len(), 1);
    assert!(
        s.live
            .controller
            .store()
            .admission(CONTROLLER_SCOPE, "mine")
            .unwrap()
            .is_some_and(|a| a.kind == "propose"
                && a.outcome == pf_engine::port::store::AdmissionOutcome::Admitted),
        "the ID `mine` went to the admitted call"
    );
}

#[test]
fn the_inputs_are_narrowed_and_a_failed_build_posts_no_card() {
    let s = Start::new();
    // Many calls in one minute: the bound is off for this test (`0`).
    let out = s
        .live
        .cli(&["settings", "set", "chat.propose_per_min", "0"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let shell = s.chat_shell(0, true, Some("S-1"), &[]);
    let call = |inputs: Value, id: &str| {
        s.call_as(
            &shell,
            "propose",
            json!({"inputs": inputs, "command_id": id}),
        )
    };
    let mut n = 0;
    let mut refused = |inputs: Value, needle: &str| {
        n += 1;
        let r = call(inputs, &format!("c{n}"));
        assert!(!r.ok, "{needle}: {:?}", r.result);
        let why = r.error.unwrap();
        assert!(why.starts_with("propose refused:"), "{why}");
        assert!(why.contains(needle), "{needle}: {why}");
        assert!(
            s.bodies(0, "start_card").is_empty(),
            "{needle}: a card was posted"
        );
        assert!(
            proposals_left(&s).is_empty(),
            "{needle}: a snapshot is left"
        );
    };
    // The workflow by installed name only, sent raw.
    let mut i = s.inputs("s1");
    i["workflow"] = json!("../workflows/paper-spine-d20");
    refused(i, "takes the name of an installed workflow");
    // A relative ledger, sent raw.
    let mut i = s.inputs("s1");
    i["ledger"] = json!("ledger.md");
    refused(i, "is not an absolute path");
    // `replace`, sent raw.
    let mut i = s.inputs("s1");
    i["replace"] = json!("s1-1");
    refused(i, "replacing a run is the operator's");
    // An unknown workflow.
    let mut i = s.inputs("s1");
    i["workflow"] = json!("no-such-workflow");
    refused(i, "the workflow does not load");
    // A missing parameter.
    let mut i = s.inputs("s1");
    i["params"] = json!(["spine_branch=spine-s1"]);
    refused(i, "is declared but not given");
    // A bad spine name.
    let mut i = s.inputs("s1");
    i["spine"] = json!("a b");
    refused(i, "must match");
    // The CLI refuses `--replace` with exit 2 (an unexpected argument) and a path workflow with 1.
    let mut args = s.propose_args("s1");
    args.extend(["--replace".to_string(), "s1-1".to_string()]);
    let ran = s.cli_below(0, &args);
    assert_eq!(ran.rc, 2, "{}", ran.err);
    assert!(ran.err.contains("--replace"), "{}", ran.err);
    let mut args = s.propose_args("s1");
    let at = args.iter().position(|a| a == PACKAGE).unwrap();
    args[at] = "workflows/paper-spine-d20".to_string();
    let ran = s.cli_below(0, &args);
    assert_eq!(ran.rc, 1, "{}", ran.err);
    assert!(
        ran.err.contains(
            "propose refused: --workflow takes the name of an installed workflow, not a path"
        ),
        "{}",
        ran.err
    );
    assert!(s.bodies(0, "start_card").is_empty());

    // A failed plan_check.
    let yaml = s
        .share
        .join("workflows")
        .join(PACKAGE)
        .join("workflow.yaml");
    let text = std::fs::read_to_string(&yaml).unwrap();
    std::fs::write(
        &yaml,
        text.replace(&format!("touch {}; echo 1", s.marker.display()), "exit 3"),
    )
    .unwrap();
    refused(s.inputs("s1"), "plan_check");
    std::fs::write(&yaml, &text).unwrap();

    // An active earlier run.
    s.live.arm();
    let mut i = s.inputs("s1");
    i["spine"] = json!("s9");
    refused(i, "is not at an end state");
    s.live
        .controller
        .handles()
        .last()
        .unwrap()
        .engine
        .end_run("test")
        .unwrap();

    // An ended session (`project remove` refuses while a chat is live, so the removed-project cause
    // is not reachable from a live chat; the controller still checks it). The end stops the chat
    // process, so the walk finds no live chat process, and the caller that still holds the marker is
    // refused as the marker-only case.
    let out = s.live.cli(&["session", "end", "S-1"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    refused(s.inputs("s1"), "runs under no live chat process");
    // Every one of those is one refused line, and none used a caller's ID.
    assert!(refused_lines(&s).len() >= 9, "{:#?}", refused_lines(&s));
}

#[test]
fn the_bound_is_a_setting_and_a_burst_costs_two_lines() {
    let s = Start::with(StartOptions {
        tweak: Box::new(|cfg| cfg.refusal_window = Duration::from_secs(4)),
        ..StartOptions::default()
    });
    let shell = s.chat_shell(0, true, Some("S-1"), &[]);
    // The built-in 4: a fifth proposal within a minute is refused, and no plan is built.
    for n in 0..4 {
        let r = s.propose_as(&shell, "s1", &format!("a{n}"));
        assert!(r.ok, "{n}: {:?}", r.error);
    }
    let _ = std::fs::remove_file(&s.marker);
    let r = s.propose_as(&shell, "s1", "a4");
    assert!(!r.ok);
    let why = r.error.unwrap();
    assert!(why.contains("propose_rate"), "{why}");
    assert!(why.contains("chat.propose_per_min = 4"), "{why}");
    assert!(why.contains("from built_in"), "{why}");
    assert!(!s.marker.exists(), "a plan was built for a refused call");

    // A burst of 50 over the bound writes one line at once, and the count at the window's close.
    for n in 0..50 {
        let r = s.propose_as(&shell, "s1", &format!("burst{n}"));
        assert!(!r.ok);
    }
    let lines = s.ledger_with("bucket=propose_rate");
    assert_eq!(lines.len(), 1, "{lines:#?}");
    assert!(lines[0].contains("caller=chat:S-1"), "{}", lines[0]);
    let end = Instant::now() + Duration::from_secs(20);
    while s.ledger_with("bucket=propose_rate").len() < 2 {
        assert!(
            Instant::now() < end,
            "the window never closed: {:#?}",
            s.ledger_with("propose")
        );
        std::thread::sleep(Duration::from_millis(500));
        s.live.controller.tick_all();
    }
    let lines = s.ledger_with("bucket=propose_rate");
    assert_eq!(lines.len(), 2, "{lines:#?}");
    assert!(lines[1].contains("refused=50"), "the count: {}", lines[1]);
    drop(shell);

    // With `settings set chat.propose_per_min 1 --project P-1`, the second is refused.
    let s = Start::new();
    let out = s.live.cli(&[
        "settings",
        "set",
        "chat.propose_per_min",
        "1",
        "--project",
        &s.projects[0],
    ]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let shell = s.chat_shell(0, true, Some("S-1"), &[]);
    assert!(s.propose_as(&shell, "s1", "b0").ok);
    let r = s.propose_as(&shell, "s1", "b1");
    assert!(!r.ok);
    let why = r.error.unwrap();
    assert!(
        why.contains("propose_rate") && why.contains("chat.propose_per_min = 1"),
        "{why}"
    );
    assert!(
        why.contains(&format!("project:{}", s.projects[0])) || why.contains(&s.projects[0]),
        "{why}"
    );
    drop(shell);

    // With 0 the bound is off: ten in a row all admit.
    let s = Start::new();
    let out = s
        .live
        .cli(&["settings", "set", "chat.propose_per_min", "0"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let shell = s.chat_shell(0, true, Some("S-1"), &[]);
    for n in 0..10 {
        let r = s.propose_as(&shell, "s1", &format!("z{n}"));
        assert!(r.ok, "{n}: {:?}", r.error);
    }
    // A value out of range is refused.
    let out = s
        .live
        .cli(&["settings", "set", "chat.propose_per_min", "61"]);
    assert_eq!(code(&out), 1);
}

#[test]
fn replay_supersede_and_withdrawals() {
    let s = Start::new();
    let first = s.propose(0, "s1", &["--command-id", "p1"]);
    let again = s.propose(0, "s1", &["--command-id", "p1"]);
    assert_eq!(again["replayed"], true);
    assert_eq!(again["proposal"], first["proposal"]);
    assert_eq!(s.bodies(0, "start_card").len(), 1, "one card");
    assert_eq!(
        s.ledger_with("propose S-1 proposal=").len(),
        1,
        "one propose line"
    );

    // A second proposal supersedes the first: the first card gets a withdrawn result.
    let second = s.propose(0, "s1", &["--command-id", "p2"]);
    assert_ne!(second["proposal"], first["proposal"]);
    let results = s.bodies(0, "start_result");
    assert_eq!(results.len(), 1, "{results:?}");
    assert_eq!(results[0].1["result"], "withdrawn");
    assert_eq!(results[0].1["outcome"], "withdrawn");
    assert!(
        results[0].1["reason"]
            .as_str()
            .unwrap()
            .contains(second["proposal"].as_str().unwrap()),
        "{:?}",
        results[0].1
    );
    let first_card = s.bodies(0, "start_card")[0].0;
    assert_eq!(results[0].1["answers_seq"], first_card);
    assert_eq!(
        s.live
            .controller
            .proposal_state(first["proposal"].as_str().unwrap())
            .unwrap(),
        format!(
            "withdrawn:superseded by {}",
            second["proposal"].as_str().unwrap()
        )
    );
    // The first snapshot is gone, and the second stays.
    assert_eq!(
        proposals_left(&s),
        vec![second["proposal"].as_str().unwrap().to_string()]
    );

    // `session end` withdraws the open card.
    let out = s.live.cli(&["session", "end", "S-1"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let results = s.bodies(0, "start_result");
    assert_eq!(results.len(), 2, "{results:?}");
    assert_eq!(results[1].1["reason"], "the session ended");
    assert!(proposals_left(&s).is_empty());
}

#[test]
fn a_controller_reopened_on_the_same_state_dir_withdraws_the_open_card() {
    let s = Start::new();
    let reply = s.propose(0, "s1", &[]);
    assert_eq!(s.bodies(0, "start_result").len(), 0);
    // A second controller on the same state directory stands for the restart.
    let mut cfg =
        pulse_factory::controller::Config::new(s.live.sb.state.clone(), 3_600_000, BIN.into());
    cfg.chat_share_dir = s.share.clone();
    let reopened = pulse_factory::controller::Controller::open(cfg, s.live.ports.clone()).unwrap();
    let results = s.bodies(0, "start_result");
    assert_eq!(results.len(), 1, "{results:?}");
    assert_eq!(results[0].1["result"], "withdrawn");
    assert!(
        results[0].1["reason"]
            .as_str()
            .unwrap()
            .contains("the controller restarted"),
        "{:?}",
        results[0].1
    );
    assert!(
        reopened.proposal_ids().is_empty(),
        "a restart keeps no proposal"
    );
    assert!(
        proposals_left(&s).is_empty(),
        "the snapshot went with the restart"
    );
    drop(reply);
}

#[test]
fn arm_from_a_chat_is_still_refused_and_a_detached_marked_settings_write_is_audited() {
    let s = Start::new();
    // G1: `arm` from below the shell is refused naming the chat, and no run exists.
    let mut args = s.propose_args("s1");
    args[0] = "arm".to_string();
    args.push("--yes".to_string());
    let ran = s.cli_below(0, &args);
    assert_eq!(ran.rc, 1, "{}", ran.err);
    assert!(ran.err.contains("chat"), "{}", ran.err);
    assert!(s.runs().is_empty());

    // `settings_caller_key`: a detached marked child's refused `settings set` is audited as `chat:S-1`.
    let ran = s.call_below(
        0,
        Style::DoubleFork,
        true,
        Some("S-1"),
        "settings_set",
        &json!({"key": "chat.self_turn_cap", "value": 5, "command_id": "w1"}),
    );
    assert_eq!(ran.response()["ok"], false, "{}", ran.out);
    let lines = s.ledger_with("caller=chat:S-1");
    assert!(
        lines.iter().any(|l| l.contains("setting_set refused")),
        "{:#?}",
        controller_ledger(&s.live)
    );
}

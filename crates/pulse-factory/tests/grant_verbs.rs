//! AC-3 of `r2.s3.w1`: the operator gives, ends and reads a project's clear grant with `grant clear`,
//! `grant revoke` and `grant list` (`docs/contracts/cli.md`, `grant`; ADR-0004, ADR-0005, ADR-0009,
//! amended). Giving and ending the grant pass the caller check that `arm` passes, take no claim and
//! read no claim token. Each is one admitted controller command with its ledger line. A removed
//! project's grant is not live for any reader, and the operator can still end its row.
//!
//! The controller runs in the test process with a recording `FakeChat`. A caller that must be below
//! a chat, or carry a marker or a seat token, is this test binary as a helper, run by a shell whose
//! PID the test registers as the chat's process.

mod common;

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::chat::*;
use common::ctl::Live;
use common::*;
use pf_api::types::{ClearGrant, GrantChanged, GrantList, RequestList};
use pf_engine::port::chat::ChatId;
use pf_engine::port::store::{CONTROLLER_SCOPE, Store};
use pf_ipc::{Handler, Request};
use serde_json::{Value, json};

const MARKER: &str = "PULSE_FACTORY_CHAT";

struct Rig {
    live: Live,
    rec: Arc<Rec>,
    chat: ChatId,
    /// A project with a live chat session (the chat that the caller check knows).
    chatty: String,
    /// A project with no session, which a test can remove.
    plain: String,
}

/// A controller with an armed run, the project `P-1` with a live chat session `S-1`, and the
/// project `P-2` with no session.
fn rig() -> Rig {
    let rec = Rec::new(strict_fake());
    let live = start(rec.clone());
    live.arm_with_units(&["u1"]);
    let (chatty, session) = started(&live, "alpha");
    assert_eq!(session, "S-1");
    let plain = add_project(&live, &repo(&live, "beta"));
    let chat = rec.chats.lock().unwrap()[0].clone();
    assert_eq!((chatty.as_str(), plain.as_str()), ("P-1", "P-2"));
    Rig {
        live,
        rec,
        chat,
        chatty,
        plain,
    }
}

impl Rig {
    fn clear(&self, project: &str, id: &str) -> std::process::Output {
        self.live
            .cli(&["grant", "clear", "--project", project, "--command-id", id])
    }

    fn list(&self) -> GrantList {
        let v = cli_json(&self.live, &["grant", "list"]);
        serde_json::from_value(v).expect("grant list decodes as GrantList")
    }

    fn ledger(&self, kind: &str) -> Vec<String> {
        ledger_kind(&self.live, kind)
    }

    fn rows(&self) -> usize {
        self.live.controller.store().clear_grants().unwrap().len()
    }

    /// The controller's admissions, as the store holds them.
    fn admissions(&self) -> usize {
        self.live
            .controller
            .store()
            .admissions(CONTROLLER_SCOPE)
            .unwrap()
            .len()
    }

    fn requests_json(&self) -> Value {
        cli_json(&self.live, &["requests"])
    }
}

fn changed(out: &std::process::Output) -> GrantChanged {
    assert_eq!(code(out), 0, "{}", stderr(out));
    serde_json::from_str(&stdout(out)).unwrap_or_else(|e| panic!("{e}: {}", stdout(out)))
}

fn json_args(args: &[&str]) -> Vec<String> {
    let mut all: Vec<String> = args.iter().map(ToString::to_string).collect();
    all.push("--json".to_string());
    all
}

fn run_json(rig: &Rig, args: &[&str]) -> std::process::Output {
    let all = json_args(args);
    let refs: Vec<&str> = all.iter().map(String::as_str).collect();
    rig.live.cli(&refs)
}

// ------------------------------------------------------------------ give and end

#[test]
fn grant_clear_gives_generation_one_and_writes_one_ledger_line() {
    let rig = rig();
    let out = run_json(
        &rig,
        &["grant", "clear", "--project", "P-2", "--command-id", "gc-1"],
    );
    let done = changed(&out);
    assert!(!done.replayed);
    assert_eq!(done.schema_version, 1);
    let g = &done.grant;
    assert_eq!(
        (g.project_id.as_str(), g.generation, g.agent_id.as_str()),
        ("P-2", 1, "orchestrator")
    );
    assert_eq!(g.granted_by, "cli");
    assert!(g.live && g.revoked_at.is_none() && g.revoked_by.is_none());
    // The ledger line starts with its sequence number and its time, and ends with the text.
    let lines = rig.ledger("grant_clear");
    assert_eq!(lines.len(), 1, "{lines:#?}");
    assert!(
        lines[0].ends_with("grant_clear P-2 agent=orchestrator generation=1 by=cli command=gc-1"),
        "{lines:#?}"
    );
}

#[test]
fn a_replay_of_the_command_id_answers_replayed_and_writes_no_second_line() {
    let rig = rig();
    changed(&run_json(
        &rig,
        &["grant", "clear", "--project", "P-2", "--command-id", "gc-1"],
    ));
    let again = changed(&run_json(
        &rig,
        &["grant", "clear", "--project", "P-2", "--command-id", "gc-1"],
    ));
    assert!(again.replayed, "the same command ID and content");
    assert_eq!(again.grant.generation, 1, "the first record");
    assert_eq!(rig.ledger("grant_clear").len(), 1, "no second line");
    assert_eq!(rig.rows(), 1);
    // The same ID with other content (another project) is a conflict.
    let out = rig.clear("P-1", "gc-1");
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("other arguments"), "{}", stderr(&out));
    assert_eq!(rig.rows(), 1);
    // A revoke replays too, and names the generation it ended even after a new grant.
    let first = changed(&run_json(
        &rig,
        &[
            "grant",
            "revoke",
            "--project",
            "P-2",
            "--command-id",
            "gr-1",
        ],
    ));
    assert_eq!(first.grant.generation, 1);
    changed(&run_json(
        &rig,
        &["grant", "clear", "--project", "P-2", "--command-id", "gc-2"],
    ));
    let replay = changed(&run_json(
        &rig,
        &[
            "grant",
            "revoke",
            "--project",
            "P-2",
            "--command-id",
            "gr-1",
        ],
    ));
    assert!(replay.replayed);
    assert_eq!(
        replay.grant.generation, 1,
        "the generation the revoke ended"
    );
    assert!(!replay.grant.live);
    assert_eq!(rig.ledger("grant_revoke").len(), 1, "no second line");
}

#[test]
fn a_second_grant_is_refused_and_names_the_generation_then_a_revoke_ends_it() {
    let rig = rig();
    changed(&run_json(&rig, &["grant", "clear", "--project", "P-2"]));
    let out = rig.clear("P-2", "gc-2");
    assert_eq!(code(&out), 1);
    assert!(
        stderr(&out).contains("project P-2 already holds the clear grant (generation 1)"),
        "{}",
        stderr(&out)
    );
    assert_eq!(rig.rows(), 1, "no second row");
    // The refusal is an admission and one ledger line of its own.
    let lines = rig.ledger("grant_clear");
    assert!(
        lines
            .iter()
            .any(|l| l.contains("refused") && l.contains("command=gc-2")),
        "{lines:#?}"
    );

    let ended = changed(&run_json(&rig, &["grant", "revoke", "--project", "P-2"]));
    assert_eq!(ended.grant.generation, 1);
    assert_eq!(ended.grant.revoked_by.as_deref(), Some("cli"));
    assert!(ended.grant.revoked_at.is_some() && !ended.grant.live);
    let lines = rig.ledger("grant_revoke");
    assert_eq!(lines.len(), 1, "{lines:#?}");
    assert!(
        lines[0].contains("grant_revoke P-2 generation=1 by=cli command="),
        "{lines:#?}"
    );
    // A second revoke is refused.
    let out = rig.live.cli(&["grant", "revoke", "--project", "P-2"]);
    assert_eq!(code(&out), 1);
    assert!(
        stderr(&out).contains("project P-2 holds no clear grant"),
        "{}",
        stderr(&out)
    );
    // A new grant is generation 2.
    let next = changed(&run_json(&rig, &["grant", "clear", "--project", "P-2"]));
    assert_eq!(next.grant.generation, 2);
    assert!(next.grant.live);
}

#[test]
fn the_text_forms_print_one_line_each() {
    let rig = rig();
    let out = rig.live.cli(&["grant", "clear", "--project", "P-2"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "clear grant given: P-2 orchestrator generation 1\n"
    );
    let out = rig.live.cli(&["grant", "list"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&out);
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(
        text.starts_with("P-2  generation 1  granted by cli at ")
            && text.trim_end().ends_with("live"),
        "{text}"
    );
    let out = rig.live.cli(&["grant", "revoke", "--project", "P-2"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(stdout(&out), "clear grant ended: P-2 generation 1\n");
    let text = stdout(&rig.live.cli(&["grant", "list"]));
    assert!(text.contains("revoked by cli at "), "{text}");
    assert!(!text.contains("live"), "{text}");
    // No grant at all reads as one line too.
    let rig = self::rig();
    assert_eq!(
        stdout(&rig.live.cli(&["grant", "list"])),
        "no clear grants\n"
    );
}

#[test]
fn an_unknown_project_and_a_removed_project_are_refused_on_grant_clear() {
    let rig = rig();
    let out = rig.clear("P-9", "gc-9");
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("P-9"), "{}", stderr(&out));
    let out = rig.live.cli(&["project", "remove", "P-2"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let out = rig.clear("P-2", "gc-2");
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("P-2"), "{}", stderr(&out));
    assert_eq!(rig.rows(), 0, "no row was written");
    // Each refusal is an admission with a line, and no grant line is an admitted one.
    assert!(
        rig.ledger("grant_clear")
            .iter()
            .all(|l| l.contains("refused")),
        "{:#?}",
        rig.ledger("grant_clear")
    );
}

#[test]
fn grant_list_decodes_as_a_grant_list_and_marks_one_row_live() {
    let rig = rig();
    changed(&run_json(&rig, &["grant", "clear", "--project", "P-1"]));
    changed(&run_json(&rig, &["grant", "clear", "--project", "P-2"]));
    changed(&run_json(&rig, &["grant", "revoke", "--project", "P-2"]));
    changed(&run_json(&rig, &["grant", "clear", "--project", "P-2"]));
    let list = rig.list();
    let key = |g: &ClearGrant| (g.project_id.clone(), g.generation, g.live);
    assert_eq!(
        list.grants.iter().map(key).collect::<Vec<_>>(),
        vec![
            ("P-1".to_string(), 1, true),
            ("P-2".to_string(), 2, true),
            ("P-2".to_string(), 1, false),
        ],
        "by project, and in a project newest generation first, one row live in each"
    );
    // A project filter.
    let one = cli_json(&rig.live, &["grant", "list", "--project", "P-2"]);
    let one: GrantList = serde_json::from_value(one).unwrap();
    assert_eq!(one.grants.len(), 2);
    assert!(one.grants.iter().all(|g| g.project_id == "P-2"));
}

// ------------------------------------------------------------------ a removed project (audit C5)

#[test]
fn a_removed_projects_grant_is_not_live_and_the_operator_can_still_end_it() {
    let rig = rig();
    changed(&run_json(&rig, &["grant", "clear", "--project", "P-2"]));
    assert!(rig.requests_json()["grants"].is_array(), "the live grant");
    let out = rig.live.cli(&["project", "remove", "P-2"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));

    // No reader marks it live.
    let list = rig.list();
    assert_eq!(list.grants.len(), 1, "the row stays");
    assert!(
        !list.grants[0].live,
        "a removed project's grant is not live"
    );
    let requests = rig.requests_json();
    assert!(
        requests.get("grants").is_none(),
        "requests holds no grant of a removed project: {requests}"
    );
    let typed: RequestList = serde_json::from_value(requests).unwrap();
    assert!(typed.grants.is_empty());

    // The revoke is admitted: it writes its line and closes the row.
    let lines_before = rig.ledger("grant_revoke").len();
    let ended = changed(&run_json(&rig, &["grant", "revoke", "--project", "P-2"]));
    assert_eq!(ended.grant.generation, 1);
    assert!(ended.grant.revoked_at.is_some() && !ended.grant.live);
    assert_eq!(rig.ledger("grant_revoke").len(), lines_before + 1);
    assert!(
        rig.ledger("grant_revoke")
            .last()
            .unwrap()
            .contains("grant_revoke P-2 generation=1 by=cli command="),
        "{:#?}",
        rig.ledger("grant_revoke")
    );
    // A second revoke is refused, and so is a revoke of a project the store does not hold.
    let out = rig.live.cli(&["grant", "revoke", "--project", "P-2"]);
    assert_eq!(code(&out), 1);
    assert!(
        stderr(&out).contains("holds no clear grant"),
        "{}",
        stderr(&out)
    );
    let out = rig.live.cli(&["grant", "revoke", "--project", "P-9"]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("P-9"), "{}", stderr(&out));
}

// ------------------------------------------------------------------ the caller check

static RUNS: AtomicUsize = AtomicUsize::new(0);

/// Not a test: the process that `call_as` starts. Run on its own it does nothing.
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

impl Rig {
    /// Call `verb` from the helper, below a shell. `below_chat` registers the shell as the live chat's
    /// process; `env` is what the helper's environment holds besides the path to the controller.
    fn call_as(&self, below_chat: bool, env: &[(&str, &str)], verb: &str, args: &Value) -> Value {
        let n = RUNS.fetch_add(1, Ordering::SeqCst);
        let go = self.live.sb.root.path().join(format!("go-{n}"));
        let exe = std::env::current_exe().unwrap();
        let mut shell = Command::new("/bin/sh");
        shell
            .arg("-c")
            .arg(
                "while [ ! -e \"$GO\" ]; do sleep 0.02; done; \
                 \"$EXE\" --exact helper_calls_the_controller --nocapture --test-threads=1",
            )
            .env_clear()
            .env("PATH", self.live.sb.path_env())
            .env("GO", &go)
            .env("EXE", &exe)
            .env("PF_TEST_HELPER_STATE", &self.live.sb.state)
            .env("PF_TEST_HELPER_VERB", verb)
            .env("PF_TEST_HELPER_ARGS", args.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in env {
            shell.env(k, v);
        }
        let child = shell.spawn().unwrap();
        if below_chat {
            self.rec
                .pids
                .lock()
                .unwrap()
                .insert(self.chat.clone(), child.id());
        }
        std::fs::write(&go, "").unwrap();
        let out = child.wait_with_output().unwrap();
        self.rec.pids.lock().unwrap().clear();
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let line = text
            .lines()
            .find_map(|l| l.split_once("HELPER_RESPONSE:").map(|(_, r)| r.to_string()))
            .unwrap_or_else(|| {
                panic!(
                    "no response from the helper: {text}\n{}",
                    String::from_utf8_lossy(&out.stderr)
                )
            });
        serde_json::from_str(&line).unwrap()
    }
}

fn refused_with(resp: &Value, verb: &str, because: &str) {
    assert_eq!(resp["ok"], false, "{verb}: {resp}");
    let why = resp["error"].as_str().unwrap_or_default();
    assert!(
        why.contains(&format!("{verb} refused")) && why.contains(because),
        "{verb}: {why}"
    );
}

#[test]
fn a_chat_a_marker_and_a_seat_are_refused_on_both_verbs_and_write_nothing() {
    let rig = rig();
    // The grant that a revoke would end.
    changed(&run_json(&rig, &["grant", "clear", "--project", "P-2"]));
    let (rows, admissions, lines) = (
        rig.rows(),
        rig.admissions(),
        controller_ledger(&rig.live).len(),
    );
    for (verb, args) in [
        (
            "grant_clear",
            json!({ "project_id": "P-1", "command_id": "x-1" }),
        ),
        (
            "grant_revoke",
            json!({ "project_id": "P-2", "command_id": "x-2" }),
        ),
    ] {
        // A caller below the live chat process.
        let resp = rig.call_as(true, &[], verb, &args);
        refused_with(&resp, verb, "descends from, chat process");
        // A caller that holds the chat marker and no chat ancestor.
        let resp = rig.call_as(false, &[(MARKER, "S-1")], verb, &args);
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
    assert_eq!(rig.rows(), rows, "no row was written or ended");
    assert_eq!(rig.admissions(), admissions, "no admission");
    assert_eq!(controller_ledger(&rig.live).len(), lines, "no ledger line");
    assert!(rig.list().grants[0].live, "the grant is still live");
    // The control: a caller with none of these passes the check.
    let resp = rig.call_as(
        false,
        &[],
        "grant_revoke",
        &json!({ "project_id": "P-2", "command_id": "x-3" }),
    );
    assert_eq!(resp["ok"], true, "{resp}");
    assert_eq!(rig.rows(), rows);
    assert!(!rig.list().grants[0].live);
}

#[test]
fn the_client_refuses_a_seat_token_before_it_sends() {
    let rig = rig();
    let admissions = rig.admissions();
    for verb in ["clear", "revoke"] {
        let out = rig
            .live
            .sb
            .cmd(&["grant", verb, "--project", "P-2"])
            .env("PF_SEAT_TOKEN", "seat-token-value-1234")
            .output()
            .unwrap();
        assert_eq!(code(&out), 1);
        assert!(
            stderr(&out).contains(&format!("grant {verb} refused: PF_SEAT_TOKEN is set")),
            "{}",
            stderr(&out)
        );
    }
    assert_eq!(rig.rows(), 0);
    assert_eq!(rig.admissions(), admissions, "nothing was sent");
    assert!(rig.ledger("grant_clear").is_empty());
}

#[test]
fn with_no_peer_both_verbs_are_refused_for_want_of_a_caller() {
    let rig = rig();
    let admissions = rig.admissions();
    for verb in ["grant_clear", "grant_revoke"] {
        let resp = rig.live.controller.handle(Request::new(
            verb,
            json!({ "project_id": "P-2", "command_id": "np-1" }),
        ));
        assert!(!resp.ok);
        let why = resp.error.unwrap();
        assert_eq!(
            why,
            format!("{verb} refused: the controller cannot tell which process called")
        );
    }
    assert_eq!(rig.rows(), 0);
    assert_eq!(rig.admissions(), admissions);
}

#[test]
fn grant_list_answers_a_chat_caller() {
    let rig = rig();
    changed(&run_json(&rig, &["grant", "clear", "--project", "P-2"]));
    let resp = rig.call_as(true, &[], "grant_list", &json!({}));
    assert_eq!(resp["ok"], true, "{resp}");
    let list: GrantList = serde_json::from_value(resp["result"].clone()).unwrap();
    assert_eq!(list.grants.len(), 1);
    assert!(list.grants[0].live);
    // So does the plain handler: a read needs no caller.
    let resp = rig
        .live
        .controller
        .handle(Request::new("grant_list", json!({})));
    assert!(resp.ok, "{:?}", resp.error);
}

#[test]
fn halt_leaves_the_grant_live() {
    let rig = rig();
    changed(&run_json(&rig, &["grant", "clear", "--project", "P-2"]));
    let out = rig.live.cli(&["halt", "--reason", "stop now"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let list = rig.list();
    assert!(list.grants[0].live, "halt does not revoke");
    assert_eq!(rig.ledger("grant_revoke").len(), 0);
    assert_eq!(rig.chatty, "P-1");
    assert_eq!(rig.plain, "P-2");
}

// ------------------------------------------------------------------ no claim token

#[test]
fn no_ledger_line_admission_or_answer_holds_a_claim_token() {
    let rig = rig();
    // The claim's token leaves the controller once, in the answer of `claim`.
    let claim = pf_ipc::call(&rig.live.sb.state, "claim", json!({ "holder": "ann@host" })).unwrap();
    assert!(claim.ok, "{:?}", claim.error);
    let token = claim.result["token"].as_str().unwrap().to_string();
    assert_eq!(token.len(), 64);

    let mut answers = Vec::new();
    for args in [
        vec!["grant", "clear", "--project", "P-2"],
        vec![
            "grant",
            "clear",
            "--project",
            "P-2",
            "--command-id",
            "again",
        ],
        vec!["grant", "revoke", "--project", "P-2"],
        vec!["grant", "list"],
    ] {
        let out = run_json(&rig, &args);
        answers.push(stdout(&out));
        answers.push(stderr(&out));
    }
    // The grant verbs take no claim: the claim is where it was.
    let view = rig.live.handle().engine.view().unwrap();
    assert_eq!(view.claims.len(), 1);
    assert!(view.claims[0].fenced_at.is_none(), "no claim moved");
    for text in &answers {
        assert!(!text.contains(&token), "an answer holds the token");
    }
    let ledger = std::fs::read_to_string(rig.live.sb.state.join("controller.ledger")).unwrap();
    assert!(!ledger.contains(&token), "{ledger}");
    for a in rig
        .live
        .controller
        .store()
        .admissions(CONTROLLER_SCOPE)
        .unwrap()
    {
        assert!(!a.content.contains(&token) && !a.reason.unwrap_or_default().contains(&token));
    }
    assert!(
        rig.live
            .controller
            .store()
            .clear_grants()
            .unwrap()
            .iter()
            .all(|g| !format!("{g:?}").contains(&token))
    );
}

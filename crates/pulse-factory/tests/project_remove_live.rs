//! AC-3 of `r1.s5.w5`: `project remove` with `end_sessions`, and the refusal while a run of the project
//! is active. The controller runs in the test process on a `FakeChat` runtime and a `FakeAdapter`; the
//! socket cases call the verb handler, and the device cases go through the real API (the listener is
//! spawned on a loopback port). No test starts `claude`, spends a token, or reaches beyond `127.0.0.1`.

mod common;

use std::process::{Command, Stdio};
use std::time::Duration;

use common::chat::*;
use common::ctl::Live;
use common::*;
use pf_engine::port::store::{
    AdmissionOutcome, CONTROLLER_SCOPE, Commit, Committed, Effect, NewAdmission, Store,
};
use pf_ipc::{Handler, Request, Response};
use pulse_factory::controller::api;
use rusqlite::{Connection, params};
use serde_json::{Value, json};

fn rec_live() -> (std::sync::Arc<Rec>, Live) {
    let rec = Rec::new(strict_fake());
    let live = start(rec.clone());
    (rec, live)
}

/// A second live session in a project that has its Orchestrator session already. A project has one
/// live Orchestrator session (a unique index), and no verb makes another agent in this release, so the
/// session is a row of an agent put in the tables by hand (as `chat_session_admission.rs` does). It has
/// no chat process, so ending it stops nothing.
fn another_session(live: &Live, project: &str) -> String {
    let conn = Connection::open(live.sb.state.join("store.sqlite")).unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO agents VALUES ('helper', 'Helper', 'Helps', 0, 'accent', 'helper', NULL, \
         'claude-code', 'm', 'high', 'default', '[]', '{}', '{}', 't0')",
        [],
    )
    .unwrap();
    let worktree: String = conn
        .query_row(
            "SELECT worktree_id FROM chat_sessions WHERE project_id = ?1 ORDER BY rowid LIMIT 1",
            [project],
            |r| r.get(0),
        )
        .unwrap();
    let next: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(CAST(substr(id, 3) AS INTEGER)), 0) + 1 FROM chat_sessions",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let id = format!("S-{next}");
    conn.execute(
        "INSERT INTO chat_sessions (id, project_id, worktree_id, agent_id, runtime, state, model, \
             effort, permission_mode, created_at, updated_at) \
         VALUES (?1, ?2, ?3, 'helper', 'claude-code', 'idle', 'm', 'high', 'default', 't', 't')",
        params![id, project, worktree],
    )
    .unwrap();
    id
}

fn remove(live: &Live, id: &str, end: Option<bool>, command_id: &str) -> Response {
    let mut args = json!({ "id": id, "command_id": command_id });
    if let Some(e) = end {
        args["end_sessions"] = json!(e);
    }
    live.controller.handle(Request::new("project_remove", args))
}

fn ok(r: Response) -> Value {
    assert!(r.ok, "refused: {:?}", r.error);
    r.result
}

fn refused(r: Response) -> String {
    assert!(!r.ok, "removed: {}", r.result);
    r.error.unwrap_or_default()
}

fn is_ended(live: &Live, session: &str) -> bool {
    live.controller
        .store()
        .chat_session(session)
        .unwrap()
        .unwrap()
        .ended_at
        .is_some()
}

fn project_is_live(live: &Live, id: &str) -> bool {
    live.controller
        .store()
        .project(id)
        .unwrap()
        .unwrap()
        .removed_at
        .is_none()
}

/// Arm a run and link it to a project, as a device's start card does (`Effect::SetRunProject` under
/// the controller's scope). It answers the run's ID.
fn run_of(live: &Live, project: &str) -> String {
    let run = live.arm();
    link(live, &run, project);
    run
}

fn link(live: &Live, run: &str, project: &str) {
    let committed = live
        .controller
        .store()
        .commit(Commit {
            run_id: CONTROLLER_SCOPE.to_string(),
            command_id: format!("run-project-{run}"),
            content: json!({ "run": run, "project": project }).to_string(),
            admission: NewAdmission {
                kind: "run_project".to_string(),
                outcome: AdmissionOutcome::Admitted,
                reason: None,
                unit_id: Some(run.to_string()),
                role: None,
                model: None,
                attempt: None,
                admitted_at: "2026-11-13T00:00:00.000Z".to_string(),
            },
            effects: vec![Effect::SetRunProject {
                run_id: run.to_string(),
                project_id: project.to_string(),
            }],
            ledger: None,
        })
        .unwrap();
    assert!(
        matches!(committed, Committed::Applied { .. }),
        "{committed:?}"
    );
}

fn end_lines(live: &Live, command_id: &str) -> Vec<String> {
    ledger_kind(live, "chat_session_end")
        .into_iter()
        .filter(|l| l.contains(&format!("command={command_id}:end:")))
        .collect()
}

#[test]
fn with_end_sessions_the_remove_ends_every_live_session_and_removes_the_project() {
    let (rec, live) = rec_live();
    let (project, first) = started(&live, "alpha");
    let second = another_session(&live, &project);
    assert_eq!(
        live.controller.live_chat_sessions().len(),
        1,
        "one has a chat process"
    );
    assert_eq!(rec.stop_count(), 0);

    let v = ok(remove(&live, &project, Some(true), "rm-both"));

    assert_eq!(v["replayed"], false);
    assert_eq!(v["ended"], json!([first, second]), "in session order: {v}");
    assert!(v["project"]["removed_at"].is_string(), "{v}");
    assert!(!project_is_live(&live, &project));
    for s in [&first, &second] {
        assert!(is_ended(&live, s), "{s} is ended");
    }
    assert_eq!(
        rec.stop_count(),
        1,
        "the fake chat process of the session that had one stopped"
    );
    assert!(live.controller.live_chat_sessions().is_empty());
    // One chat_session_end line each, under the derived command ID, then the remove's own line.
    let ends = end_lines(&live, "rm-both");
    assert_eq!(ends.len(), 2, "{ends:?}");
    assert!(
        ends[0].contains(&format!(
            "chat_session_end {first} admitted command=rm-both:end:{first}"
        )),
        "{}",
        ends[0]
    );
    assert!(
        ends[1].contains(&format!(
            "chat_session_end {second} admitted command=rm-both:end:{second}"
        )),
        "{}",
        ends[1]
    );
    let removes = ledger_kind(&live, "project_remove");
    assert_eq!(removes.len(), 1, "{removes:?}");
    assert!(removes[0].contains("command=rm-both"), "{}", removes[0]);
}

#[test]
fn without_the_field_a_live_session_refuses_the_remove_as_before_and_ends_nothing() {
    let (rec, live) = rec_live();
    let (project, session) = started(&live, "alpha");

    for (n, end) in [None, Some(false)].into_iter().enumerate() {
        let why = refused(remove(&live, &project, end, &format!("rm-no-{n}")));
        assert_eq!(
            why,
            format!("project {project} has a live chat session {session}; end it first")
        );
    }

    assert!(!is_ended(&live, &session));
    assert!(project_is_live(&live, &project));
    assert_eq!(rec.stop_count(), 0);
    assert!(end_lines(&live, "rm-no-0").is_empty());
}

#[test]
fn a_remove_that_ends_no_session_answers_an_empty_list_and_a_plain_remove_answers_none() {
    let (_rec, live) = rec_live();
    let empty = add_project(&live, &repo(&live, "empty"));
    let plain = add_project(&live, &repo(&live, "plain"));

    let v = ok(remove(&live, &empty, Some(true), "rm-empty"));
    assert_eq!(v["ended"], json!([]), "end_sessions with nothing live: {v}");
    let v = ok(remove(&live, &plain, None, "rm-plain"));
    assert!(
        v.get("ended").is_none(),
        "a remove without the field has no `ended`: {v}"
    );
}

#[test]
fn an_active_run_of_the_project_refuses_the_remove_names_the_run_and_ends_no_session() {
    let (rec, live) = rec_live();
    let (project, session) = started(&live, "alpha");
    let run = run_of(&live, &project);
    // `run_list` shows the run's project: the confirm of the app reads it.
    let listed = ok(live.controller.handle(Request::new("run_list", json!({}))));
    assert_eq!(listed["runs"][0]["id"], run.as_str());
    assert_eq!(
        listed["runs"][0]["project_id"],
        project.as_str(),
        "{listed}"
    );

    for (n, end) in [None, Some(true)].into_iter().enumerate() {
        let why = refused(remove(&live, &project, end, &format!("rm-run-{n}")));
        assert_eq!(
            why,
            format!("project {project} has an active run {run}; remove it after the run ends")
        );
    }

    assert!(!is_ended(&live, &session), "no session was ended");
    assert_eq!(rec.stop_count(), 0);
    assert!(project_is_live(&live, &project));
    assert!(
        end_lines(&live, "rm-run-1").is_empty(),
        "no end was admitted"
    );
    // The refusal is an admission of its own, with its ledger line.
    let lines = ledger_kind(&live, "project_remove");
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines
            .iter()
            .all(|l| l.contains("refused") && l.contains(&run)),
        "{lines:?}"
    );
    // The CLI refuses the same way, with the text on stderr.
    let out = live.cli(&["project", "remove", &project, "--end-sessions"]);
    assert_eq!(code(&out), 1);
    assert!(
        stderr(&out).contains(&format!("has an active run {run}")),
        "{}",
        stderr(&out)
    );
    assert!(!is_ended(&live, &session));
}

#[test]
fn a_run_of_another_project_or_an_ended_run_does_not_refuse() {
    let (_rec, live) = rec_live();
    let (mine, session) = started(&live, "mine");
    let theirs = add_project(&live, &repo(&live, "theirs"));
    let run = run_of(&live, &theirs);

    // A run of another project does not hold this one.
    let v = ok(remove(&live, &mine, Some(true), "rm-other"));
    assert_eq!(v["ended"], json!([session]));
    // The run's own project is held while the run is active, and free when it has ended.
    assert!(refused(remove(&live, &theirs, None, "rm-theirs-1")).contains(&run));
    live.handle().engine.end_run("done").unwrap();
    ok(remove(&live, &theirs, None, "rm-theirs-2"));
}

#[test]
fn a_run_with_no_project_does_not_refuse() {
    let (_rec, live) = rec_live();
    let project = add_project(&live, &repo(&live, "alpha"));
    // Armed from the CLI: no project, and `run_list` shows null.
    live.arm();
    let listed = ok(live.controller.handle(Request::new("run_list", json!({}))));
    assert!(listed["runs"][0]["project_id"].is_null(), "{listed}");
    assert!(
        listed["runs"][0]
            .as_object()
            .unwrap()
            .contains_key("project_id"),
        "{listed}"
    );
    ok(remove(&live, &project, Some(true), "rm-cli-run"));
}

#[test]
fn a_replay_ends_nothing_twice_and_names_the_same_sessions() {
    let (rec, live) = rec_live();
    let (project, first) = started(&live, "alpha");
    let second = another_session(&live, &project);
    ok(remove(&live, &project, Some(true), "rm-replay"));
    let stops = rec.stop_count();
    assert_eq!(stops, 1);
    let lines_before = controller_ledger(&live);

    let again = ok(remove(&live, &project, Some(true), "rm-replay"));

    assert_eq!(again["replayed"], true);
    assert_eq!(again["ended"], json!([first, second]), "{again}");
    assert_eq!(rec.stop_count(), stops, "a replay stops nothing");
    assert_eq!(
        controller_ledger(&live),
        lines_before,
        "a replay writes no line"
    );
    assert_eq!(end_lines(&live, "rm-replay").len(), 2);
    // The same ID with another project is a conflict, and ends nothing.
    let (other, s) = started(&live, "beta");
    let why = refused(remove(&live, &other, Some(true), "rm-replay"));
    assert!(why.contains("used before with other arguments"), "{why}");
    assert!(!is_ended(&live, &s));
}

#[test]
fn a_replay_of_a_remove_that_ended_a_session_stays_a_replay_after_a_run_exists() {
    let (_rec, live) = rec_live();
    let (project, session) = started(&live, "alpha");
    ok(remove(&live, &project, Some(true), "rm-then-run"));
    // A run of the removed project keeps running (Planner ruling 3). The replay is still a replay.
    live.arm();
    let again = ok(remove(&live, &project, Some(true), "rm-then-run"));
    assert_eq!(again["replayed"], true);
    assert_eq!(again["ended"], json!([session]));
}

#[test]
fn a_command_id_too_long_for_its_derived_ids_is_refused_before_any_end() {
    let (rec, live) = rec_live();
    let (project, first) = started(&live, "alpha");
    let second = another_session(&live, &project);
    // 125 characters is a valid command ID, and `<id>:end:S-1` is 5 + 3 characters longer than 128.
    let long = "c".repeat(125);

    let why = refused(remove(&live, &project, Some(true), &long));

    assert!(why.contains("too long to end session"), "{why}");
    assert!(why.contains(&first), "{why}");
    assert!(
        !why.contains(&long),
        "the refusal does not echo the command ID: {why}"
    );
    assert!(
        !is_ended(&live, &first) && !is_ended(&live, &second),
        "nothing was ended"
    );
    assert_eq!(rec.stop_count(), 0);
    assert!(project_is_live(&live, &project));
    // A 120-character ID leaves room for the derived IDs, and works.
    let fits = "c".repeat(120);
    let v = ok(remove(&live, &project, Some(true), &fits));
    assert_eq!(v["ended"], json!([first, second]));
}

#[test]
fn a_session_that_is_already_ended_is_not_ended_again() {
    let (rec, live) = rec_live();
    let (project, first) = started(&live, "alpha");
    let second = another_session(&live, &project);
    cli_json(&live, &["session", "end", &first]);
    assert_eq!(rec.stop_count(), 1);

    let v = ok(remove(&live, &project, Some(true), "rm-one-left"));

    assert_eq!(v["ended"], json!([second]), "{v}");
    assert_eq!(rec.stop_count(), 1, "the other session had no process");
    assert_eq!(end_lines(&live, "rm-one-left").len(), 1);
}

#[test]
fn an_unknown_project_is_refused_as_before() {
    let (_rec, live) = rec_live();
    let why = refused(remove(&live, "P-99", Some(true), "rm-none"));
    assert!(why.contains("no project P-99"), "{why}");
}

#[test]
fn the_cli_flag_sends_end_sessions_and_prints_the_ended_sessions() {
    let (_rec, live) = rec_live();
    let (project, session) = started(&live, "alpha");
    let out = live.cli(&["project", "remove", &project, "--end-sessions"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.starts_with(&format!("project {project} removed: alpha")),
        "{text}"
    );
    assert!(
        text.contains(&format!("sessions ended: {session}")),
        "{text}"
    );
    assert!(is_ended(&live, &session));
}

// ------------------------------------------------------------------ a device, through the real API

fn b64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().fold(0u32, |n, b| (n << 8) | u32::from(*b)) << (8 * (3 - chunk.len()));
        for i in 0..=chunk.len() {
            out.push(char::from(A[((n >> (18 - 6 * i)) & 63) as usize]));
        }
        for _ in chunk.len()..3 {
            out.push('=');
        }
    }
    out
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

/// The API's listener on a loopback port, on a thread of its own, for a live controller.
struct Listener {
    addr: String,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Listener {
    fn drop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn free_addr() -> String {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    format!("127.0.0.1:{port}")
}

fn rec_live_with_api() -> (std::sync::Arc<Rec>, Live, Listener) {
    let rec = Rec::new(strict_fake());
    let addr = free_addr();
    let for_cfg = addr.clone();
    let live = start_with(rec.clone(), move |cfg| cfg.api_listen = Some(for_cfg));
    let served = live.controller.clone();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let thread = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let _api = api::spawn_api(&served);
            let _ = rx.await;
        });
    });
    for _ in 0..400 {
        let up = pf_ipc::call(&live.sb.state, "api_status", json!({}))
            .is_ok_and(|r| r.ok && r.result["state"] == "listening");
        if up {
            return (
                rec,
                live,
                Listener {
                    addr,
                    stop: Some(tx),
                    thread: Some(thread),
                },
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("the API never listened");
}

impl Listener {
    fn http(&self, live: &Live, path: &str, token: Option<&str>, body: &str) -> (u16, Value) {
        let spki = pf_ipc::call(&live.sb.state, "api_status", json!({}))
            .unwrap()
            .result["key_spki_sha256"]
            .as_str()
            .unwrap()
            .to_string();
        let pin = format!("sha256//{}", b64(&unhex(&spki)));
        let mut cmd = Command::new("curl");
        cmd.args(["-sS", "-k", "--max-time", "20", "--pinnedpubkey", &pin])
            .args(["-X", "POST", "-w", "\n%{http_code}"])
            .args([
                "-H",
                "Content-Type: application/json",
                "--data-binary",
                body,
            ])
            .arg(format!("https://{}{path}", self.addr))
            .stdout(Stdio::piped());
        if let Some(t) = token {
            cmd.args(["-H", &format!("Authorization: Bearer {t}")]);
        }
        let out = cmd.output().expect("curl is on this host");
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let (body, status) = text.rsplit_once('\n').unwrap_or(("", "0"));
        (
            status.trim().parse().unwrap_or(0),
            serde_json::from_str(body).unwrap_or(Value::Null),
        )
    }

    fn token(&self, live: &Live) -> String {
        let v = cli_json(live, &["device", "pair"]);
        let (status, paired) = self.http(
            live,
            "/v1/pair",
            None,
            &json!({ "code": v["code"], "device_name": "macbook" }).to_string(),
        );
        assert_eq!(status, 200, "{paired}");
        paired["token"].as_str().unwrap().to_string()
    }
}

fn api_lines(live: &Live) -> Vec<String> {
    lines(&live.sb.state.join("api.ledger"))
        .into_iter()
        .filter(|l| l.contains("verb=project_remove"))
        .collect()
}

#[test]
fn a_device_removes_with_end_sessions_through_the_api_and_gets_its_api_ledger_line() {
    let (rec, live, api) = rec_live_with_api();
    let token = api.token(&live);
    let (project, first) = started(&live, "alpha");
    let second = another_session(&live, &project);
    let body = json!({ "id": project, "end_sessions": true, "command_id": "dev-rm-1" }).to_string();

    let (status, v) = api.http(&live, "/v1/verbs/project_remove", Some(&token), &body);

    assert_eq!(status, 200, "{v}");
    assert_eq!(v["ended"], json!([first, second]));
    assert!(!project_is_live(&live, &project));
    assert_eq!(rec.stop_count(), 1);
    let lines = api_lines(&live);
    assert_eq!(
        lines.len(),
        1,
        "one api.ledger line for the call: {lines:?}"
    );
    for word in ["device=D-1", "status=200", "outcome=ok"] {
        assert!(lines[0].contains(word), "{word} in {}", lines[0]);
    }
    assert_eq!(end_lines(&live, "dev-rm-1").len(), 2);
    // The replay over the API ends nothing and shows as a replay in the request ledger.
    let (status, again) = api.http(&live, "/v1/verbs/project_remove", Some(&token), &body);
    assert_eq!(status, 200, "{again}");
    assert_eq!(again["replayed"], true);
    assert_eq!(again["ended"], json!([first, second]));
    assert_eq!(rec.stop_count(), 1);
    assert!(api_lines(&live)[1].contains("outcome=replayed"));
}

#[test]
fn an_active_run_refuses_a_device_remove_with_the_run_named_and_ends_no_session() {
    let (rec, live, api) = rec_live_with_api();
    let token = api.token(&live);
    let (project, session) = started(&live, "alpha");
    let run = run_of(&live, &project);
    let body =
        json!({ "id": project, "end_sessions": true, "command_id": "dev-rm-run" }).to_string();

    let (status, v) = api.http(&live, "/v1/verbs/project_remove", Some(&token), &body);

    assert_eq!(status, 422, "{v}");
    assert_eq!(
        v["error"],
        format!("project {project} has an active run {run}; remove it after the run ends")
    );
    assert!(!is_ended(&live, &session));
    assert_eq!(rec.stop_count(), 0);
    assert!(project_is_live(&live, &project));
    let lines = api_lines(&live);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].contains("status=422") && lines[0].contains("outcome=refused"),
        "{}",
        lines[0]
    );
    // Without the field it is the same refusal.
    let body = json!({ "id": project, "command_id": "dev-rm-run-2" }).to_string();
    let (status, v) = api.http(&live, "/v1/verbs/project_remove", Some(&token), &body);
    assert_eq!(status, 422, "{v}");
    assert!(
        v["error"].as_str().unwrap().contains("has an active run"),
        "{v}"
    );
}

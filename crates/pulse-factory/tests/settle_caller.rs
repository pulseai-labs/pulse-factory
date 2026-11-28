//! AC-5 of `r2.s3.w2`: the controller names the caller of `settle` from its process and its host, and
//! refuses every caller it cannot name (`docs/contracts/requests.md`, "The caller check"; ADR-0005,
//! amended; `session-trust`). The check reads the caller's pidfd, walks its ancestry to a live chat
//! process, finds that session's live host and, under `systemd`, compares cgroups. It checks again at
//! the end that no process of the chain exited or was replaced. It writes no attach line, opens no
//! connection to a host, and reads no claim token.
//!
//! Real processes, the `detached` launcher, and a seam for the OS. A refusal writes nothing but its
//! audited line.

mod common;
#[path = "common/settle_rig.rs"]
mod settle_rig;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use common::chat::*;
use common::*;
use pf_engine::port::store::Store;
use pf_ipc::{Handler, Peer, Request};
use pulse_factory::controller::hosts::HostLauncher;
use serde_json::json;
use settle_rig::*;

fn unchanged(rig: &SRig, id: &str) {
    let r = rig.live.controller.store().request(id).unwrap().unwrap();
    assert_eq!(r.status.as_str(), "open", "the request stays open");
    assert_eq!(
        rig.live.unit_status("u1"),
        "parked",
        "the unit keeps its park"
    );
    assert_eq!(
        rig.admissions("settle"),
        0,
        "no settle admission in the run"
    );
}

// ------------------------------------------------------------------ the OS

#[test]
fn a_controller_on_macos_refuses_every_settle_and_the_grant_verbs_still_work() {
    for systemd in [false, true] {
        // The `systemd` double names a cgroup for the chat's host unit that no caller is in.
        let stub = CgroupStub::new(&HostSeed::fake("S-1").unit);
        let program = stub.program();
        let rig = SRig::with(&["u1"], Some(10), move |cfg| {
            cfg.os = "macos".to_string();
            if systemd {
                cfg.host_launcher = HostLauncher::Systemd;
                cfg.systemctl = program;
            }
        });
        let (id, rev) = rig.request("u1");
        let text = why(&rig.settle(&id, rev, "clear", None, WHY));
        assert!(
            text.contains(
                "settle refused: this server runs on macos; the clear grant's acts are admitted on Linux only (the grant can still be given and ended here)"
            ),
            "{text}"
        );
        unchanged(&rig, &id);
        // The grant can still be ended, read and given again.
        let out = rig
            .live
            .cli(&["grant", "revoke", "--project", &rig.project]);
        assert_eq!(code(&out), 0, "{}", stderr(&out));
        let out = rig.live.cli(&["grant", "list"]);
        assert_eq!(code(&out), 0, "{}", stderr(&out));
        let out = rig.live.cli(&["grant", "clear", "--project", &rig.project]);
        assert_eq!(code(&out), 0, "{}", stderr(&out));
    }
}

// ------------------------------------------------------------------ the pidfd

#[test]
fn a_failing_pidfd_read_refuses_every_settle_with_the_kernel_text() {
    let rig = SRig::new(&["u1"], Some(10));
    let (id, rev) = rig.request("u1");
    rig.live
        .controller
        .settle_faults()
        .fail_pidfd
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let text = why(&rig.settle(&id, rev, "clear", None, WHY));
    assert!(
        text.contains(
            "settle refused: the kernel gives no pidfd of the caller (SO_PEERPIDFD), so settle cannot name it"
        ),
        "{text}"
    );
    unchanged(&rig, &id);
    // The control: with the fault off, the same caller is admitted.
    rig.live
        .controller
        .settle_faults()
        .fail_pidfd
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let resp = rig.settle(&id, rev, "clear", None, WHY);
    assert_eq!(resp["ok"], true, "{resp}");
}

#[test]
fn a_caller_with_no_socket_descriptor_is_refused() {
    let rig = SRig::new(&["u1"], Some(10));
    let (id, rev) = rig.request("u1");
    // The device path calls `handle`, which cannot tell who called.
    let resp = rig.live.controller.handle(Request::new(
        "settle",
        settle_args(&id, rev, "clear", None, WHY),
    ));
    assert!(!resp.ok);
    // A caller whose process the kernel named but whose connection the server could not duplicate.
    let resp = rig.live.controller.handle_from(
        Request::new("settle", settle_args(&id, rev, "clear", None, WHY)),
        Peer {
            pid: Some(std::process::id()),
        },
    );
    assert!(!resp.ok);
    assert!(
        resp.error
            .unwrap()
            .contains("settle refused: the controller holds no socket descriptor of the caller"),
    );
    unchanged(&rig, &id);
}

#[test]
fn a_caller_that_ended_before_the_last_read_is_refused() {
    let rig = SRig::new(&["u1"], Some(10));
    let (id, rev) = rig.request("u1");
    // The caller sends its request and exits at once. The pause holds the controller until it is
    // gone.
    rig.live
        .controller
        .settle_faults()
        .set_before_end_check(Some(Arc::new(|| {
            std::thread::sleep(Duration::from_millis(400))
        })));
    let (child, go) = rig.spawn_caller(
        true,
        &[],
        "settle",
        &settle_args(&id, rev, "clear", None, WHY),
        "helper_sends_the_request_and_exits",
    );
    std::fs::write(&go, "").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    rig.rec.pids.lock().unwrap().clear();
    // The refusal is audited, with the cause; nothing else was written.
    let end = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let refused = rig.refused_settles();
        if let Some((reason, _)) = refused.first() {
            // Wherever the controller meets the gone process, every read fails closed.
            assert!(
                reason.contains("exited")
                    || reason.contains("names no process")
                    || reason.contains("cannot read /proc/"),
                "{reason}"
            );
            break;
        }
        assert!(std::time::Instant::now() < end, "no audited refusal");
        std::thread::sleep(Duration::from_millis(20));
    }
    unchanged(&rig, &id);
}

// ------------------------------------------------------------------ the negative cases

#[test]
fn the_negative_cases_are_each_refused_with_nothing_written_but_the_audited_line() {
    let rig = SRig::with(&["u1"], Some(10), |cfg| {
        cfg.refusal_window = Duration::ZERO;
    });
    let (id, rev) = rig.request("u1");
    let args = settle_args(&id, rev, "clear", None, WHY);

    // A chat marker that names another session.
    let resp = rig.call_as(true, &[(MARKER, "S-9")], "settle", &args);
    assert!(
        why(&resp).contains("the chat marker names S-9, but the caller runs under chat process"),
        "{resp}"
    );
    // A marker and no chat on the chain: the marker never names a caller.
    let resp = rig.call_as(false, &[(MARKER, "S-1")], "settle", &args);
    assert!(
        why(&resp).contains("the marker never names the caller"),
        "{resp}"
    );
    // A seat token in the caller's environment.
    let resp = rig.call_as(
        true,
        &[("PF_SEAT_TOKEN", "seat-token-value-1234")],
        "settle",
        &args,
    );
    assert!(
        why(&resp).contains("PF_SEAT_TOKEN is in the caller's environment"),
        "{resp}"
    );
    // A tracked seat on the chain.
    let resp = rig.call_as_seat("settle", &args);
    assert!(why(&resp).contains("descends from, seat process"), "{resp}");
    // No chat on the chain at all.
    let resp = rig.call_as(false, &[], "settle", &args);
    assert!(
        why(&resp).contains("the caller runs under no chat session"),
        "{resp}"
    );

    unchanged(&rig, &id);
    // Every refusal is an audited line, and the token is in none.
    assert!(
        rig.refused_settles().len() >= 5,
        "{:#?}",
        rig.refused_settles()
    );
    let ledger = std::fs::read_to_string(rig.live.sb.state.join("controller.ledger")).unwrap();
    assert!(!ledger.contains("seat-token-value-1234"));
}

#[test]
fn another_projects_orchestrator_a_run_with_no_project_and_an_ended_session_are_refused() {
    // The session belongs to the first project; the second owns the run.
    let rig = SRig::linked(&["u1"], Some(10), Link::Other, |_| {});
    let (id, rev) = rig.request("u1");
    let args = settle_args(&id, rev, "clear", None, WHY);
    let text = why(&rig.call_as(true, &[], "settle", &args));
    assert!(
        text.contains("belongs to project P-2, and session S-1 belongs to project P-1"),
        "{text}"
    );
    unchanged(&rig, &id);

    // A run armed from the CLI has no project.
    let rig = SRig::linked(&["u1"], Some(10), Link::NoProject, |_| {});
    let (id, rev) = rig.request("u1");
    let args = settle_args(&id, rev, "clear", None, WHY);
    let text = why(&rig.call_as(true, &[], "settle", &args));
    assert!(
        text.contains("has no project (it was armed from the CLI)"),
        "{text}"
    );
    unchanged(&rig, &id);

    // A session that has ended.
    let rig = SRig::new(&["u1"], Some(10));
    let (id, rev) = rig.request("u1");
    let out = rig.live.cli(&["session", "end", &rig.session]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let resp = rig.call_as(
        true,
        &[],
        "settle",
        &settle_args(&id, rev, "clear", None, WHY),
    );
    assert!(why(&resp).contains("settle refused"), "{resp}");
    unchanged(&rig, &id);
}

#[test]
fn a_chat_session_of_another_agent_is_refused() {
    let rig = SRig::new(&["u1"], Some(10));
    let (id, rev) = rig.request("u1");
    // No verb makes another agent in this release, and the store keeps a session's agent. The test
    // builds the state through its own connection: it adds the agent, drops the guard trigger and
    // moves the session. The controller's agent check is then the only rule left that refuses.
    let conn = rusqlite::Connection::open(rig.live.sb.state.join("store.sqlite")).unwrap();
    conn.execute(
        "INSERT INTO agents VALUES ('helper', 'Helper', 'Helps', 0, 'accent', 'helper', NULL, \
         'claude-code', 'm', 'high', 'default', '[]', '{}', '{}', 't0')",
        [],
    )
    .unwrap();
    conn.execute("DROP TRIGGER chat_sessions_owner_never_changes", [])
        .unwrap();
    let moved = conn
        .execute(
            "UPDATE chat_sessions SET agent_id = 'helper' WHERE id = ?1",
            [&rig.session],
        )
        .unwrap();
    assert_eq!(moved, 1, "the session now runs the other agent");
    drop(conn);
    let text = why(&rig.settle(&id, rev, "clear", None, WHY));
    assert!(
        text.contains("runs agent helper, and only a session of the orchestrator agent settles"),
        "{text}"
    );
    unchanged(&rig, &id);
}

#[test]
fn under_systemd_the_grant_verbs_refuse_when_a_live_chat_hosts_cgroup_cannot_be_read() {
    // `A read that fails refuses` (ADR-0005, amended). The user manager gives no cgroup for the
    // chat's host unit, so the verbs cannot tell whether the caller runs in it, and they refuse.
    let unit = HostSeed::fake("S-1").unit;
    let stub = CgroupStub::new(&unit);
    let program = stub.program();
    let rig = SRig::with(&["u1"], Some(10), move |cfg| {
        cfg.host_launcher = HostLauncher::Systemd;
        cfg.systemctl = program;
    });
    let args = |verb: &str| json!({ "project_id": rig.project, "command_id": format!("{verb}-1") });
    let live = |rig: &SRig| {
        let list = cli_json(&rig.live, &["grant", "list"]);
        list["grants"][0]["live"].as_bool().unwrap()
    };
    assert!(
        live(&rig),
        "the rig gave the grant, with the cgroup readable"
    );
    stub.fail();
    let text = why(&rig.call_as(false, &[], "grant_revoke", &args("grant_revoke")));
    assert!(
        text.contains("grant_revoke refused: the user manager names no cgroup for"),
        "{text}"
    );
    assert!(live(&rig), "the refused revoke changed nothing");
    // The control: the stub names the cgroup again, the caller is in no chat host unit, and the
    // revoke is admitted. Then a refused `grant clear` gives no second grant.
    stub.answer(&unit);
    let resp = rig.call_as(false, &[], "grant_revoke", &args("grant_revoke"));
    assert_eq!(resp["ok"], true, "{resp}");
    assert!(!live(&rig));
    stub.fail();
    let text = why(&rig.call_as(false, &[], "grant_clear", &args("grant_clear")));
    assert!(
        text.contains("grant_clear refused: the user manager names no cgroup for"),
        "{text}"
    );
    assert!(!live(&rig), "the refused clear gave no grant");
}

#[test]
fn an_ancestor_that_exits_during_the_walk_is_refused() {
    let rig = SRig::new(&["u1"], Some(10));
    let (id, rev) = rig.request("u1");
    let args = settle_args(&id, rev, "clear", None, WHY);
    let (child, go) = rig.spawn_caller(true, &[], "settle", &args, "helper_calls_the_controller");
    let shell = child.id();
    // Between the walk and the end check the test ends the chat's shell, an ancestor of the caller.
    rig.live
        .controller
        .settle_faults()
        .set_before_end_check(Some(Arc::new(move || {
            let _ = std::process::Command::new("kill")
                .args(["-9", &shell.to_string()])
                .status();
            std::thread::sleep(Duration::from_millis(100));
        })));
    std::fs::write(&go, "").unwrap();
    let resp = rig.finish_caller(child);
    let text = why(&resp);
    assert!(
        text.contains("exited or was replaced during the check") || text.contains("exited"),
        "{text}"
    );
    unchanged(&rig, &id);
}

#[test]
fn an_ancestor_whose_pid_is_used_again_is_refused() {
    let rig = SRig::new(&["u1"], Some(10));
    let (id, rev) = rig.request("u1");
    let args = settle_args(&id, rev, "clear", None, WHY);
    let (child, go) = rig.spawn_caller(true, &[], "settle", &args, "helper_calls_the_controller");
    let shell = child.id();
    // A PID that is used again names a process with another start time. The test cannot make the
    // kernel give the chat shell's PID to a new process, so between the walk and the end check it
    // replaces the start time that the walk pinned for the chat's shell, and the shell stays alive.
    let faults = rig.live.controller.settle_faults();
    faults.reuse_top_ancestor_pid.store(true, Ordering::SeqCst);
    let alive = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&alive);
    faults.set_before_end_check(Some(Arc::new(move || {
        seen.store(
            std::path::Path::new(&format!("/proc/{shell}")).exists(),
            Ordering::SeqCst,
        );
    })));
    std::fs::write(&go, "").unwrap();
    let resp = rig.finish_caller(child);
    assert!(
        alive.load(Ordering::SeqCst),
        "the chat's shell was alive at the end check, so only its start time could refuse"
    );
    let text = why(&resp);
    assert!(
        text.starts_with("settle refused:")
            && text.contains(&format!(
                "process {shell} on the caller's chain exited or was replaced during the check"
            )),
        "{text}"
    );
    unchanged(&rig, &id);
    assert_eq!(rig.refused_settles().len(), 1, "the refusal is audited");
}

#[test]
fn the_check_writes_no_attach_line_and_opens_no_connection_to_the_chats_host() {
    let rig = SRig::new(&["u1"], Some(10));
    let (id, rev) = rig.request("u1");
    let unit = format!("pf-host-testserver-{}-1", rig.session);
    let before = controller_ledger(&rig.live);
    let resp = rig.settle(&id, rev, "clear", None, WHY);
    assert_eq!(resp["ok"], true, "{resp}");
    let after = controller_ledger(&rig.live);
    assert!(
        after[before.len()..].iter().all(|l| !l.contains("attach")),
        "{:#?}",
        &after[before.len()..]
    );
    assert!(
        !std::path::Path::new(&format!("/tmp/{unit}")).exists(),
        "no host socket was made or opened"
    );
    // A host row with an end is no live host: the session cannot be named from it.
    rig.live.queue_item("u2", 2);
    rig.tick();
    rig.live
        .handle()
        .engine
        .park(
            "u2",
            pf_engine::port::store::Cause::Stalled,
            "stalled: again",
        )
        .unwrap();
    rig.tick();
    let (id2, rev2) = rig.request("u2");
    let committed = commit(
        &rig.live,
        "host_ended",
        "he-1",
        json!({ "unit": unit }),
        vec![pf_engine::port::store::Effect::HostEnded {
            unit: unit.clone(),
            ended_at: "2026-11-24T00:00:00.000Z".to_string(),
            exit: "{}".to_string(),
        }],
    );
    assert!(
        matches!(committed, pf_engine::port::store::Committed::Applied { .. }),
        "{committed:?}"
    );
    let text = why(&rig.settle(&id2, rev2, "clear", None, WHY));
    assert!(
        text.contains(&format!("chat session {} has no live host", rig.session)),
        "{text}"
    );
}

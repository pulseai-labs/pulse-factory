//! AC-8 of `r2.s2.w3` (Q8, audit 12): the hook waits out a controller restart. With no controller to
//! answer, `hook` retries every 200 ms until `PF_HOOK_WAIT_SECONDS` (1 to 120, else 15) have passed
//! since its first attempt, then denies as before. A controller's answer, a deny included, is final
//! and is never retried. The hook is the real binary; the controller is a stub that answers one verb.

mod common;

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use common::BIN;
use pf_ipc::{Handler, Request, Response};
use pulse_factory::hook::wait_seconds;
use serde_json::{Value, json};

/// Answers `hook` with a fixed decision and counts the calls.
struct Stub {
    decision: &'static str,
    calls: Arc<AtomicUsize>,
}

impl Handler for Stub {
    fn handle(&self, request: Request) -> Response {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.verb, "hook");
        Response::ok(
            json!({ "decision": self.decision, "reason": "the stub says so", "rule": null }),
        )
    }
}

/// Serve the socket of `state` on a thread, from the moment the call returns.
fn serve(
    state: &Path,
    decision: &'static str,
) -> (Arc<AtomicUsize>, tokio::sync::oneshot::Sender<()>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let bound = pf_ipc::bind(state).unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let stub = Arc::new(Stub {
        decision,
        calls: calls.clone(),
    });
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(pf_ipc::serve(bound, stub, async {
                let _ = rx.await;
            }));
    });
    (calls, tx)
}

/// Run the hook binary on one call. `wait` is the value of `PF_HOOK_WAIT_SECONDS`, when set.
fn hook(state: &Path, wait: Option<&str>) -> (Value, Duration) {
    let mut cmd = Command::new(BIN);
    cmd.arg("hook")
        .env_clear()
        .env("PF_STATE_DIR", state)
        .env("PF_SESSION_KEY", "pfk_test")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(w) = wait {
        cmd.env("PF_HOOK_WAIT_SECONDS", w);
    }
    let start = Instant::now();
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            json!({"role": "x", "tool": "bash", "input": {"command": "ls"}})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let took = start.elapsed();
    (
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "not JSON ({e}): {}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        }),
        took,
    )
}

#[test]
fn the_wait_is_one_to_one_hundred_twenty_seconds_and_fifteen_otherwise() {
    assert_eq!(wait_seconds(None), 15);
    assert_eq!(wait_seconds(Some("")), 15);
    assert_eq!(wait_seconds(Some("0")), 15);
    assert_eq!(wait_seconds(Some("121")), 15);
    assert_eq!(wait_seconds(Some("-3")), 15);
    assert_eq!(wait_seconds(Some("ten")), 15);
    assert_eq!(wait_seconds(Some("2.5")), 15);
    assert_eq!(wait_seconds(Some("1")), 1);
    assert_eq!(wait_seconds(Some("2")), 2);
    assert_eq!(wait_seconds(Some("120")), 120);
    assert_eq!(wait_seconds(Some(" 7 ")), 7);
}

#[test]
fn with_no_controller_the_hook_denies_after_the_wait_and_before_twice_the_wait() {
    let state = tempfile::tempdir().unwrap();
    let (answer, took) = hook(state.path(), Some("2"));
    assert_eq!(answer["decision"], "deny", "{answer}");
    assert!(
        answer["reason"]
            .as_str()
            .unwrap()
            .contains("no controller answers"),
        "{answer}"
    );
    assert!(took >= Duration::from_secs(2), "{took:?}");
    assert!(took < Duration::from_secs(4), "{took:?}");
}

#[test]
fn a_controller_that_binds_its_socket_during_the_wait_answers_the_call() {
    let state = tempfile::tempdir().unwrap();
    let dir = state.path().to_path_buf();
    let late = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(1));
        serve(&dir, "allow")
    });
    let (answer, took) = hook(state.path(), Some("5"));
    let (calls, stop) = late.join().unwrap();
    assert_eq!(answer["decision"], "allow", "{answer}");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(took >= Duration::from_secs(1), "{took:?}");
    assert!(
        took < Duration::from_secs(3),
        "the hook did not stop waiting: {took:?}"
    );
    let _ = stop.send(());
}

#[test]
fn a_deny_answer_is_final_and_is_not_retried() {
    let state = tempfile::tempdir().unwrap();
    let (calls, stop) = serve(state.path(), "deny");
    let (answer, took) = hook(state.path(), Some("5"));
    assert_eq!(answer["decision"], "deny", "{answer}");
    assert_eq!(answer["reason"], "the stub says so");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "a controller's deny was asked again"
    );
    assert!(took < Duration::from_secs(2), "{took:?}");
    let _ = stop.send(());
}

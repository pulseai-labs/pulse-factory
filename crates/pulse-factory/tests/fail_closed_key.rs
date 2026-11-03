//! AC-8: the random source fails closed. A launch whose session key cannot be made is refused
//! with the cause named, and no key of zeros is issued. `claim` makes no token and no claim row.
//! The test seam is `Config.random_device`.

mod common;

use std::path::PathBuf;

use common::ctl::*;
use common::*;
use pf_engine::port::agent::AgentError;
use pulse_factory::controller::seat::random_bytes;
use serde_json::json;

/// A path that cannot be read as a random source: a directory opens, and its read fails.
fn unreadable(dir: &std::path::Path) -> PathBuf {
    let d = dir.join("not-a-device");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn short(dir: &std::path::Path) -> PathBuf {
    let f = dir.join("short");
    std::fs::write(&f, [7u8, 8, 9, 10, 11]).unwrap();
    f
}

fn zeros(dir: &std::path::Path) -> PathBuf {
    let f = dir.join("zeros");
    std::fs::write(&f, [0u8; 64]).unwrap();
    f
}

/// A controller whose random source is the path `device` makes in a directory of the test's own.
/// The directory is returned so it lives as long as the controller.
fn live_on(device: impl FnOnce(&std::path::Path) -> PathBuf) -> (Live, tempfile::TempDir) {
    let holder = tempfile::tempdir().unwrap();
    let path = device(holder.path());
    let live = Live::start_config(BIN.into(), move |cfg| cfg.random_device = path);
    (live, holder)
}

#[test]
fn random_bytes_names_the_cause_of_each_failure() {
    let dir = tempfile::tempdir().unwrap();

    let missing = dir.path().join("missing");
    let e = random_bytes(&missing, 32).unwrap_err();
    assert!(
        e.contains("cannot open") && e.contains(&missing.display().to_string()),
        "{e}"
    );

    let e = random_bytes(&unreadable(dir.path()), 32).unwrap_err();
    assert!(e.contains("cannot read the random source"), "{e}");

    let e = random_bytes(&short(dir.path()), 32).unwrap_err();
    assert!(e.contains("gave 5 bytes, and 32 are needed"), "{e}");

    let e = random_bytes(&zeros(dir.path()), 32).unwrap_err();
    assert!(e.contains("only zeros"), "{e}");
    let e = random_bytes(std::path::Path::new("/dev/zero"), 32).unwrap_err();
    assert!(e.contains("only zeros"), "{e}");
}

#[test]
fn the_system_source_gives_the_bytes_asked_for() {
    let a = random_bytes(std::path::Path::new("/dev/urandom"), 32).unwrap();
    let b = random_bytes(std::path::Path::new("/dev/urandom"), 32).unwrap();
    assert_eq!((a.len(), b.len()), (32, 32));
    assert_ne!(a, b);
}

fn launch_is_refused(device: impl FnOnce(&std::path::Path) -> PathBuf, cause: &str) {
    // A session key is 24 bytes; a claim token is 32.
    let (live, _device_dir) = live_on(device);
    live.arm();
    let settings = live.role_settings("lane", "u1");
    let before = live.ports.plans.lock().unwrap().len();
    let err = live.launch("lane", "u1", &settings).unwrap_err();
    match err {
        AgentError::Refused(why) => {
            assert!(why.contains("no session key can be made"), "{why}");
            assert!(why.contains(cause), "{why}");
        }
        other => panic!("expected Refused, got {other:?}"),
    }
    // Nothing reached the adapter, so no seat got a key, least of all one of zeros.
    assert_eq!(live.ports.plans.lock().unwrap().len(), before);
    assert!(
        live.ports
            .plans
            .lock()
            .unwrap()
            .iter()
            .all(|p| !p.settings.contains_key("env.PF_SESSION_KEY")),
    );
}

#[test]
fn a_launch_is_refused_when_the_random_source_cannot_be_read() {
    launch_is_refused(unreadable, "cannot read the random source");
}

#[test]
fn a_launch_is_refused_when_the_random_source_is_missing() {
    launch_is_refused(|d| d.join("missing"), "cannot open the random source");
}

#[test]
fn a_launch_is_refused_on_a_short_read() {
    launch_is_refused(short, "gave 5 bytes, and 24 are needed");
}

#[test]
fn a_launch_is_refused_when_the_source_gives_only_zeros() {
    launch_is_refused(zeros, "only zeros");
}

#[test]
fn claim_makes_no_token_and_no_claim_row_without_random_bytes() {
    for (name, device) in [
        ("unreadable", unreadable as fn(&std::path::Path) -> PathBuf),
        ("short", short),
        ("zeros", zeros),
    ] {
        let (live, _device_dir) = live_on(device);
        live.arm();

        // Through the socket.
        let resp = pf_ipc::call(&live.sb.state, "claim", json!({ "holder": "ann@host" })).unwrap();
        assert!(!resp.ok, "{name}: claim must be refused");
        let why = resp.error.unwrap();
        assert!(why.contains("no token can be made"), "{name}: {why}");
        assert!(
            resp.result.get("token").is_none() || resp.result["token"].is_null(),
            "{name}: no token in the reply"
        );

        // Through the CLI: exit 1, the cause on stderr, no token file.
        let out = live
            .sb
            .cmd(&["claim", "--holder", "ann@host"])
            .output()
            .unwrap();
        assert_eq!(code(&out), 1, "{name}: {}", stdout(&out));
        assert!(stderr(&out).contains("no token can be made"), "{name}");
        assert!(
            !live.sb.state.join("claim-token").exists(),
            "{name}: no token file"
        );

        // No claim row, no ledger line.
        let view = live.handle().engine.view().unwrap();
        assert!(view.claims.is_empty(), "{name}: {:?}", view.claims);
        assert!(
            !live.sb.read_ledger().contains("claim "),
            "{name}: no ledger line for a claim that was not made"
        );
    }
}

#[test]
fn the_system_source_still_makes_a_claim() {
    // The fail-closed path is the exception: with the real source the claim is made.
    let live = Live::start();
    live.arm();
    let out = live
        .sb
        .cmd(&["claim", "--holder", "ann@host"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(live.handle().engine.view().unwrap().claims.len(), 1);
}

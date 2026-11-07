//! AC-1 of `r1.s2.w4`: the one function that builds a `/v1/verbs/<verb>` request refuses a verb that
//! is not in `pf_api::ALLOWLIST`, as `bad_request`, before it connects.

use pf_client::error::ErrorKind;
use pf_client::request::verb_request;
use serde_json::json;

#[test]
fn the_allowlisted_verbs_build_a_post_to_their_path() {
    for verb in pf_api::ALLOWLIST {
        let r = verb_request(verb, &json!({ "a": 1 })).unwrap();
        assert_eq!(r.method, "POST");
        assert_eq!(r.path, format!("/v1/verbs/{verb}"));
        assert_eq!(r.body, Some(br#"{"a":1}"#.to_vec()));
    }
}

#[test]
fn halt_ack_and_an_unknown_verb_are_bad_requests() {
    for verb in [
        "halt",
        "ack",
        "no_such_verb",
        "",
        "project_add/../halt",
        "PROJECT_ADD",
    ] {
        let e = verb_request(verb, &json!({})).unwrap_err();
        assert_eq!(e.kind, ErrorKind::BadRequest, "{verb:?}");
        assert!(!e.message.is_empty());
    }
}

#[test]
fn the_arguments_must_be_a_json_object() {
    for args in [json!([]), json!(null), json!("x"), json!(3)] {
        let e = verb_request("project_list", &args).unwrap_err();
        assert_eq!(e.kind, ErrorKind::BadRequest, "{args}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_refused_verb_never_connects() {
    use pf_client::store::{FileStore, Paired};
    use pf_client::transport::Transport;
    use std::sync::Arc;

    // A listener that counts connections. The paired address is this listener.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = count.clone();
    tokio::spawn(async move {
        while listener.accept().await.is_ok() {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    });

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FileStore::new(dir.path().join("device.json")));
    use pf_client::store::DeviceStore;
    store
        .save(&Paired {
            address: addr,
            pin: "00".repeat(32),
            token: "ab".repeat(32),
            device: serde_json::from_value(json!({
                "id": "D-1", "name": "m", "paired_at": "t",
                "last_seen_at": null, "last_peer": null, "revoked_at": null
            }))
            .unwrap(),
        })
        .unwrap();
    let transport = Transport::new(store);
    for verb in ["halt", "ack", "nope"] {
        let e = transport.verb(verb, json!({})).await.unwrap_err();
        assert_eq!(e.kind, ErrorKind::BadRequest, "{verb}");
    }
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 0);
}

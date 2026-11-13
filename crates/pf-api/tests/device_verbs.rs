//! AC-6 of `r1.s4.w1`: the router over a fake host with the device verbs. The allowlist holds the
//! verbs a device uses to read and answer requests, a host refusal that names a word from the closed
//! list reaches the 422 body and the audit record, and any other word is dropped.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use pf_api::code::{hex, token_sha256};
use pf_api::guard::{GuardConfig, system_clock};
use pf_api::host::{ALLOWLIST, REFUSALS, is_allowed};
use pf_api::types::{ApiStatus, DeviceRecord, PairResult};
use pf_api::{ApiHost, PeerAddr, Refusal, RequestRecord, router_with};
use serde_json::{Value, json};
use tower::ServiceExt;

const TOKEN: &str = "ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12";
const PEER: IpAddr = IpAddr::V4(Ipv4Addr::new(100, 64, 0, 9));

#[derive(Default)]
struct Fake {
    calls: Mutex<Vec<(String, Value)>>,
    records: Mutex<Vec<RequestRecord>>,
    answer: Mutex<Option<pf_ipc::Response>>,
}

fn device() -> DeviceRecord {
    DeviceRecord {
        id: "D-1".to_string(),
        name: "macbook".to_string(),
        paired_at: "2026-11-07T10:00:00.000Z".to_string(),
        last_seen_at: None,
        last_peer: None,
        revoked_at: None,
    }
}

impl ApiHost for Fake {
    fn redeem(&self, _: &str, _: &str, _: IpAddr) -> Result<PairResult, Refusal> {
        Err(Refusal::Code)
    }

    fn device(&self, token_sha256_hex: &str, _peer: IpAddr) -> Option<DeviceRecord> {
        (Some(token_sha256_hex.to_string()) == token_sha256(TOKEN)).then(device)
    }

    fn call(&self, _device: &DeviceRecord, verb: &str, args: Value) -> pf_ipc::Response {
        self.calls.lock().unwrap().push((verb.to_string(), args));
        self.answer
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| pf_ipc::Response::ok(json!({ "schema_version": 1 })))
    }

    fn status(&self) -> ApiStatus {
        ApiStatus {
            schema_version: 1,
            version: "0.1.0".to_string(),
            key_spki_sha256: hex(&[7; 32]),
            device: None,
            server: None,
        }
    }

    fn audit(&self, record: &RequestRecord) {
        self.records.lock().unwrap().push(record.clone());
    }
}

fn rig() -> (Router, Arc<Fake>) {
    let fake = Arc::new(Fake::default());
    let no_limit = GuardConfig {
        max_failures: usize::MAX,
        ..GuardConfig::default()
    };
    (router_with(fake.clone(), no_limit, system_clock()), fake)
}

async fn post(app: &Router, verb: &str, body: Value) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri(format!("/v1/verbs/{verb}"))
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
        .body(Body::from(body.to_string()))
        .unwrap();
    request.extensions_mut().insert(PeerAddr(PEER));
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

/// A host answer that refuses with a text and, as the controller does, a word in `result`.
fn refused(text: &str, word: Option<&str>) -> pf_ipc::Response {
    let mut r = pf_ipc::Response::err(text);
    if let Some(word) = word {
        r.result = json!({ "refusal": word });
    }
    r
}

#[test]
fn the_allowlist_holds_the_device_verbs_and_the_ten_of_the_chat_spine() {
    for verb in [
        "requests",
        "show",
        "confirm_code",
        "answer",
        "clear",
        "ack",
        // The ten of r1.s3.w3.
        "project_add",
        "project_list",
        "project_remove",
        "session_list",
        "session_start",
        "session_send",
        "session_answer",
        "session_interrupt",
        "session_set",
        "session_end",
    ] {
        assert!(ALLOWLIST.contains(&verb), "{verb} is on the allowlist");
        assert!(is_allowed(verb), "{verb}");
    }
    for verb in [
        "claim",
        "hook",
        "halt",
        "device_pair",
        "device_revoke",
        "session_redact",
        "session_items",
    ] {
        assert!(!ALLOWLIST.contains(&verb), "{verb} is off the allowlist");
        assert!(!is_allowed(verb), "{verb}");
    }
}

#[tokio::test]
async fn a_verb_off_the_allowlist_answers_403_and_never_reaches_call() {
    let (app, fake) = rig();
    for verb in [
        "claim",
        "hook",
        "halt",
        "device_pair",
        "device_revoke",
        "session_redact",
        "session_items",
    ] {
        let (status, body) = post(&app, verb, json!({})).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{verb}");
        assert!(body["error"].as_str().unwrap().contains(verb), "{body}");
        assert!(body.get("refusal").is_none(), "{body}");
    }
    assert!(fake.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_device_verbs_reach_the_host() {
    let (app, fake) = rig();
    for verb in ["requests", "show", "confirm_code", "answer", "clear", "ack"] {
        let (status, _) = post(&app, verb, json!({ "request": "R-1" })).await;
        assert_eq!(status, StatusCode::OK, "{verb}");
    }
    let calls = fake.calls.lock().unwrap();
    let verbs: Vec<&str> = calls.iter().map(|(v, _)| v.as_str()).collect();
    assert_eq!(
        verbs,
        ["requests", "show", "confirm_code", "answer", "clear", "ack"]
    );
}

#[test]
fn the_refusal_list_is_the_nine_words_and_no_more() {
    assert_eq!(
        REFUSALS,
        [
            "code_unknown",
            "code_foreign",
            "code_used",
            "code_voided",
            "code_expired",
            "code_subject",
            "subject_moved",
            "claim_moved",
            "device_revoked",
        ]
    );
}

#[tokio::test]
async fn a_host_refusal_with_a_listed_word_reaches_the_422_body_and_the_audit_record() {
    for word in REFUSALS {
        let (app, fake) = rig();
        *fake.answer.lock().unwrap() = Some(refused("the code was used before", Some(word)));
        let (status, body) = post(&app, "ack", json!({ "request": "R-1" })).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{word}");
        assert_eq!(body["refusal"], *word, "{body}");
        assert_eq!(body["error"], "the code was used before", "{body}");
        let records = fake.records.lock().unwrap();
        let record = records.last().unwrap();
        assert_eq!(record.status, 422);
        assert_eq!(record.verb.as_deref(), Some("ack"));
        assert_eq!(record.device.as_deref(), Some("D-1"));
        assert_eq!(record.refusal.as_deref(), Some(*word), "{word}");
    }
}

#[tokio::test]
async fn a_word_outside_the_list_is_dropped_from_the_body_and_the_record() {
    for word in [
        "code_USED",
        "pfc1-0123456789abcdef0123456789abcdef",
        "",
        "code_used ",
        "totally_made_up",
    ] {
        let (app, fake) = rig();
        *fake.answer.lock().unwrap() = Some(refused("nope", Some(word)));
        let (status, body) = post(&app, "ack", json!({ "request": "R-1" })).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{word:?}");
        assert!(body.get("refusal").is_none(), "{word:?}: {body}");
        assert_eq!(body["error"], "nope");
        assert_eq!(fake.records.lock().unwrap().last().unwrap().refusal, None);
    }
    // A refusal result that is not an object, and one whose word is not text, are dropped too.
    for result in [json!("code_used"), json!({ "refusal": 7 }), json!(null)] {
        let (app, fake) = rig();
        let mut r = pf_ipc::Response::err("nope");
        r.result = result;
        *fake.answer.lock().unwrap() = Some(r);
        let (status, body) = post(&app, "ack", json!({})).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert!(body.get("refusal").is_none(), "{body}");
        assert_eq!(fake.records.lock().unwrap().last().unwrap().refusal, None);
    }
}

#[tokio::test]
async fn the_record_of_a_200_and_of_a_plain_refusal_holds_no_word() {
    let (app, fake) = rig();
    let (status, _) = post(&app, "requests", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    *fake.answer.lock().unwrap() = Some(refused("the request is unknown", None));
    let (status, body) = post(&app, "show", json!({ "id": "R-9" })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body.get("refusal").is_none(), "{body}");
    let records = fake.records.lock().unwrap();
    assert!(records.iter().all(|r| r.refusal.is_none()), "{records:?}");
}

// ------------------------------------------------------------------ `r1.s5.w5`: folder_list

#[test]
fn folder_list_is_on_the_allowlist_beside_the_verbs_it_joined() {
    assert!(ALLOWLIST.contains(&"folder_list"));
    assert!(is_allowed("folder_list"));
    // Once: an allowlist entry that appears twice is a merge error.
    assert_eq!(ALLOWLIST.iter().filter(|v| **v == "folder_list").count(), 1);
    // The verbs that change who acts stay off, and the new read opens none of them.
    for verb in [
        "claim",
        "halt",
        "device_pair",
        "device_revoke",
        "session_redact",
    ] {
        assert!(!is_allowed(verb), "{verb}");
    }
}

#[tokio::test]
async fn folder_list_reaches_the_host_with_its_arguments_and_its_record_names_the_verb() {
    let (app, fake) = rig();
    let (status, _) = post(&app, "folder_list", json!({ "path": "/home/dev/code" })).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = post(&app, "folder_list", json!({})).await;
    assert_eq!(status, StatusCode::OK);

    let calls = fake.calls.lock().unwrap();
    assert_eq!(
        *calls,
        [
            (
                "folder_list".to_string(),
                json!({ "path": "/home/dev/code" })
            ),
            ("folder_list".to_string(), json!({})),
        ]
    );
    let records = fake.records.lock().unwrap();
    assert_eq!(records.len(), 2, "one record for each call: {records:?}");
    for record in records.iter() {
        assert_eq!(record.verb.as_deref(), Some("folder_list"));
        assert_eq!(record.device.as_deref(), Some("D-1"));
        assert_eq!(record.status, 200);
        assert_eq!(record.refusal, None);
    }
}

#[tokio::test]
async fn folder_list_with_no_token_is_401_and_never_reaches_the_host() {
    let (app, fake) = rig();
    let mut request = Request::builder()
        .method("POST")
        .uri("/v1/verbs/folder_list")
        .body(Body::from("{}"))
        .unwrap();
    request.extensions_mut().insert(PeerAddr(PEER));
    let response = app.clone().oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(fake.calls.lock().unwrap().is_empty());
    let records = fake.records.lock().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].status, 401);
    assert_eq!(records[0].device, None);
}

#[tokio::test]
async fn a_folder_list_the_host_refuses_is_a_422_with_its_text_and_no_listing() {
    let (app, fake) = rig();
    *fake.answer.lock().unwrap() = Some(refused("the path is outside the home folder", None));

    let (status, body) = post(&app, "folder_list", json!({ "path": "/etc" })).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "the path is outside the home folder");
    assert!(body.get("entries").is_none(), "{body}");
    assert!(body.get("refusal").is_none(), "{body}");
    assert_eq!(fake.records.lock().unwrap().last().unwrap().status, 422);
}

//! AC-11 of `r1.s4.w2`: the router over a fake host with the device `arm`. `arm` joins the allowlist,
//! and every verb that stays off it still answers 403 without reaching the host. A host refusal on
//! `arm` that names `subject_moved` reaches the 422 body and the audit record, and five bad tokens on
//! `POST /v1/verbs/arm` from one peer make the sixth call 429.

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
const BAD_TOKEN: &str = "cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34";
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
        paired_at: "2026-11-10T10:00:00.000Z".to_string(),
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

/// A router with no failure limit, for the tests that send many calls with a good token.
fn rig() -> (Router, Arc<Fake>) {
    let fake = Arc::new(Fake::default());
    let no_limit = GuardConfig {
        max_failures: usize::MAX,
        ..GuardConfig::default()
    };
    (router_with(fake.clone(), no_limit, system_clock()), fake)
}

async fn post_with(app: &Router, token: &str, verb: &str, body: Value) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri(format!("/v1/verbs/{verb}"))
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::from(body.to_string()))
        .unwrap();
    request.extensions_mut().insert(PeerAddr(PEER));
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn post(app: &Router, verb: &str, body: Value) -> (StatusCode, Value) {
    post_with(app, TOKEN, verb, body).await
}

fn refused(text: &str, word: Option<&str>) -> pf_ipc::Response {
    let mut r = pf_ipc::Response::err(text);
    if let Some(word) = word {
        r.result = json!({ "refusal": word });
    }
    r
}

#[test]
fn arm_is_on_the_allowlist_with_the_ten_chat_verbs_w1s_six_and_w4s_three() {
    for verb in [
        "arm",
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
        // The six of r1.s4.w1.
        "requests",
        "show",
        "confirm_code",
        "answer",
        "clear",
        "ack",
        // The three of r1.s4.w4.
        "settings_get",
        "settings_set",
        "settings_unset",
    ] {
        assert!(ALLOWLIST.contains(&verb), "{verb} is on the allowlist");
        assert!(is_allowed(verb), "{verb}");
    }
    assert_eq!(ALLOWLIST.len(), 20, "{ALLOWLIST:?}");
}

#[tokio::test]
async fn the_verbs_that_stay_off_the_list_answer_403_and_never_reach_call() {
    let (app, fake) = rig();
    for verb in [
        "claim",
        "halt",
        "propose",
        "device_pair",
        "device_revoke",
        "session_redact",
        "session_items",
    ] {
        assert!(!is_allowed(verb), "{verb}");
        let (status, body) = post(&app, verb, json!({})).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{verb}");
        assert!(body["error"].as_str().unwrap().contains(verb), "{body}");
        assert!(body.get("refusal").is_none(), "{body}");
    }
    assert!(fake.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn arm_reaches_the_host_with_its_arguments() {
    let (app, fake) = rig();
    let args = json!({
        "proposal": "sp-0123456789abcdef",
        "plan_sha256": "0".repeat(64),
        "code": "pfc1-00000000000000000000000000000000",
        "command_id": "arm-1",
    });
    let (status, _) = post(&app, "arm", args.clone()).await;
    assert_eq!(status, StatusCode::OK);
    let calls = fake.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "arm");
    assert_eq!(calls[0].1, args);
}

#[tokio::test]
async fn a_host_refusal_on_arm_gives_a_422_with_the_word_and_an_audit_record_that_names_it() {
    let (app, fake) = rig();
    *fake.answer.lock().unwrap() = Some(refused(
        "arm refused: the package snapshot changed",
        Some("subject_moved"),
    ));
    let (status, body) = post(&app, "arm", json!({ "proposal": "sp-1" })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["refusal"], "subject_moved", "{body}");
    assert_eq!(body["error"], "arm refused: the package snapshot changed");
    let records = fake.records.lock().unwrap();
    let record = records.last().unwrap();
    assert_eq!(record.status, 422);
    assert_eq!(record.verb.as_deref(), Some("arm"));
    assert_eq!(record.device.as_deref(), Some("D-1"));
    assert_eq!(record.refusal.as_deref(), Some("subject_moved"));
}

#[tokio::test]
async fn five_bad_tokens_on_arm_from_one_peer_make_the_sixth_call_429() {
    let fake = Arc::new(Fake::default());
    let app = router_with(fake.clone(), GuardConfig::default(), system_clock());
    for n in 1..=5 {
        let (status, _) = post_with(&app, BAD_TOKEN, "arm", json!({})).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "failure {n}");
    }
    // The block covers the peer, even with a good token.
    let (status, _) = post_with(&app, TOKEN, "arm", json!({})).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        fake.calls.lock().unwrap().is_empty(),
        "a blocked peer reaches no host method"
    );
}

#[test]
fn the_refusal_list_is_still_w1s_nine_words() {
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

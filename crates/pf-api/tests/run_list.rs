//! AC-3 of `r1.s4.w3`: `run_list` on the router over a fake host, with no TLS. It is one allowlist
//! entry, it needs a live device token, and it reaches the host's `call` with its arguments. The
//! verbs that `router.rs` keeps off are still off, and `RunSummary` has exactly its eight fields.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use pf_api::code::{hex, token_sha256};
use pf_api::guard::{GuardConfig, system_clock};
use pf_api::types::{ApiStatus, DeviceRecord, PairResult, RunList, RunListArgs, RunSummary};
use pf_api::{ALLOWLIST, ApiHost, PeerAddr, Refusal, router_with};
use serde_json::{Value, json};
use tower::ServiceExt;

const TOKEN: &str = "ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12";
const OTHER_TOKEN: &str = "cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34";
const PEER: IpAddr = IpAddr::V4(Ipv4Addr::new(100, 64, 0, 9));

struct Fake {
    calls: Mutex<Vec<(String, Value)>>,
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

fn runs() -> Value {
    json!({"schema_version": 1, "runs": [{
        "id": "R-1", "spine": "s", "workflow": "w", "state": "active", "halted_reason": null,
        "end_reason": null, "open_requests": 2, "waiting": "waiting text"}]})
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
        pf_ipc::Response::ok(runs())
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
}

fn rig() -> (Router, Arc<Fake>) {
    let fake = Arc::new(Fake {
        calls: Mutex::new(Vec::new()),
    });
    let no_limit = GuardConfig {
        max_failures: usize::MAX,
        ..GuardConfig::default()
    };
    (router_with(fake.clone(), no_limit, system_clock()), fake)
}

async fn post(app: &Router, verb: &str, token: Option<&str>, body: &[u8]) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(format!("/v1/verbs/{verb}"))
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let mut request = builder.body(Body::from(body.to_vec())).unwrap();
    request.extensions_mut().insert(PeerAddr(PEER));
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn run_list_is_one_allowlist_entry_and_reaches_call_with_its_arguments() {
    assert!(ALLOWLIST.contains(&"run_list"));
    assert!(pf_api::host::is_allowed("run_list"));
    // One entry: no other verb about runs is on the list.
    let about_runs: Vec<_> = ALLOWLIST.iter().filter(|v| v.contains("run")).collect();
    assert_eq!(about_runs, vec![&"run_list"]);

    let (app, fake) = rig();
    let (status, body) = post(&app, "run_list", Some(TOKEN), b"{}").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, runs());
    // An unknown field is passed on as sent: the controller ignores it.
    let (status, _) = post(&app, "run_list", Some(TOKEN), br#"{"extra":1}"#).await;
    assert_eq!(status, StatusCode::OK);
    let calls = fake.calls.lock().unwrap().clone();
    assert_eq!(
        calls,
        vec![
            ("run_list".to_string(), json!({})),
            ("run_list".to_string(), json!({"extra": 1}))
        ]
    );
    // The answer is the generated type.
    let list: RunList = serde_json::from_value(body).unwrap();
    assert_eq!(list.runs[0].waiting.as_deref(), Some("waiting text"));
}

#[tokio::test]
async fn with_no_token_or_a_bad_one_run_list_is_401_and_never_reaches_call() {
    let (app, fake) = rig();
    for token in [None, Some(OTHER_TOKEN), Some("not-a-token")] {
        let (status, body) = post(&app, "run_list", token, b"{}").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{token:?}");
        assert!(body["error"].is_string());
    }
    assert!(fake.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn every_verb_router_rs_keeps_off_is_still_off() {
    let (app, fake) = rig();
    for verb in [
        "arm",
        "claim",
        "halt",
        "hook",
        "runs",
        "status",
        "device_pair",
        "device_list",
        "device_revoke",
        "api_status",
        "session_redact",
        "session_items",
    ] {
        assert!(!ALLOWLIST.contains(&verb), "{verb}");
        let (status, _) = post(&app, verb, Some(TOKEN), b"{}").await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{verb}");
    }
    assert!(fake.calls.lock().unwrap().is_empty());
}

#[test]
fn a_run_summary_has_exactly_its_eight_fields_and_the_args_are_empty() {
    let summary = RunSummary {
        id: "R-1".into(),
        spine: "s".into(),
        workflow: "w".into(),
        state: "ended".into(),
        halted_reason: None,
        end_reason: Some("done".into()),
        open_requests: 0,
        waiting: None,
    };
    let value = serde_json::to_value(&summary).unwrap();
    let mut keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "end_reason",
            "halted_reason",
            "id",
            "open_requests",
            "spine",
            "state",
            "waiting",
            "workflow"
        ]
    );
    assert_eq!(serde_json::to_value(RunListArgs {}).unwrap(), json!({}));
    // An unknown field of the arguments is ignored.
    let args: RunListArgs = serde_json::from_value(json!({"extra": 1})).unwrap();
    assert_eq!(args, RunListArgs {});
}

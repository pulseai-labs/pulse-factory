//! AC-3 of `r1.s2.w2`: the router over a fake host, with no TLS. A request reaches the host's `call`
//! only with a live token and an allowlisted verb. Every other case is refused before `call`.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use pf_api::code::{PairingCode, hex, token_sha256};
use pf_api::guard::{GuardConfig, system_clock};
use pf_api::types::{ApiStatus, DeviceRecord, PairResult};
use pf_api::{ALLOWLIST, ApiHost, MAX_BODY_BYTES, PeerAddr, Refusal, router_with};
use serde_json::{Value, json};
use tower::ServiceExt;

const TOKEN: &str = "ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12";
const REVOKED_TOKEN: &str = "cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34";
const SPKI: [u8; 32] = [7; 32];
const SECRET: [u8; 10] = [3; 10];
const PEER: IpAddr = IpAddr::V4(Ipv4Addr::new(100, 64, 0, 9));

/// What the fake host saw.
#[derive(Default)]
struct Seen {
    calls: Vec<(String, Value)>,
    device_lookups: Vec<String>,
    redeems: Vec<(String, String, IpAddr)>,
}

struct Fake {
    seen: Mutex<Seen>,
    /// What `call` answers.
    answer: Mutex<pf_ipc::Response>,
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
    fn redeem(
        &self,
        code_secret: &str,
        device_name: &str,
        peer: IpAddr,
    ) -> Result<PairResult, Refusal> {
        self.seen.lock().unwrap().redeems.push((
            code_secret.to_string(),
            device_name.to_string(),
            peer,
        ));
        if code_secret == hex(&SECRET) {
            Ok(PairResult {
                schema_version: 1,
                device: device(),
                token: TOKEN.to_string(),
            })
        } else {
            Err(Refusal::Code)
        }
    }

    fn device(&self, token_sha256_hex: &str, _peer: IpAddr) -> Option<DeviceRecord> {
        self.seen
            .lock()
            .unwrap()
            .device_lookups
            .push(token_sha256_hex.to_string());
        // The revoked token's hash is unknown to this host, as the store answers for a revoked row.
        (Some(token_sha256_hex.to_string()) == token_sha256(TOKEN)).then(device)
    }

    fn call(&self, _device: &DeviceRecord, verb: &str, args: Value) -> pf_ipc::Response {
        self.seen
            .lock()
            .unwrap()
            .calls
            .push((verb.to_string(), args));
        self.answer.lock().unwrap().clone()
    }

    fn status(&self) -> ApiStatus {
        ApiStatus {
            schema_version: 1,
            version: "0.1.0".to_string(),
            key_spki_sha256: hex(&SPKI),
            device: None,
            server: None,
        }
    }
}

fn rig() -> (Router, Arc<Fake>) {
    let fake = Arc::new(Fake {
        seen: Mutex::new(Seen::default()),
        answer: Mutex::new(pf_ipc::Response::ok(
            json!({ "schema_version": 1, "projects": [] }),
        )),
    });
    // These tests send dozens of bad tokens and codes from the one peer address, to check the
    // answers. The peer guard (`guard.rs`, `r1.s2.w5`) would block that peer on the fifth, so this
    // rig turns its limit up out of reach. The guard has its own tests (`tests/guard.rs`).
    let no_limit = GuardConfig {
        max_failures: usize::MAX,
        ..GuardConfig::default()
    };
    (router_with(fake.clone(), no_limit, system_clock()), fake)
}

struct Reply {
    status: StatusCode,
    body: Vec<u8>,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("not JSON ({e}): {}", String::from_utf8_lossy(&self.body)))
    }

    fn error(&self) -> String {
        self.json()["error"]
            .as_str()
            .unwrap_or_else(|| panic!("no error text in {}", String::from_utf8_lossy(&self.body)))
            .to_string()
    }
}

async fn send(app: &Router, mut request: Request<Body>) -> Reply {
    request.extensions_mut().insert(PeerAddr(PEER));
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        "application/json"
    );
    // Every answer of every route names the server: the 200s, the 401s, the 403s and the rest.
    assert_eq!(
        response
            .headers()
            .get("pf-server-id")
            .map(|v| v.to_str().unwrap()),
        Some(hex(&SPKI).as_str()),
        "pf-server-id on a {status} answer"
    );
    assert_eq!(
        response
            .headers()
            .get("pf-server-name")
            .map(|v| v.to_str().unwrap()),
        Some("server"),
        "pf-server-name on a {status} answer"
    );
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    Reply {
        status,
        body: body.to_vec(),
    }
}

fn request(method: &str, path: &str, token: Option<&str>, body: Vec<u8>) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(path);
    if let Some(t) = token {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    b.body(Body::from(body)).unwrap()
}

fn code_text(spki: [u8; 32], secret: [u8; 10]) -> String {
    PairingCode {
        secret,
        spki_sha256: spki,
        listen: "100.90.203.21:7443".to_string(),
    }
    .encode()
}

fn pair_body(code: &str, name: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({ "code": code, "device_name": name })).unwrap()
}

fn calls(fake: &Fake) -> usize {
    fake.seen.lock().unwrap().calls.len()
}

#[tokio::test]
async fn a_call_with_no_token_a_bad_token_or_an_unknown_token_answers_401_and_never_reaches_call() {
    let (app, fake) = rig();
    let paths = [
        ("POST", "/v1/verbs/project_list"),
        ("GET", "/v1/status"),
        ("POST", "/v1/verbs/halt"),
    ];
    let mut texts = Vec::new();
    for (method, path) in paths {
        // No header.
        texts.push(send(&app, request(method, path, None, b"{}".to_vec())).await);
        // Malformed: too short, not hex, too long.
        for bad in ["abc", &"zz".repeat(32), &"ab".repeat(33), ""] {
            texts.push(send(&app, request(method, path, Some(bad), b"{}".to_vec())).await);
        }
        // Another scheme, and two headers.
        let mut basic = request(method, path, None, b"{}".to_vec());
        basic.headers_mut().insert(
            header::AUTHORIZATION,
            format!("Basic {TOKEN}").parse().unwrap(),
        );
        texts.push(send(&app, basic).await);
        let mut two = request(method, path, Some(TOKEN), b"{}".to_vec());
        two.headers_mut().append(
            header::AUTHORIZATION,
            format!("Bearer {TOKEN}").parse().unwrap(),
        );
        texts.push(send(&app, two).await);
        // Well formed, and unknown to the host (a revoked token is unknown to it too).
        texts.push(
            send(
                &app,
                request(method, path, Some(REVOKED_TOKEN), b"{}".to_vec()),
            )
            .await,
        );
        texts.push(
            send(
                &app,
                request(method, path, Some(&"0f".repeat(32)), b"{}".to_vec()),
            )
            .await,
        );
    }
    for reply in &texts {
        assert_eq!(
            reply.status,
            StatusCode::UNAUTHORIZED,
            "{}",
            String::from_utf8_lossy(&reply.body)
        );
    }
    // One text for every reason: the answer never tells which.
    let first = texts[0].error();
    assert!(
        texts.iter().all(|r| r.error() == first),
        "the 401 texts differ"
    );
    assert_eq!(calls(&fake), 0, "no refused call reached the host");
    // The token never comes back in an answer.
    assert!(
        texts
            .iter()
            .all(|r| !String::from_utf8_lossy(&r.body).contains(TOKEN))
    );
    // A malformed token is refused before the host is asked: only well-formed tokens were looked up.
    let seen = fake.seen.lock().unwrap();
    assert!(
        seen.device_lookups.iter().all(|h| h.len() == 64),
        "{:?}",
        seen.device_lookups
    );
    assert_eq!(
        seen.device_lookups.len(),
        3 * 2,
        "per path: the revoked token and the unknown one were looked up, and nothing else"
    );
}

#[tokio::test]
async fn pair_with_a_bad_code_answers_401_and_uses_nothing_up() {
    let (app, fake) = rig();
    let wrong_key = code_text([8; 32], SECRET);
    let wrong_secret = code_text(SPKI, [4; 10]);
    let bodies = [
        pair_body("", "mac"),
        pair_body("hello", "mac"),
        pair_body("pf1-", "mac"),
        pair_body("pf1-!!!!", "mac"),
        pair_body(&wrong_key, "mac"),
        pair_body(&wrong_secret, "mac"),
    ];
    let mut texts = Vec::new();
    for body in bodies {
        let reply = send(&app, request("POST", "/v1/pair", None, body)).await;
        assert_eq!(
            reply.status,
            StatusCode::UNAUTHORIZED,
            "{}",
            String::from_utf8_lossy(&reply.body)
        );
        texts.push(reply.error());
    }
    assert!(texts.iter().all(|t| *t == texts[0]), "{texts:?}");
    // Only the code with this server's key and an unknown secret got as far as the host.
    let seen = fake.seen.lock().unwrap();
    assert_eq!(seen.redeems.len(), 1);
    assert_eq!(seen.redeems[0].0, hex(&[4; 10]));
    assert_eq!(calls_len(&seen), 0);
}

fn calls_len(seen: &Seen) -> usize {
    seen.calls.len()
}

#[tokio::test]
async fn pair_with_a_good_code_answers_the_device_and_the_token_once() {
    let (app, fake) = rig();
    let code = code_text(SPKI, SECRET);
    let reply = send(
        &app,
        request("POST", "/v1/pair", None, pair_body(&code, "macbook")),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    let result: PairResult = serde_json::from_slice(&reply.body).unwrap();
    assert_eq!(result.schema_version, 1);
    assert_eq!(result.device.id, "D-1");
    assert_eq!(result.token, TOKEN);
    assert_eq!(
        fake.seen.lock().unwrap().redeems,
        vec![(hex(&SECRET), "macbook".to_string(), PEER)],
        "the host gets the secret, the name and the peer, and never the whole code"
    );
    // A code the host refuses (used up) answers 401.
    let (app2, _) = rig();
    let used = code_text(SPKI, [5; 10]);
    let reply = send(
        &app2,
        request("POST", "/v1/pair", None, pair_body(&used, "macbook")),
    )
    .await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn pair_checks_the_name_before_it_uses_the_code_and_the_body_before_the_code() {
    let (app, fake) = rig();
    let code = code_text(SPKI, SECRET);
    for name in ["", &"n".repeat(65), "line\nbreak", "tab\there"] {
        let reply = send(
            &app,
            request("POST", "/v1/pair", None, pair_body(&code, name)),
        )
        .await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{name:?}");
    }
    assert!(
        fake.seen.lock().unwrap().redeems.is_empty(),
        "no bad name used the code"
    );
    // A name of 64 characters is fine.
    let reply = send(
        &app,
        request("POST", "/v1/pair", None, pair_body(&code, &"n".repeat(64))),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    // A body that is not JSON, or lacks a field, is a 400.
    for body in [&b"not json"[..], b"{}", b"{\"code\":\"x\"}", b"[]"] {
        let reply = send(&app, request("POST", "/v1/pair", None, body.to_vec())).await;
        assert_eq!(
            reply.status,
            StatusCode::BAD_REQUEST,
            "{}",
            String::from_utf8_lossy(body)
        );
    }
}

#[tokio::test]
async fn project_list_with_a_good_token_reaches_call_with_the_verb_and_the_body() {
    let (app, fake) = rig();
    let reply = send(
        &app,
        request(
            "POST",
            "/v1/verbs/project_list",
            Some(TOKEN),
            b"{}".to_vec(),
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.json(), json!({ "schema_version": 1, "projects": [] }));

    let add = json!({ "root": "/work/alpha", "name": "alpha", "command_id": "c-1" });
    let reply = send(
        &app,
        request(
            "POST",
            "/v1/verbs/project_add",
            Some(TOKEN),
            serde_json::to_vec(&add).unwrap(),
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);

    // An empty body is `{}`.
    send(
        &app,
        request("POST", "/v1/verbs/project_remove", Some(TOKEN), Vec::new()),
    )
    .await;

    let seen = fake.seen.lock().unwrap();
    assert_eq!(
        seen.calls,
        vec![
            ("project_list".to_string(), json!({})),
            ("project_add".to_string(), add),
            ("project_remove".to_string(), json!({})),
        ]
    );
}

#[tokio::test]
async fn every_verb_off_the_allowlist_answers_403_and_names_the_verb_without_reaching_call() {
    let (app, fake) = rig();
    let off = [
        "runs",
        "status",
        "claim",
        "halt",
        "hook",
        "device_pair",
        "device_list",
        "device_revoke",
        "api_status",
        "session_redact",
        "session_items",
        "no_such_verb",
        "Project_List",
        "project_list ",
    ];
    for verb in off {
        // The router names the verb as the path spells it.
        let spelled = verb.replace(' ', "%20");
        let path = format!("/v1/verbs/{spelled}");
        let reply = send(&app, request("POST", &path, Some(TOKEN), b"{}".to_vec())).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{verb}");
        assert!(
            reply.error().contains(&spelled),
            "{verb}: {}",
            reply.error()
        );
        // No token: the caller learns nothing about the allowlist.
        let reply = send(&app, request("POST", &path, None, b"{}".to_vec())).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{verb}");
    }
    assert_eq!(calls(&fake), 0);
    // The allowlist holds the ten verbs of the chat spine (the project verbs and the chat verbs), `arm` (`r1.s4.w2`), the
    // three settings verbs (`r1.s4.w4`), and `r1.s4.w1` adds the device verbs (`tests/device_verbs.rs`).
    // The test pins no exact count. `halt`, `claim`, `session_redact` (the CLI only) and
    // `session_items` (the stream reads the items) are off.
    for verb in [
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
        "settings_get",
        "settings_set",
        "settings_unset",
    ] {
        assert!(ALLOWLIST.contains(&verb), "{verb} is not on the allowlist");
    }
    for verb in [
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
        assert!(
            !ALLOWLIST.contains(&verb),
            "{verb} must stay off the allowlist"
        );
    }
}

#[tokio::test]
async fn every_chat_verb_on_the_allowlist_reaches_call_with_its_name_and_body() {
    let (app, fake) = rig();
    let chat = [
        "session_list",
        "session_start",
        "session_send",
        "session_answer",
        "session_interrupt",
        "session_set",
        "session_end",
    ];
    for verb in chat {
        let body = json!({ "verb": verb });
        let reply = send(
            &app,
            request(
                "POST",
                &format!("/v1/verbs/{verb}"),
                Some(TOKEN),
                serde_json::to_vec(&body).unwrap(),
            ),
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK, "{verb}");
    }
    let seen = fake.seen.lock().unwrap();
    let names: Vec<&str> = seen.calls.iter().map(|(v, _)| v.as_str()).collect();
    assert_eq!(names, chat);
}

#[tokio::test]
async fn every_settings_verb_on_the_allowlist_reaches_call_with_its_name_and_body() {
    let (app, fake) = rig();
    let verbs = ["settings_get", "settings_set", "settings_unset"];
    for verb in verbs {
        let body = json!({ "verb": verb, "key": "chat.self_turn_cap" });
        let reply = send(
            &app,
            request(
                "POST",
                &format!("/v1/verbs/{verb}"),
                Some(TOKEN),
                serde_json::to_vec(&body).unwrap(),
            ),
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK, "{verb}");
    }
    let seen = fake.seen.lock().unwrap();
    let names: Vec<&str> = seen.calls.iter().map(|(v, _)| v.as_str()).collect();
    assert_eq!(names, verbs);
}

#[tokio::test]
async fn the_stream_route_needs_a_token_and_an_upgrade_and_is_not_a_missing_path() {
    let (app, fake) = rig();
    // No token: 401, as on the other token routes.
    let reply = send(&app, request("GET", "/v1/stream", None, Vec::new())).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    // A good token and no upgrade headers: 400, not 404.
    let reply = send(&app, request("GET", "/v1/stream", Some(TOKEN), Vec::new())).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.error());
    // Another method: 405.
    let reply = send(&app, request("POST", "/v1/stream", Some(TOKEN), Vec::new())).await;
    assert_eq!(reply.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(calls(&fake), 0);
}

#[tokio::test]
async fn a_handler_refusal_answers_422_with_its_message() {
    let (app, fake) = rig();
    *fake.answer.lock().unwrap() = pf_ipc::Response::err("the root does not exist: /nope");
    let reply = send(
        &app,
        request("POST", "/v1/verbs/project_add", Some(TOKEN), b"{}".to_vec()),
    )
    .await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        reply.json(),
        json!({ "error": "the root does not exist: /nope" })
    );
    assert_eq!(calls(&fake), 1);
}

#[tokio::test]
async fn a_body_over_one_mib_answers_413_and_one_mib_exactly_goes_through() {
    let (app, fake) = rig();
    let padded = |n: usize| {
        let head = b"{\"pad\":\"";
        let tail = b"\"}";
        let fill = n - head.len() - tail.len();
        let mut body = head.to_vec();
        body.extend(std::iter::repeat_n(b'x', fill));
        body.extend_from_slice(tail);
        assert_eq!(body.len(), n);
        body
    };
    assert_eq!(MAX_BODY_BYTES, 1 << 20);
    let reply = send(
        &app,
        request(
            "POST",
            "/v1/verbs/project_add",
            Some(TOKEN),
            padded(MAX_BODY_BYTES + 1),
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(calls(&fake), 0, "the call did not run");
    let reply = send(
        &app,
        request(
            "POST",
            "/v1/verbs/project_add",
            Some(TOKEN),
            padded(MAX_BODY_BYTES),
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(calls(&fake), 1);
    // The same limit holds for the pairing route.
    let reply = send(
        &app,
        request("POST", "/v1/pair", None, padded(MAX_BODY_BYTES + 1)),
    )
    .await;
    assert_eq!(reply.status, StatusCode::PAYLOAD_TOO_LARGE);
    // A caller with no token never has its body read: the 401 comes first.
    let reply = send(
        &app,
        request(
            "POST",
            "/v1/verbs/project_add",
            None,
            padded(MAX_BODY_BYTES + 1),
        ),
    )
    .await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn other_paths_answer_404_and_a_wrong_method_never_reaches_call() {
    let (app, fake) = rig();
    for (method, path) in [
        ("GET", "/v2/status"),
        ("POST", "/v2/pair"),
        ("GET", "/"),
        ("GET", "/v1"),
        ("GET", "/v1/nothing"),
        ("POST", "/v1/verbs/"),
        ("POST", "/v1/verbs/project_list/extra"),
        ("POST", "/v1/status/x"),
    ] {
        let reply = send(&app, request(method, path, Some(TOKEN), b"{}".to_vec())).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{method} {path}");
    }
    for (method, path) in [
        ("GET", "/v1/verbs/project_list"),
        ("DELETE", "/v1/verbs/project_list"),
        ("POST", "/v1/status"),
        ("GET", "/v1/pair"),
    ] {
        let reply = send(&app, request(method, path, Some(TOKEN), b"{}".to_vec())).await;
        assert_eq!(
            reply.status,
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} {path}"
        );
    }
    assert_eq!(calls(&fake), 0);
}

#[tokio::test]
async fn status_answers_the_version_the_key_and_the_calling_device() {
    let (app, _fake) = rig();
    let reply = send(&app, request("GET", "/v1/status", Some(TOKEN), Vec::new())).await;
    assert_eq!(reply.status, StatusCode::OK);
    let status: ApiStatus = serde_json::from_slice(&reply.body).unwrap();
    assert_eq!(status.schema_version, 1);
    assert_eq!(status.version, "0.1.0");
    assert_eq!(status.key_spki_sha256, hex(&SPKI));
    assert_eq!(status.device, Some(device()));
    // The record never holds the token or its hash.
    let text = String::from_utf8_lossy(&reply.body).to_string();
    assert!(!text.contains(TOKEN));
    assert!(!text.contains(&token_sha256(TOKEN).unwrap()));
}

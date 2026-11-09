//! AC-2 of `r1.s2.w5`: the router calls `ApiHost::audit` once for every request it answers, with the
//! route, the verb, the device, the status and the outcome. A blocked peer's 429s are coalesced into
//! one record a minute. The host here is a fake that records the calls. There is no TLS.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use pf_api::code::{PairingCode, hex, token_sha256};
use pf_api::guard::{Clock, GuardConfig};
use pf_api::types::{ApiStatus, DeviceRecord, PairResult};
use pf_api::{
    ApiHost, MAX_BODY_BYTES, Outcome, PeerAddr, Refusal, RequestRecord, Route, router_with,
};
use serde_json::{Value, json};
use tower::ServiceExt;

const TOKEN: &str = "ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12";
const BAD_TOKEN: &str = "cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34";
const SPKI: [u8; 32] = [7; 32];
const SECRET: [u8; 10] = [3; 10];
const PEER: IpAddr = IpAddr::V4(Ipv4Addr::new(100, 64, 0, 9));
const BASE_SECS: u64 = 1_800_000_000;

#[derive(Clone, Default)]
struct Time(Arc<Mutex<Duration>>);

impl Time {
    fn advance(&self, by: Duration) {
        *self.0.lock().unwrap() += by;
    }

    fn clock(&self) -> Clock {
        let t = self.0.clone();
        Arc::new(move || UNIX_EPOCH + Duration::from_secs(BASE_SECS) + *t.lock().unwrap())
    }
}

/// What `call` does.
enum Mode {
    Answer(pf_ipc::Response),
    Panic,
}

struct Fake {
    audits: Mutex<Vec<RequestRecord>>,
    mode: Mutex<Mode>,
}

fn device() -> DeviceRecord {
    DeviceRecord {
        id: "D-7".to_string(),
        name: "macbook".to_string(),
        paired_at: "2026-11-07T10:00:00.000Z".to_string(),
        last_seen_at: None,
        last_peer: None,
        revoked_at: None,
    }
}

impl ApiHost for Fake {
    fn redeem(&self, secret: &str, _name: &str, _peer: IpAddr) -> Result<PairResult, Refusal> {
        if secret == hex(&SECRET) {
            Ok(PairResult {
                schema_version: 1,
                device: device(),
                token: TOKEN.to_string(),
            })
        } else {
            Err(Refusal::Code)
        }
    }

    fn device(&self, sha: &str, _peer: IpAddr) -> Option<DeviceRecord> {
        (Some(sha.to_string()) == token_sha256(TOKEN)).then(device)
    }

    fn call(&self, _d: &DeviceRecord, _verb: &str, _args: Value) -> pf_ipc::Response {
        // The lock is dropped before the panic, so the panic does not poison it.
        let answer = match &*self.mode.lock().unwrap() {
            Mode::Answer(r) => Some(r.clone()),
            Mode::Panic => None,
        };
        answer.unwrap_or_else(|| panic!("the host fell over"))
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

    fn audit(&self, record: &RequestRecord) {
        self.audits.lock().unwrap().push(record.clone());
    }
}

struct Rig {
    app: Router,
    fake: Arc<Fake>,
    time: Time,
}

fn rig() -> Rig {
    let fake = Arc::new(Fake {
        audits: Mutex::new(Vec::new()),
        mode: Mutex::new(Mode::Answer(pf_ipc::Response::ok(
            json!({ "schema_version": 1 }),
        ))),
    });
    let time = Time::default();
    let app = router_with(fake.clone(), GuardConfig::default(), time.clock());
    Rig { app, fake, time }
}

impl Rig {
    fn answer_with(&self, response: pf_ipc::Response) {
        *self.fake.mode.lock().unwrap() = Mode::Answer(response);
    }

    /// Send one request and return its status and the one record it left.
    async fn once(&self, request: Request<Body>) -> (StatusCode, RequestRecord) {
        self.once_from(PEER, request).await
    }

    async fn once_from(
        &self,
        peer: IpAddr,
        mut request: Request<Body>,
    ) -> (StatusCode, RequestRecord) {
        let before = self.fake.audits.lock().unwrap().len();
        request.extensions_mut().insert(PeerAddr(peer));
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let audits = self.fake.audits.lock().unwrap();
        assert_eq!(
            audits.len(),
            before + 1,
            "{status}: audit ran {} times",
            audits.len() - before
        );
        (status, audits.last().unwrap().clone())
    }

    /// Send one request that may leave no record.
    async fn send(&self, peer: IpAddr, mut request: Request<Body>) -> StatusCode {
        request.extensions_mut().insert(PeerAddr(peer));
        self.app.clone().oneshot(request).await.unwrap().status()
    }

    fn records(&self) -> usize {
        self.fake.audits.lock().unwrap().len()
    }
}

fn request(method: &str, path: &str, token: Option<&str>, body: Vec<u8>) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(path);
    if let Some(t) = token {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    b.body(Body::from(body)).unwrap()
}

fn pair_with(secret: [u8; 10]) -> Request<Body> {
    let code = PairingCode {
        secret,
        spki_sha256: SPKI,
        listen: "100.90.203.21:7443".to_string(),
    }
    .encode();
    let body = serde_json::to_vec(&json!({ "code": code, "device_name": "mac" })).unwrap();
    request("POST", "/v1/pair", None, body)
}

fn verb(name: &str, token: Option<&str>, body: &[u8]) -> Request<Body> {
    request("POST", &format!("/v1/verbs/{name}"), token, body.to_vec())
}

#[tokio::test]
async fn every_answer_calls_audit_once_with_its_outcome_and_status() {
    let rig = rig();

    // A good verb call.
    let (status, r) = rig.once(verb("project_list", Some(TOKEN), b"{}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (
            r.route,
            r.verb.as_deref(),
            r.device.as_deref(),
            r.status,
            r.outcome
        ),
        (
            Route::Verb,
            Some("project_list"),
            Some("D-7"),
            200,
            Outcome::Ok
        )
    );
    assert_eq!((r.peer, r.count, r.blocked_until), (PEER, 1, None));

    // An answer with "replayed": true.
    rig.answer_with(pf_ipc::Response::ok(
        json!({ "schema_version": 1, "replayed": true }),
    ));
    let (_, r) = rig.once(verb("project_add", Some(TOKEN), b"{}")).await;
    assert_eq!((r.status, r.outcome), (200, Outcome::Replayed));
    // "replayed": false is a plain ok.
    rig.answer_with(pf_ipc::Response::ok(
        json!({ "schema_version": 1, "replayed": false }),
    ));
    let (_, r) = rig.once(verb("project_add", Some(TOKEN), b"{}")).await;
    assert_eq!((r.status, r.outcome), (200, Outcome::Ok));

    // No token.
    let (_, r) = rig.once(verb("project_list", None, b"{}")).await;
    assert_eq!(
        (r.route, r.device, r.status, r.outcome),
        (Route::Verb, None, 401, Outcome::Unauthorized)
    );

    // A verb off the allowlist: the token matched, so the device is named.
    let (_, r) = rig.once(verb("halt", Some(TOKEN), b"{}")).await;
    assert_eq!(
        (r.verb.as_deref(), r.device.as_deref(), r.status, r.outcome),
        (Some("halt"), Some("D-7"), 403, Outcome::Forbidden)
    );

    // A refusal of the handler.
    rig.answer_with(pf_ipc::Response::err("the root does not exist: /nope"));
    let (_, r) = rig.once(verb("project_add", Some(TOKEN), b"{}")).await;
    assert_eq!((r.status, r.outcome), (422, Outcome::Refused));

    // A host call that panics.
    *rig.fake.mode.lock().unwrap() = Mode::Panic;
    let (status, r) = rig.once(verb("project_add", Some(TOKEN), b"{}")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        (r.device.as_deref(), r.status, r.outcome),
        (Some("D-7"), 500, Outcome::Failed)
    );
    rig.answer_with(pf_ipc::Response::ok(json!({})));

    // A body that is not JSON.
    let (_, r) = rig
        .once(verb("project_add", Some(TOKEN), b"not json"))
        .await;
    assert_eq!((r.status, r.outcome), (400, Outcome::BadRequest));

    // A body of 1 MiB + 1 byte.
    let big = vec![b' '; MAX_BODY_BYTES + 1];
    let (_, r) = rig.once(verb("project_add", Some(TOKEN), &big)).await;
    assert_eq!((r.status, r.outcome), (413, Outcome::TooLarge));

    // Another version, and the wrong method.
    let (_, r) = rig
        .once(request("GET", "/v2/status", None, Vec::new()))
        .await;
    assert_eq!(
        (r.route, r.verb, r.device, r.status, r.outcome),
        (Route::Other, None, None, 404, Outcome::NotFound)
    );
    let (_, r) = rig.once(request("GET", "/v1/pair", None, Vec::new())).await;
    assert_eq!(
        (r.route, r.status, r.outcome),
        (Route::Pair, 405, Outcome::MethodNotAllowed)
    );

    // The status route, with a token.
    let (_, r) = rig
        .once(request("GET", "/v1/status", Some(TOKEN), Vec::new()))
        .await;
    assert_eq!(
        (r.route, r.verb, r.device.as_deref(), r.status, r.outcome),
        (Route::Status, None, Some("D-7"), 200, Outcome::Ok)
    );

    // A bad code, and then a good one.
    let (_, r) = rig.once(pair_with([4; 10])).await;
    assert_eq!(
        (r.route, r.device, r.status, r.outcome),
        (Route::Pair, None, 401, Outcome::Unauthorized)
    );
    let (_, r) = rig.once(pair_with(SECRET)).await;
    assert_eq!(
        (r.route, r.status, r.outcome),
        (Route::Pair, 200, Outcome::Ok)
    );

    // A pairing refusal for the name is a 422.
    let body = serde_json::to_vec(&json!({ "code": "x", "device_name": "" })).unwrap();
    let (_, r) = rig.once(request("POST", "/v1/pair", None, body)).await;
    // The code "x" is bad, so the answer is 401 before the name is looked at.
    assert_eq!((r.status, r.outcome), (401, Outcome::Unauthorized));
}

#[tokio::test]
async fn the_stream_route_leaves_one_record_with_the_route_stream() {
    let rig = rig();
    // A good token and no upgrade headers: the 400 is a record of the route `stream`, and it names
    // the device and no verb.
    let (status, r) = rig
        .once(request("GET", "/v1/stream", Some(TOKEN), Vec::new()))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        (r.route, r.verb, r.device.as_deref(), r.status, r.outcome),
        (Route::Stream, None, Some("D-7"), 400, Outcome::BadRequest)
    );
    // A bad token is a record of the same route, with no device.
    let (_, r) = rig
        .once(request("GET", "/v1/stream", Some(BAD_TOKEN), Vec::new()))
        .await;
    assert_eq!(
        (r.route, r.device, r.status, r.outcome),
        (Route::Stream, None, 401, Outcome::Unauthorized)
    );
}

#[tokio::test]
async fn a_bad_token_leaves_a_record_with_no_device() {
    let rig = rig();
    let (_, r) = rig
        .once(request("GET", "/v1/status", Some(BAD_TOKEN), Vec::new()))
        .await;
    assert_eq!(
        (r.device, r.status, r.outcome),
        (None, 401, Outcome::Unauthorized)
    );
    // A matched token carries its device.
    let (_, r) = rig
        .once(request("GET", "/v1/status", Some(TOKEN), Vec::new()))
        .await;
    assert_eq!(r.device.as_deref(), Some("D-7"));
}

#[tokio::test]
async fn the_fifth_failure_says_when_the_block_ends() {
    let rig = rig();
    for n in 1..=4 {
        let (_, r) = rig.once(pair_with([4; 10])).await;
        assert_eq!(r.blocked_until, None, "failure {n}");
    }
    let (status, r) = rig.once(pair_with([4; 10])).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.outcome, Outcome::Unauthorized);
    assert_eq!(r.blocked_until.as_deref(), Some("2027-01-15T08:10:00.000Z"));
}

#[tokio::test]
async fn a_blocked_peers_requests_are_counted_in_one_record_a_minute() {
    let rig = rig();
    for _ in 0..5 {
        rig.send(PEER, pair_with([4; 10])).await;
    }
    assert_eq!(rig.records(), 5);
    // The first 429 writes a record with count 1.
    let (status, r) = rig
        .once(request("GET", "/v1/status", Some(TOKEN), Vec::new()))
        .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        (r.status, r.outcome, r.count, r.peer, r.device),
        (429, Outcome::Blocked, 1, PEER, None)
    );
    // 29 more inside the minute make no record.
    for _ in 0..29 {
        let at = rig.records();
        let status = rig
            .send(PEER, request("GET", "/v1/status", Some(TOKEN), Vec::new()))
            .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(rig.records(), at);
    }
    // One more, 61 seconds after the line: one record that counts all 30 since then.
    rig.time.advance(Duration::from_secs(61));
    let (status, r) = rig.once(pair_with(SECRET)).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!((r.outcome, r.count), (Outcome::Blocked, 30));
    // A request 10 seconds later is inside the next minute: no record.
    rig.time.advance(Duration::from_secs(10));
    let at = rig.records();
    rig.send(PEER, pair_with(SECRET)).await;
    assert_eq!(rig.records(), at);
    // Another peer still gets a record for each of its requests.
    let other = IpAddr::V4(Ipv4Addr::new(100, 64, 0, 10));
    let (status, r) = rig
        .once_from(other, request("GET", "/v1/status", Some(TOKEN), Vec::new()))
        .await;
    assert_eq!(
        (status, r.outcome, r.peer),
        (StatusCode::OK, Outcome::Ok, other)
    );
}

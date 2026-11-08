//! AC-1 of `r1.s2.w5`: the peer guard over a fake host, with an injected clock and no TLS. Five bad
//! codes, or five bad tokens, from one peer inside 10 minutes block that peer for 10 minutes on every
//! route. A blocked request reaches no host method but `audit`. Each peer is counted alone.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use pf_api::code::{PairingCode, hex, token_sha256};
use pf_api::guard::{Clock, Failure, Guard, GuardConfig};
use pf_api::types::{ApiStatus, DeviceRecord, PairResult};
use pf_api::{ApiHost, PeerAddr, Refusal, RequestRecord, router_with};
use serde_json::{Value, json};
use tower::ServiceExt;

const TOKEN: &str = "ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12";
const BAD_TOKEN: &str = "cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34";
const SPKI: [u8; 32] = [7; 32];
const SECRET: [u8; 10] = [3; 10];
const BASE_SECS: u64 = 1_800_000_000;

fn peer(n: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(100, 64, 0, n))
}

/// A clock a test moves by hand.
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

/// A host that counts every method except `audit`, which it only records.
#[derive(Default)]
struct Fake {
    calls: Mutex<usize>,
    audits: Mutex<Vec<RequestRecord>>,
}

impl Fake {
    fn reached(&self) -> usize {
        *self.calls.lock().unwrap()
    }

    fn hit(&self) {
        *self.calls.lock().unwrap() += 1;
    }
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
    fn redeem(&self, secret: &str, _name: &str, _peer: IpAddr) -> Result<PairResult, Refusal> {
        self.hit();
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
        self.hit();
        (Some(sha.to_string()) == token_sha256(TOKEN)).then(device)
    }

    fn call(&self, _d: &DeviceRecord, _verb: &str, _args: Value) -> pf_ipc::Response {
        self.hit();
        pf_ipc::Response::ok(json!({ "schema_version": 1 }))
    }

    fn status(&self) -> ApiStatus {
        self.hit();
        ApiStatus {
            schema_version: 1,
            version: "0.1.0".to_string(),
            key_spki_sha256: hex(&SPKI),
            device: None,
        }
    }

    fn audit(&self, record: &RequestRecord) {
        self.audits.lock().unwrap().push(record.clone());
    }
}

fn rig() -> (Router, Arc<Fake>, Time) {
    let fake = Arc::new(Fake::default());
    let time = Time::default();
    let app = router_with(fake.clone(), GuardConfig::default(), time.clock());
    (app, fake, time)
}

struct Reply {
    status: StatusCode,
    retry_after: Option<String>,
    body: Value,
}

async fn send(app: &Router, from: IpAddr, mut request: Request<Body>) -> Reply {
    request.extensions_mut().insert(PeerAddr(from));
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let retry_after = response
        .headers()
        .get(header::RETRY_AFTER)
        .map(|v| v.to_str().unwrap().to_string());
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    Reply {
        status,
        retry_after,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    }
}

fn request(method: &str, path: &str, token: Option<&str>, body: Vec<u8>) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(path);
    if let Some(t) = token {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    b.body(Body::from(body)).unwrap()
}

fn code_text(secret: [u8; 10]) -> String {
    PairingCode {
        secret,
        spki_sha256: SPKI,
        listen: "100.90.203.21:7443".to_string(),
    }
    .encode()
}

fn pair(code: &str) -> Request<Body> {
    let body = serde_json::to_vec(&json!({ "code": code, "device_name": "mac" })).unwrap();
    request("POST", "/v1/pair", None, body)
}

fn bad_code() -> Request<Body> {
    pair(&code_text([4; 10]))
}

fn good_code() -> Request<Body> {
    pair(&code_text(SECRET))
}

fn bad_token(path: &str) -> Request<Body> {
    request("GET", path, Some(BAD_TOKEN), Vec::new())
}

fn good_status() -> Request<Body> {
    request("GET", "/v1/status", Some(TOKEN), Vec::new())
}

fn minutes(n: u64) -> Duration {
    Duration::from_secs(60 * n)
}

#[tokio::test]
async fn the_fifth_bad_code_starts_a_block_that_covers_every_route() {
    let (app, fake, _time) = rig();
    let a = peer(1);
    for n in 1..=5 {
        let reply = send(&app, a, bad_code()).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "failure {n}");
        assert!(reply.retry_after.is_none(), "failure {n}");
    }
    let before = fake.reached();
    let probes = [
        pair(&code_text(SECRET)),
        request("GET", "/v1/status", Some(TOKEN), Vec::new()),
        request(
            "POST",
            "/v1/verbs/project_list",
            Some(TOKEN),
            b"{}".to_vec(),
        ),
        request("POST", "/v1/verbs/halt", None, Vec::new()),
        request("GET", "/v2/status", None, Vec::new()),
        request("GET", "/v1/pair", None, Vec::new()),
    ];
    for probe in probes {
        let reply = send(&app, a, probe).await;
        assert_eq!(reply.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(reply.retry_after.as_deref(), Some("600"));
        let text = reply.body["error"].as_str().unwrap();
        assert!(
            text.starts_with("too many failed attempts from this address: try again after "),
            "{text}"
        );
        // 1_800_000_000 is 2027-01-15T08:00:00Z; the block ends 10 minutes on.
        assert!(text.ends_with("2027-01-15T08:10:00.000Z"), "{text}");
    }
    assert_eq!(
        fake.reached(),
        before,
        "a blocked request reaches no host method"
    );
}

#[tokio::test]
async fn five_bad_tokens_block_the_peer_and_the_block_covers_pairing() {
    let (app, fake, _time) = rig();
    let a = peer(1);
    let paths = [
        "/v1/status",
        "/v1/verbs/project_list",
        "/v1/status",
        "/v1/verbs/halt",
        "/v1/status",
    ];
    for (n, path) in paths.iter().enumerate() {
        let method = if path.contains("verbs") {
            "POST"
        } else {
            "GET"
        };
        let reply = send(
            &app,
            a,
            request(method, path, Some(BAD_TOKEN), b"{}".to_vec()),
        )
        .await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "failure {}", n + 1);
    }
    let before = fake.reached();
    let reply = send(&app, a, good_code()).await;
    assert_eq!(reply.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(fake.reached(), before);
    // A missing token is a failed token too.
    let (app, _fake, _time) = rig();
    for _ in 0..5 {
        let reply = send(&app, a, request("GET", "/v1/status", None, Vec::new())).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    }
    let reply = send(&app, a, good_status()).await;
    assert_eq!(reply.status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn failures_spread_over_more_than_ten_minutes_never_block() {
    let (app, _fake, time) = rig();
    let a = peer(1);
    // One failure every 3 minutes: at most 4 sit inside any 10-minute window.
    for n in 0..30 {
        let reply = send(&app, a, bad_code()).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "failure {n}");
        time.advance(minutes(3));
    }
    // Four, a pause past the window, four more.
    let (app, _fake, time) = rig();
    for _ in 0..4 {
        send(&app, a, bad_token("/v1/status")).await;
    }
    time.advance(minutes(10));
    for n in 0..4 {
        let reply = send(&app, a, bad_token("/v1/status")).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "late failure {n}");
    }
    assert_eq!(send(&app, a, good_status()).await.status, StatusCode::OK);
}

#[tokio::test]
async fn the_peer_is_served_again_after_ten_minutes_of_block() {
    let (app, _fake, time) = rig();
    let a = peer(1);
    for _ in 0..5 {
        send(&app, a, bad_code()).await;
    }
    assert_eq!(
        send(&app, a, good_status()).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
    time.advance(Duration::from_secs(599));
    let reply = send(&app, a, good_status()).await;
    assert_eq!(reply.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(reply.retry_after.as_deref(), Some("1"));
    time.advance(Duration::from_secs(1));
    assert_eq!(send(&app, a, good_status()).await.status, StatusCode::OK);
    // The block cleared the count: one more failure does not block again.
    send(&app, a, bad_code()).await;
    assert_eq!(send(&app, a, good_status()).await.status, StatusCode::OK);
}

#[tokio::test]
async fn a_second_peer_is_served_and_pairs_while_the_first_is_blocked() {
    let (app, _fake, _time) = rig();
    for _ in 0..5 {
        send(&app, peer(1), bad_code()).await;
    }
    assert_eq!(
        send(&app, peer(1), good_code()).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
    let reply = send(&app, peer(2), good_code()).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body["token"], TOKEN);
    assert_eq!(
        send(&app, peer(2), good_status()).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_success_between_failures_does_not_clear_the_count() {
    let (app, _fake, _time) = rig();
    let a = peer(1);
    for _ in 0..3 {
        send(&app, a, bad_code()).await;
    }
    assert_eq!(send(&app, a, good_code()).await.status, StatusCode::OK);
    assert_eq!(send(&app, a, good_status()).await.status, StatusCode::OK);
    send(&app, a, bad_code()).await;
    let fifth = send(&app, a, bad_code()).await;
    assert_eq!(fifth.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        send(&app, a, good_status()).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn the_guard_holds_at_most_4096_peers() {
    let time = Time::default();
    let guard = Guard::new(GuardConfig::default(), time.clock());
    for n in 0..5000u32 {
        let [a, b, c, d] = (0x0a00_0000 + n).to_be_bytes();
        guard.fail(IpAddr::V4(Ipv4Addr::new(a, b, c, d)), Failure::Code);
        assert!(guard.len() <= 4096, "{} entries after {n}", guard.len());
    }
    assert_eq!(guard.len(), 4096);
    // The oldest entries went first: the newest peer still has its failure on file, so four more
    // block it, and the oldest peer has none, so four more do not.
    let newest = IpAddr::V4(Ipv4Addr::from(0x0a00_0000u32 + 4999));
    let oldest = IpAddr::V4(Ipv4Addr::from(0x0a00_0000u32));
    for _ in 0..3 {
        assert!(guard.fail(newest, Failure::Code).is_none());
        assert!(guard.fail(oldest, Failure::Code).is_none());
    }
    assert!(guard.fail(newest, Failure::Code).is_some());
    assert!(guard.fail(oldest, Failure::Code).is_none());
}

#[tokio::test]
async fn expired_entries_leave_the_map() {
    let time = Time::default();
    let guard = Guard::new(GuardConfig::default(), time.clock());
    for n in 0..100u8 {
        guard.fail(peer(n), Failure::Token);
    }
    assert_eq!(guard.len(), 100);
    time.advance(minutes(11));
    // Any later call may sweep. One failure from a new peer leaves only that peer.
    guard.fail(peer(200), Failure::Token);
    assert_eq!(guard.len(), 1);
}

#[tokio::test]
async fn a_400_on_pair_counts_as_a_failed_code() {
    let (app, _fake, _time) = rig();
    let a = peer(1);
    for n in 1..=5 {
        let reply = send(
            &app,
            a,
            request("POST", "/v1/pair", None, b"not json".to_vec()),
        )
        .await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "failure {n}");
    }
    assert_eq!(
        send(&app, a, good_code()).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn the_two_counters_are_separate() {
    let (app, _fake, _time) = rig();
    let a = peer(1);
    for _ in 0..4 {
        assert_eq!(
            send(&app, a, bad_code()).await.status,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            send(&app, a, bad_token("/v1/status")).await.status,
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(send(&app, a, good_code()).await.status, StatusCode::OK);
    assert_eq!(send(&app, a, good_status()).await.status, StatusCode::OK);
}

#[tokio::test]
async fn a_422_a_403_and_a_404_count_toward_neither_counter() {
    let (app, _fake, _time) = rig();
    let a = peer(1);
    for _ in 0..12 {
        let body =
            serde_json::to_vec(&json!({ "code": code_text(SECRET), "device_name": "" })).unwrap();
        let reply = send(&app, a, request("POST", "/v1/pair", None, body)).await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
        let reply = send(
            &app,
            a,
            request("POST", "/v1/verbs/halt", Some(TOKEN), b"{}".to_vec()),
        )
        .await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN);
        let reply = send(&app, a, request("GET", "/v2/status", None, Vec::new())).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND);
    }
    assert_eq!(send(&app, a, good_code()).await.status, StatusCode::OK);
    assert_eq!(send(&app, a, good_status()).await.status, StatusCode::OK);
}

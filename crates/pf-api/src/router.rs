//! The router: the routes of version 1 over a host port (ADR-0012, `docs/contracts/api.md`).
//!
//! - `POST /v1/pair`, no token: a pairing code and a device name in, a device token out.
//! - `GET /v1/status`, token: the server's version and key, and the calling device.
//! - `POST /v1/verbs/<verb>`, token: one allowlisted verb, through the controller's verb handler.
//! - `GET /v1/stream`, token: a WebSocket upgrade. The token is checked at the upgrade, the host opens
//!   the stream, and the router carries the host's frames to the socket and ends it with the host's
//!   close reason (`r1.s3.w3`).
//!
//! Every answer, the `101` of an upgrade and every error included, names the server in the headers
//! `pf-server-id` and `pf-server-name`.
//!
//! The token goes in `Authorization: Bearer <token>`. A missing, malformed, unknown or revoked token
//! answers 401 with one text, so the answer never tells which. A verb off the allowlist answers 403
//! and names the verb. A refusal from the handler answers 422 with its message. A body over 1 MiB
//! answers 413. Any other path answers 404. Every answer is JSON.
//!
//! The router checks the token before it reads a body or looks at the verb, so a caller with no
//! token learns nothing else. It runs each host call on a blocking thread: a verb may wait on the
//! store.
//!
//! Before any of that, the peer guard (`guard.rs`) checks the peer address. A peer that has sent 5
//! bad codes, or 5 bad tokens, inside 10 minutes answers 429 on every route for 10 minutes, and its
//! request reaches no host method.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequest, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use http_body_util::{BodyExt, LengthLimitError, Limited};
use serde::Serialize;

use crate::code::{PairingCode, hex, token_sha256};
use crate::guard::{Blocked, Clock, Failure, Guard, GuardConfig, system_clock};
use crate::host::{
    ApiHost, Outcome, REFUSALS, Refusal, RequestRecord, Route, StreamEnd, StreamEvent, StreamFeed,
    StreamRefusal, is_allowed,
};
use crate::types::{ApiError, DeviceRecord, PairRequest, ServerRef, check_device_name};

/// The longest request body, in bytes.
pub const MAX_BODY_BYTES: usize = 1 << 20;

/// The text of every 401 on a token route. It is one text for every reason.
const NOT_AUTHORIZED: &str = "not authorized";

/// The most streams one device holds open at once. The next upgrade is 429.
pub const MAX_STREAMS_PER_DEVICE: usize = 8;

/// How often the server pings an open stream.
const PING_EVERY: Duration = Duration::from_secs(30);

/// How long the router waits for a client to answer a close frame before it drops the socket.
const CLOSE_GRACE: Duration = Duration::from_secs(1);

/// The close code of a stream whose client sent data: policy violation.
const CLOSE_READ_ONLY: u16 = 1008;
/// The close code of a stream that ends because the controller stops: going away.
const CLOSE_GOING_AWAY: u16 = 1001;
/// The close code of a stream whose device was revoked.
pub const CLOSE_REVOKED: u16 = 4001;
/// The close code of a stream that fell behind the live frames.
pub const CLOSE_LAGGED: u16 = 4002;

/// The address of the peer that opened the connection. The listener adds it to each request as an
/// extension. A request with no such extension counts as from `0.0.0.0`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeerAddr(pub IpAddr);

/// What every request needs: the host, the peer guard, the server's name and the count of open
/// streams. Clones share the one guard and the one count.
#[derive(Clone)]
struct App {
    host: Arc<dyn ApiHost>,
    guard: Arc<Guard>,
    streams: Arc<Streams>,
    /// Read from the host once, when the router is made: a controller's key and name do not change
    /// while it runs, and a blocked peer's request reaches no host method.
    server: ServerRef,
}

/// The open streams of each device. A slot is taken before the host opens a stream and given back
/// when the socket ends or the upgrade fails.
#[derive(Default)]
struct Streams {
    open: Mutex<HashMap<String, usize>>,
}

/// One place among a device's streams. Dropping it gives the place back.
struct Slot {
    streams: Arc<Streams>,
    device: String,
}

impl Streams {
    fn take(self: &Arc<Self>, device: &str) -> Option<Slot> {
        let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        let held = open.entry(device.to_string()).or_insert(0);
        if *held >= MAX_STREAMS_PER_DEVICE {
            return None;
        }
        *held += 1;
        Some(Slot {
            streams: self.clone(),
            device: device.to_string(),
        })
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut open = self.streams.open.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(held) = open.get_mut(&self.device) {
            *held = held.saturating_sub(1);
            if *held == 0 {
                open.remove(&self.device);
            }
        }
    }
}

/// The router of the API over a host, with the default numbers of the peer guard and the system
/// clock.
pub fn router(host: Arc<dyn ApiHost>) -> Router {
    router_with(host, GuardConfig::default(), system_clock())
}

/// The router of the API with its own guard numbers and clock. A test moves time with the clock.
pub fn router_with(host: Arc<dyn ApiHost>, config: GuardConfig, clock: Clock) -> Router {
    let app = App {
        server: host.server(),
        host,
        guard: Arc::new(Guard::new(config, clock)),
        streams: Arc::new(Streams::default()),
    };
    Router::new().fallback(handle).with_state(app)
}

/// An answer, and what the router learned while it made it.
struct Answer {
    response: Response,
    /// The device whose token matched.
    device: Option<String>,
    /// A 200 whose verb answer says `"replayed": true`.
    replayed: bool,
    /// The word of a refused device verb, when it is one of [`REFUSALS`].
    refusal: Option<String>,
}

impl From<Response> for Answer {
    fn from(response: Response) -> Answer {
        Answer {
            response,
            device: None,
            replayed: false,
            refusal: None,
        }
    }
}

/// The outcome the audit record names for a status.
fn outcome_of(status: StatusCode, replayed: bool) -> Outcome {
    match status {
        StatusCode::OK if replayed => Outcome::Replayed,
        StatusCode::OK => Outcome::Ok,
        StatusCode::SWITCHING_PROTOCOLS => Outcome::Upgraded,
        StatusCode::UNAUTHORIZED => Outcome::Unauthorized,
        StatusCode::FORBIDDEN => Outcome::Forbidden,
        StatusCode::UNPROCESSABLE_ENTITY => Outcome::Refused,
        StatusCode::BAD_REQUEST => Outcome::BadRequest,
        StatusCode::PAYLOAD_TOO_LARGE => Outcome::TooLarge,
        StatusCode::NOT_FOUND => Outcome::NotFound,
        StatusCode::METHOD_NOT_ALLOWED => Outcome::MethodNotAllowed,
        StatusCode::TOO_MANY_REQUESTS => Outcome::Blocked,
        _ => Outcome::Failed,
    }
}

/// Hand one record to the host on a blocking thread: a host may write a file. A host that panics
/// costs the request nothing.
async fn audit(host: &Arc<dyn ApiHost>, record: RequestRecord) {
    let host = host.clone();
    let _ = tokio::task::spawn_blocking(move || host.audit(&record)).await;
}

fn json(status: StatusCode, body: &impl Serialize) -> Response {
    let bytes = serde_json::to_vec(body).unwrap_or_default();
    let mut response = (status, bytes).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    // A token answer must never sit in a cache.
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    json(
        status,
        &ApiError {
            error: message.into(),
            refusal: None,
        },
    )
}

/// The refusal word a host answer carries in `result.refusal`, when it is on the closed list.
fn refusal_word(result: &serde_json::Value) -> Option<String> {
    let word = result.get("refusal")?.as_str()?;
    REFUSALS.contains(&word).then(|| word.to_string())
}

fn not_authorized() -> Response {
    error(StatusCode::UNAUTHORIZED, NOT_AUTHORIZED)
}

fn failed_inside() -> Response {
    error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "the controller failed inside this call",
    )
}

/// Name the server in an answer: `pf-server-id` (when the server has a key) and `pf-server-name`.
/// A value that is no header value is left out.
fn name_server(mut response: Response, server: &ServerRef) -> Response {
    let headers = response.headers_mut();
    if let Some(id) = server.id.as_deref()
        && let Ok(value) = HeaderValue::from_str(id)
    {
        headers.insert("pf-server-id", value);
    }
    if let Ok(value) = HeaderValue::from_str(&server.name) {
        headers.insert("pf-server-name", value);
    }
    response
}

/// The route a path names, and the verb text when it names one. The method is not looked at.
fn classify(path: &str) -> (Route, Option<&str>) {
    match path {
        "/v1/pair" => (Route::Pair, None),
        "/v1/status" => (Route::Status, None),
        "/v1/stream" => (Route::Stream, None),
        other => match other.strip_prefix("/v1/verbs/") {
            Some(verb) if !verb.is_empty() && !verb.contains('/') => (Route::Verb, Some(verb)),
            _ => (Route::Other, None),
        },
    }
}

/// The 429 of a blocked peer.
fn blocked_answer(block: &Blocked) -> Response {
    let mut response = error(
        StatusCode::TOO_MANY_REQUESTS,
        format!(
            "too many failed attempts from this address: try again after {}",
            block.until
        ),
    );
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from(block.retry_after));
    response
}

async fn handle(State(app): State<App>, request: Request) -> Response {
    let peer = request
        .extensions()
        .get::<PeerAddr>()
        .map_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED), |p| p.0);
    let post = request.method() == axum::http::Method::POST;
    let get = request.method() == axum::http::Method::GET;
    let path = request.uri().path().to_string();
    let (route, verb) = classify(&path);
    let verb_text = verb.map(str::to_string);
    // The block check comes first: before the token, the body and the verb. The path is read only
    // to name the route in the record.
    if let Some(block) = app.guard.blocked(peer) {
        // A blocked peer's 429s are counted: one record for the first, then one a minute.
        if let Some(count) = app.guard.count_blocked(peer) {
            let record = RequestRecord {
                peer,
                route,
                verb: verb_text,
                device: None,
                status: StatusCode::TOO_MANY_REQUESTS.as_u16(),
                outcome: Outcome::Blocked,
                count,
                blocked_until: None,
                refusal: None,
            };
            audit(&app.host, record).await;
        }
        return name_server(blocked_answer(&block), &app.server);
    }
    let host = app.host.clone();
    let answer: Answer = match route {
        Route::Pair if post => pair(host, peer, request).await,
        Route::Status if get => status(host, peer, request).await,
        Route::Verb if post => {
            verb_call(host, peer, verb.unwrap_or_default().to_string(), request).await
        }
        Route::Stream if get => stream(host, app.streams.clone(), peer, request).await,
        Route::Pair | Route::Status | Route::Verb | Route::Stream => {
            error(StatusCode::METHOD_NOT_ALLOWED, "method not allowed").into()
        }
        Route::Other => error(StatusCode::NOT_FOUND, "not found").into(),
    };
    let code = answer.response.status();
    let failure = match route {
        Route::Pair if code == StatusCode::BAD_REQUEST || code == StatusCode::UNAUTHORIZED => {
            Some(Failure::Code)
        }
        Route::Status | Route::Verb | Route::Stream if code == StatusCode::UNAUTHORIZED => {
            Some(Failure::Token)
        }
        _ => None,
    };
    let blocked_until = failure.and_then(|kind| app.guard.fail(peer, kind));
    let record = RequestRecord {
        peer,
        route,
        verb: verb_text,
        device: answer.device,
        status: code.as_u16(),
        outcome: outcome_of(code, answer.replayed),
        count: 1,
        blocked_until,
        refusal: answer.refusal,
    };
    audit(&app.host, record).await;
    name_server(answer.response, &app.server)
}

/// The token of a request, as the SHA-256 the store keeps. `None` for a missing header, another
/// scheme or a token that is not 64 hex digits.
fn bearer_sha256(request: &Request) -> Option<String> {
    let mut values = request.headers().get_all(header::AUTHORIZATION).iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    let (scheme, token) = value.to_str().ok()?.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("Bearer") {
        return None;
    }
    token_sha256(token)
}

/// The live device whose token has this SHA-256, or the 401 (or 500) that answers the request. The
/// hash is read from the request before this call, so no borrow of the request crosses an await.
async fn authenticate(
    host: &Arc<dyn ApiHost>,
    sha: Option<String>,
    peer: IpAddr,
) -> Result<DeviceRecord, Box<Response>> {
    let sha = sha.ok_or_else(|| Box::new(not_authorized()))?;
    let host = host.clone();
    match tokio::task::spawn_blocking(move || host.device(&sha, peer)).await {
        Ok(Some(device)) => Ok(device),
        Ok(None) => Err(Box::new(not_authorized())),
        Err(_) => Err(Box::new(failed_inside())),
    }
}

async fn read_body(request: Request) -> Result<Bytes, Box<Response>> {
    match Limited::new(request.into_body(), MAX_BODY_BYTES)
        .collect()
        .await
    {
        Ok(collected) => Ok(collected.to_bytes()),
        Err(e) if e.downcast_ref::<LengthLimitError>().is_some() => Err(Box::new(error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "the body is over 1 MiB",
        ))),
        Err(_) => Err(Box::new(error(
            StatusCode::BAD_REQUEST,
            "the body could not be read",
        ))),
    }
}

async fn status(host: Arc<dyn ApiHost>, peer: IpAddr, request: Request) -> Answer {
    let device = match authenticate(&host, bearer_sha256(&request), peer).await {
        Ok(d) => d,
        Err(refusal) => return (*refusal).into(),
    };
    let id = device.id.clone();
    let response = match tokio::task::spawn_blocking(move || host.status()).await {
        Ok(mut status) => {
            status.device = Some(device);
            json(StatusCode::OK, &status)
        }
        Err(_) => failed_inside(),
    };
    Answer {
        response,
        device: Some(id),
        replayed: false,
        refusal: None,
    }
}

async fn verb_call(host: Arc<dyn ApiHost>, peer: IpAddr, verb: String, request: Request) -> Answer {
    let device = match authenticate(&host, bearer_sha256(&request), peer).await {
        Ok(d) => d,
        Err(refusal) => return (*refusal).into(),
    };
    let id = device.id.clone();
    let named = |response: Response| Answer {
        response,
        device: Some(id.clone()),
        replayed: false,
        refusal: None,
    };
    if !is_allowed(&verb) {
        let shown: String = verb.chars().take(64).collect();
        return named(error(
            StatusCode::FORBIDDEN,
            format!("the verb {shown:?} is not open to devices"),
        ));
    }
    let body = match read_body(request).await {
        Ok(b) => b,
        Err(refusal) => return named(*refusal),
    };
    let args = if body.iter().all(u8::is_ascii_whitespace) {
        serde_json::json!({})
    } else {
        match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(_) => return named(error(StatusCode::BAD_REQUEST, "the body is not JSON")),
        }
    };
    match tokio::task::spawn_blocking(move || host.call(&device, &verb, args)).await {
        Ok(answer) if answer.ok => {
            let replayed = answer.result.get("replayed") == Some(&serde_json::Value::Bool(true));
            Answer {
                response: json(StatusCode::OK, &answer.result),
                device: Some(id.clone()),
                replayed,
                refusal: None,
            }
        }
        Ok(answer) => {
            // A device verb that refuses may name why, with one word of a closed list (`r1.s4.w1`).
            let refusal = refusal_word(&answer.result);
            let text = answer
                .error
                .unwrap_or_else(|| "the controller refused".to_string());
            Answer {
                response: json(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    &ApiError {
                        error: text,
                        refusal: refusal.clone(),
                    },
                ),
                device: Some(id.clone()),
                replayed: false,
                refusal,
            }
        }
        Err(_) => named(failed_inside()),
    }
}

/// The query of a stream: `session=<S-n>` and `after=<seq>`, each at most once. `None` for no
/// session, and 0 for no `after`. Any other key, a session that is no `S-<n>` and an `after` that is
/// no whole number are a 400 with the reason.
fn parse_stream_query(query: Option<&str>) -> Result<(Option<String>, u64), String> {
    let (mut session, mut after) = (None, None);
    for pair in query
        .unwrap_or_default()
        .split('&')
        .filter(|p| !p.is_empty())
    {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        match key {
            "session" if session.is_none() => {
                let digits = value.strip_prefix("S-").unwrap_or_default();
                if digits.is_empty()
                    || digits.len() > 18
                    || !digits.bytes().all(|b| b.is_ascii_digit())
                {
                    return Err("session is a chat session ID such as S-1".to_string());
                }
                session = Some(value.to_string());
            }
            "after" if after.is_none() => {
                if value.is_empty()
                    || value.len() > 18
                    || !value.bytes().all(|b| b.is_ascii_digit())
                {
                    return Err("after is a whole number: the last seq the client has".to_string());
                }
                after = value.parse::<u64>().ok();
            }
            _ => {
                return Err(format!(
                    "the query has no parameter {:?}, or has it twice",
                    key.chars().take(32).collect::<String>()
                ));
            }
        }
    }
    Ok((session, after.unwrap_or(0)))
}

/// `GET /v1/stream`: the token, the upgrade headers, the query, the device's place, the host's open,
/// then `101`. The order is the contract's, so a caller with no token learns nothing else.
async fn stream(
    host: Arc<dyn ApiHost>,
    streams: Arc<Streams>,
    peer: IpAddr,
    request: Request,
) -> Answer {
    let device = match authenticate(&host, bearer_sha256(&request), peer).await {
        Ok(d) => d,
        Err(refusal) => return (*refusal).into(),
    };
    let id = device.id.clone();
    let named = |response: Response| Answer {
        response,
        device: Some(id.clone()),
        replayed: false,
        refusal: None,
    };
    let query = request.uri().query().map(str::to_string);
    let upgrade = match WebSocketUpgrade::from_request(request, &()).await {
        Ok(upgrade) => upgrade,
        Err(_) => {
            return named(error(
                StatusCode::BAD_REQUEST,
                "this route needs a WebSocket upgrade",
            ));
        }
    };
    let (session, after) = match parse_stream_query(query.as_deref()) {
        Ok(parsed) => parsed,
        Err(why) => return named(error(StatusCode::BAD_REQUEST, why)),
    };
    let Some(slot) = streams.take(&device.id) else {
        return named(error(
            StatusCode::TOO_MANY_REQUESTS,
            "too many open streams",
        ));
    };
    let opened =
        tokio::task::spawn_blocking(move || host.open_stream(&device, session.as_deref(), after))
            .await;
    let feed = match opened {
        Ok(Ok(feed)) => feed,
        Ok(Err(StreamRefusal::NotFound(why))) => return named(error(StatusCode::NOT_FOUND, why)),
        Ok(Err(StreamRefusal::BadRequest(why))) => {
            return named(error(StatusCode::BAD_REQUEST, why));
        }
        Ok(Err(StreamRefusal::NotAuthorized)) => return named(not_authorized()),
        Err(_) => return named(failed_inside()),
    };
    // The client sends nothing: its frames are refused, so a small message bound is enough.
    let response = upgrade
        .max_message_size(4096)
        .max_frame_size(4096)
        .on_upgrade(move |socket| carry(socket, feed, slot));
    named(response)
}

/// A close frame with a code and a reason.
fn close_frame(code: u16, reason: &str) -> Message {
    Message::Close(Some(CloseFrame {
        code,
        reason: reason.into(),
    }))
}

/// Carry the host's frames to the socket until the host ends the feed, the client leaves, or the
/// client sends data. The server pings every 30 seconds. The place of the stream is given back when
/// this ends.
async fn carry(mut socket: WebSocket, mut feed: StreamFeed, _slot: Slot) {
    let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + PING_EVERY, PING_EVERY);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let closing = loop {
        tokio::select! {
            event = feed.recv() => match event {
                Some(StreamEvent::Frame(text)) => {
                    if socket.send(Message::Text(text.into())).await.is_err() {
                        return;
                    }
                }
                Some(StreamEvent::End(StreamEnd::Revoked)) => break close_frame(CLOSE_REVOKED, "revoked"),
                Some(StreamEvent::End(StreamEnd::Lagged { after_seq })) => {
                    break close_frame(
                        CLOSE_LAGGED,
                        &format!("lagged: reconnect after seq {after_seq}"),
                    );
                }
                Some(StreamEvent::End(StreamEnd::Shutdown)) | None => {
                    break close_frame(CLOSE_GOING_AWAY, "the controller stops");
                }
            },
            received = socket.recv() => match received {
                None | Some(Err(_) | Ok(Message::Close(_))) => return,
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(Message::Text(_) | Message::Binary(_))) => {
                    break close_frame(CLOSE_READ_ONLY, "the stream is read-only");
                }
            },
            _ = ping.tick() => {
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                    return;
                }
            }
        }
    };
    if socket.send(closing).await.is_err() {
        return;
    }
    // Let the client read the close frame and answer it, so no reset cuts the frame short.
    let _ = tokio::time::timeout(CLOSE_GRACE, async {
        while let Some(Ok(message)) = socket.recv().await {
            if matches!(message, Message::Close(_)) {
                break;
            }
        }
    })
    .await;
}

async fn pair(host: Arc<dyn ApiHost>, peer: IpAddr, request: Request) -> Answer {
    pair_response(host, peer, request).await.into()
}

async fn pair_response(host: Arc<dyn ApiHost>, peer: IpAddr, request: Request) -> Response {
    let body = match read_body(request).await {
        Ok(b) => b,
        Err(refusal) => return *refusal,
    };
    let Ok(pair) = serde_json::from_slice::<PairRequest>(&body) else {
        return error(
            StatusCode::BAD_REQUEST,
            "the body is not {\"code\": ..., \"device_name\": ...}",
        );
    };
    // One answer for every bad code: a malformed one, another server's key, a used, expired or
    // replaced one. Nothing tells the caller which.
    let refused = || error(StatusCode::UNAUTHORIZED, "the code is not accepted");
    let Ok(code) = PairingCode::decode(&pair.code) else {
        return refused();
    };
    let own = {
        let host = host.clone();
        match tokio::task::spawn_blocking(move || host.status()).await {
            Ok(s) => s.key_spki_sha256,
            Err(_) => return failed_inside(),
        }
    };
    if hex(&code.spki_sha256) != own {
        return refused();
    }
    // The name is checked before the code is used up, so a bad name costs the operator no code.
    if let Err(why) = check_device_name(&pair.device_name) {
        return error(StatusCode::UNPROCESSABLE_ENTITY, why);
    }
    let secret = code.secret_hex();
    let name = pair.device_name;
    match tokio::task::spawn_blocking(move || host.redeem(&secret, &name, peer)).await {
        Ok(Ok(result)) => json(StatusCode::OK, &result),
        Ok(Err(Refusal::Code)) => refused(),
        Ok(Err(Refusal::Name(why))) => error(StatusCode::UNPROCESSABLE_ENTITY, why),
        Ok(Err(Refusal::Server(why))) => error(StatusCode::INTERNAL_SERVER_ERROR, why),
        Err(_) => failed_inside(),
    }
}

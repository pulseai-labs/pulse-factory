//! The router: the routes of version 1 over a host port (ADR-0012, `docs/contracts/api.md`).
//!
//! - `POST /v1/pair`, no token: a pairing code and a device name in, a device token out.
//! - `GET /v1/status`, token: the server's version and key, and the calling device.
//! - `POST /v1/verbs/<verb>`, token: one allowlisted verb, through the controller's verb handler.
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

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use http_body_util::{BodyExt, LengthLimitError, Limited};
use serde::Serialize;

use crate::code::{PairingCode, hex, token_sha256};
use crate::guard::{Blocked, Clock, Failure, Guard, GuardConfig, system_clock};
use crate::host::{ApiHost, Outcome, Refusal, RequestRecord, Route, is_allowed};
use crate::types::{ApiError, DeviceRecord, PairRequest, check_device_name};

/// The longest request body, in bytes.
pub const MAX_BODY_BYTES: usize = 1 << 20;

/// The text of every 401 on a token route. It is one text for every reason.
const NOT_AUTHORIZED: &str = "not authorized";

/// The address of the peer that opened the connection. The listener adds it to each request as an
/// extension. A request with no such extension counts as from `0.0.0.0`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeerAddr(pub IpAddr);

/// What every request needs: the host and the peer guard. Clones share the one guard.
#[derive(Clone)]
struct App {
    host: Arc<dyn ApiHost>,
    guard: Arc<Guard>,
}

/// The router of the API over a host, with the default numbers of the peer guard and the system
/// clock.
pub fn router(host: Arc<dyn ApiHost>) -> Router {
    router_with(host, GuardConfig::default(), system_clock())
}

/// The router of the API with its own guard numbers and clock. A test moves time with the clock.
pub fn router_with(host: Arc<dyn ApiHost>, config: GuardConfig, clock: Clock) -> Router {
    let app = App {
        host,
        guard: Arc::new(Guard::new(config, clock)),
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
}

impl From<Response> for Answer {
    fn from(response: Response) -> Answer {
        Answer {
            response,
            device: None,
            replayed: false,
        }
    }
}

/// The outcome the audit record names for a status.
fn outcome_of(status: StatusCode, replayed: bool) -> Outcome {
    match status {
        StatusCode::OK if replayed => Outcome::Replayed,
        StatusCode::OK => Outcome::Ok,
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
        },
    )
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

/// The route a path names, and the verb text when it names one. The method is not looked at.
fn classify(path: &str) -> (Route, Option<&str>) {
    match path {
        "/v1/pair" => (Route::Pair, None),
        "/v1/status" => (Route::Status, None),
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
            };
            audit(&app.host, record).await;
        }
        return blocked_answer(&block);
    }
    let host = app.host.clone();
    let answer: Answer = match route {
        Route::Pair if post => pair(host, peer, request).await,
        Route::Status if get => status(host, peer, request).await,
        Route::Verb if post => {
            verb_call(host, peer, verb.unwrap_or_default().to_string(), request).await
        }
        Route::Pair | Route::Status | Route::Verb => {
            error(StatusCode::METHOD_NOT_ALLOWED, "method not allowed").into()
        }
        Route::Other => error(StatusCode::NOT_FOUND, "not found").into(),
    };
    let code = answer.response.status();
    let failure = match route {
        Route::Pair if code == StatusCode::BAD_REQUEST || code == StatusCode::UNAUTHORIZED => {
            Some(Failure::Code)
        }
        Route::Status | Route::Verb if code == StatusCode::UNAUTHORIZED => Some(Failure::Token),
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
    };
    audit(&app.host, record).await;
    answer.response
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
            }
        }
        Ok(answer) => named(error(
            StatusCode::UNPROCESSABLE_ENTITY,
            answer
                .error
                .unwrap_or_else(|| "the controller refused".to_string()),
        )),
        Err(_) => named(failed_inside()),
    }
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

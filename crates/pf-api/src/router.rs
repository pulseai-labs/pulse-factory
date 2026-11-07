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
use crate::host::{ApiHost, Refusal, is_allowed};
use crate::types::{ApiError, DeviceRecord, PairRequest, check_device_name};

/// The longest request body, in bytes.
pub const MAX_BODY_BYTES: usize = 1 << 20;

/// The text of every 401 on a token route. It is one text for every reason.
const NOT_AUTHORIZED: &str = "not authorized";

/// The address of the peer that opened the connection. The listener adds it to each request as an
/// extension. A request with no such extension counts as from `0.0.0.0`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeerAddr(pub IpAddr);

/// The router of the API over a host.
pub fn router(host: Arc<dyn ApiHost>) -> Router {
    Router::new().fallback(handle).with_state(host)
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

async fn handle(State(host): State<Arc<dyn ApiHost>>, request: Request) -> Response {
    let peer = request
        .extensions()
        .get::<PeerAddr>()
        .map_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED), |p| p.0);
    let post = request.method() == axum::http::Method::POST;
    let get = request.method() == axum::http::Method::GET;
    let path = request.uri().path().to_string();
    match path.as_str() {
        "/v1/pair" if post => pair(host, peer, request).await,
        "/v1/status" if get => status(host, peer, request).await,
        "/v1/pair" | "/v1/status" => error(StatusCode::METHOD_NOT_ALLOWED, "method not allowed"),
        other => match other.strip_prefix("/v1/verbs/") {
            Some(verb) if !verb.is_empty() && !verb.contains('/') && post => {
                verb_call(host, peer, verb.to_string(), request).await
            }
            Some(verb) if !verb.is_empty() && !verb.contains('/') => {
                error(StatusCode::METHOD_NOT_ALLOWED, "method not allowed")
            }
            _ => error(StatusCode::NOT_FOUND, "not found"),
        },
    }
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

async fn status(host: Arc<dyn ApiHost>, peer: IpAddr, request: Request) -> Response {
    let device = match authenticate(&host, bearer_sha256(&request), peer).await {
        Ok(d) => d,
        Err(refusal) => return *refusal,
    };
    match tokio::task::spawn_blocking(move || host.status()).await {
        Ok(mut status) => {
            status.device = Some(device);
            json(StatusCode::OK, &status)
        }
        Err(_) => failed_inside(),
    }
}

async fn verb_call(
    host: Arc<dyn ApiHost>,
    peer: IpAddr,
    verb: String,
    request: Request,
) -> Response {
    let device = match authenticate(&host, bearer_sha256(&request), peer).await {
        Ok(d) => d,
        Err(refusal) => return *refusal,
    };
    if !is_allowed(&verb) {
        let shown: String = verb.chars().take(64).collect();
        return error(
            StatusCode::FORBIDDEN,
            format!("the verb {shown:?} is not open to devices"),
        );
    }
    let body = match read_body(request).await {
        Ok(b) => b,
        Err(refusal) => return *refusal,
    };
    let args = if body.iter().all(u8::is_ascii_whitespace) {
        serde_json::json!({})
    } else {
        match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(_) => return error(StatusCode::BAD_REQUEST, "the body is not JSON"),
        }
    };
    match tokio::task::spawn_blocking(move || host.call(&device, &verb, args)).await {
        Ok(answer) if answer.ok => json(StatusCode::OK, &answer.result),
        Ok(answer) => error(
            StatusCode::UNPROCESSABLE_ENTITY,
            answer
                .error
                .unwrap_or_else(|| "the controller refused".to_string()),
        ),
        Err(_) => failed_inside(),
    }
}

async fn pair(host: Arc<dyn ApiHost>, peer: IpAddr, request: Request) -> Response {
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

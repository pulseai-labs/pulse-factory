//! The errors of the client, and the map from an API status to a kind
//! (`docs/contracts/ui-transport.md`, "Error kinds").
//!
//! One type, [`ClientError`], is the failure of every call. Its `kind` serializes to the nine names
//! of the transport contract. Its message is short and never holds a secret: it is the server's own
//! `{"error": "<text>"}` cut to 300 characters, or `the server answered <status>`. A refused device
//! verb also carries `refusal`, the word of the closed list.

use serde::{Deserialize, Serialize};

/// The message of a `key_changed` error, word for word (the client rule in `docs/contracts/api.md`).
pub const KEY_CHANGED_MESSAGE: &str = "server key changed: re-pair";

/// The longest message, in characters.
pub const MESSAGE_MAX_CHARS: usize = 300;

/// The nine kinds of the transport contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// No HTTP answer: refused, reset, timed out, name not found, a TLS failure before the pin check.
    Unreachable,
    /// 401 on a route that needs a token, or a device that holds no token.
    Unauthorized,
    /// 401 on `POST /v1/pair`, or a code that does not decode.
    CodeRefused,
    /// The server key is not the pinned one. Nothing was sent.
    KeyChanged,
    /// 403.
    Forbidden,
    /// 422.
    Refused,
    /// 429.
    RateLimited,
    /// 400 or 413, or a request this client cannot form.
    BadRequest,
    /// 5xx, any other status, or an answer that is not the documented JSON.
    Server,
}

/// Which route an answer came from: the status 401 means a different thing on each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// `POST /v1/pair`.
    Pair,
    /// A route that needs the device token.
    Token,
}

/// The failure of one call. It serializes as `{"kind": …, "message": …}`, the shape of a Tauri
/// rejection and of the bridge's error. A refused device verb adds `refusal`, one word of
/// `pf_api::host::REFUSALS` (`r1.s4.w3`, W3); an error with no word keeps the two-field JSON.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientError {
    pub kind: ErrorKind,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
}

impl ClientError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> ClientError {
        ClientError {
            kind,
            message: message.into(),
            refusal: None,
        }
    }

    /// The error of a pin that does not match.
    pub fn key_changed() -> ClientError {
        ClientError::new(ErrorKind::KeyChanged, KEY_CHANGED_MESSAGE)
    }
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ClientError {}

/// Whether the text holds a run of 64 hex digits: the shape of a device token.
fn holds_token_shape(text: &str) -> bool {
    let mut run = 0usize;
    for c in text.chars() {
        if c.is_ascii_hexdigit() {
            run += 1;
            if run >= 64 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

/// A server text made safe: control characters become spaces, the text is trimmed and cut to 300
/// characters. `None` when nothing is left, or when it holds a secret this client knows or anything
/// in the shape of a token.
pub fn safe_text(raw: &str, secrets: &[&str]) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let text: String = cleaned.trim().chars().take(MESSAGE_MAX_CHARS).collect();
    if text.is_empty()
        || holds_token_shape(&text)
        || secrets.iter().any(|s| !s.is_empty() && text.contains(s))
    {
        return None;
    }
    Some(text)
}

/// The error of an API answer that is not a 200.
///
/// `secrets` are the strings this call holds (the token, the code): a server text that holds one is
/// replaced by `the server answered <status>`.
pub fn error_from_status(status: u16, route: Route, body: &[u8], secrets: &[&str]) -> ClientError {
    let kind = match (status, route) {
        (401, Route::Pair) => ErrorKind::CodeRefused,
        (401, Route::Token) => ErrorKind::Unauthorized,
        (403, _) => ErrorKind::Forbidden,
        (422, _) => ErrorKind::Refused,
        (429, _) => ErrorKind::RateLimited,
        (400 | 413, _) => ErrorKind::BadRequest,
        _ => ErrorKind::Server,
    };
    let parsed = serde_json::from_slice::<serde_json::Value>(body).ok();
    let raw = parsed
        .as_ref()
        .and_then(|v| v.get("error").and_then(|e| e.as_str().map(str::to_string)))
        .unwrap_or_default();
    let message =
        safe_text(&raw, secrets).unwrap_or_else(|| format!("the server answered {status}"));
    let mut error = ClientError::new(kind, message);
    // Only a 422 carries a refusal word, and only a word of the closed list is kept: any other
    // value is the server's text, not a word.
    if status == 422 {
        error.refusal = parsed
            .as_ref()
            .and_then(|v| v.get("refusal"))
            .and_then(|w| w.as_str())
            .filter(|w| pf_api::host::REFUSALS.contains(w))
            .map(str::to_string);
    }
    error
}

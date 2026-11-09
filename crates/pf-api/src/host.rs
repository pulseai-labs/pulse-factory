//! The host port: what the controller gives the router (ADR-0012).
//!
//! The router never reaches the store or the verbs itself. It calls this trait, and the controller
//! implements it. `call` is the controller's own verb handler, the code the Unix socket runs.

use std::net::IpAddr;

use tokio::sync::mpsc;

use crate::types::{ApiStatus, DeviceRecord, PairResult, ServerRef};

/// The verbs a device may call: exactly these. A later spine that adds a verb amends ADR-0012.
/// `halt` and every verb that acks, answers or claims stay off this list. So do `session_redact`
/// (the CLI only: a redaction is the operator's, C12) and `session_items` (a device reads the items
/// on the stream, from `after=0`).
pub const ALLOWLIST: &[&str] = &[
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
    // `r1.s4.w4`: the scoped settings (ADR-0016). A device acts as the operator.
    "settings_get",
    "settings_set",
    "settings_unset",
];

/// Whether a device may call this verb.
pub fn is_allowed(verb: &str) -> bool {
    ALLOWLIST.contains(&verb)
}

/// Why a pairing was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The code is used, expired, replaced or wrong. The wire answer is the same for each.
    Code,
    /// The device name breaks the rules. The code stays live.
    Name(String),
    /// The controller could not finish: the store failed, or no random source answered. The text
    /// holds no secret.
    Server(String),
}

/// Which route a request named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    Pair,
    Status,
    Verb,
    /// `GET /v1/stream`, the WebSocket stream (`r1.s3.w3`).
    Stream,
    /// Any other path, and a path that is no route.
    Other,
}

impl Route {
    pub fn as_str(self) -> &'static str {
        match self {
            Route::Pair => "pair",
            Route::Status => "status",
            Route::Verb => "verb",
            Route::Stream => "stream",
            Route::Other => "other",
        }
    }
}

/// How the router answered a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// 200.
    Ok,
    /// 200, and the answer says `"replayed": true`.
    Replayed,
    /// 101: a stream connection was upgraded. The close writes no record of its own.
    Upgraded,
    /// 401.
    Unauthorized,
    /// 403.
    Forbidden,
    /// 422.
    Refused,
    /// 400.
    BadRequest,
    /// 413.
    TooLarge,
    /// 404.
    NotFound,
    /// 405.
    MethodNotAllowed,
    /// 429.
    Blocked,
    /// 500.
    Failed,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Replayed => "replayed",
            Outcome::Upgraded => "upgraded",
            Outcome::Unauthorized => "unauthorized",
            Outcome::Forbidden => "forbidden",
            Outcome::Refused => "refused",
            Outcome::BadRequest => "bad_request",
            Outcome::TooLarge => "too_large",
            Outcome::NotFound => "not_found",
            Outcome::MethodNotAllowed => "method_not_allowed",
            Outcome::Blocked => "blocked",
            Outcome::Failed => "failed",
        }
    }
}

/// One request the router answered, for the host's request ledger. It never holds a token, a code,
/// a secret or a key. The device ID alone names the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestRecord {
    pub peer: IpAddr,
    pub route: Route,
    /// The verb as the path spells it, when the route names one. It is the caller's text: the host
    /// must not write it unless it knows it.
    pub verb: Option<String>,
    /// The device whose token matched, if one did.
    pub device: Option<String>,
    pub status: u16,
    pub outcome: Outcome,
    /// For `blocked`: the 429s this record counts, itself included. 1 for every other outcome.
    pub count: u64,
    /// Set on the record of the failure that starts a block: when the block ends, as UTC text.
    pub blocked_until: Option<String>,
}

/// Why the host ends a stream. The router turns each into a WebSocket close frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamEnd {
    /// The device was revoked: close 4001 `revoked`.
    Revoked,
    /// The connection fell behind the live frames: close 4002 `lagged: reconnect after seq <n>`.
    Lagged { after_seq: u64 },
    /// The controller stops: close 1001.
    Shutdown,
}

/// One thing a stream feed carries to the router.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamEvent {
    /// One frame, as the JSON text that goes out as a WebSocket text message.
    Frame(String),
    /// The stream ends. The router closes the socket with the matching code, and the feed ends.
    End(StreamEnd),
}

/// The feed the host gives a stream: frames and the end. A feed whose sender is dropped without an
/// [`StreamEvent::End`] closes the socket with 1001, as a controller stop does.
pub type StreamFeed = mpsc::Receiver<StreamEvent>;

/// Why the host will not open a stream. The router never reaches the store itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamRefusal {
    /// The session is unknown: 404 with this one-line text.
    NotFound(String),
    /// The call is no good: 400 with this one-line text.
    BadRequest(String),
    /// The device was revoked after the router authenticated the request: 401, as for any call
    /// with a revoked token.
    NotAuthorized,
}

pub trait ApiHost: Send + Sync + 'static {
    /// Use up a live pairing code and record a device. `code_secret` is the 20 hex digits of the
    /// code's secret. Err names the refusal.
    fn redeem(
        &self,
        code_secret: &str,
        device_name: &str,
        peer: IpAddr,
    ) -> Result<PairResult, Refusal>;

    /// The live device whose token has this SHA-256, after a revoke check. Records last-seen.
    fn device(&self, token_sha256: &str, peer: IpAddr) -> Option<DeviceRecord>;

    /// Run one allowlisted verb through the controller's handler, as the CLI would.
    fn call(&self, device: &DeviceRecord, verb: &str, args: serde_json::Value) -> pf_ipc::Response;

    /// The server's version and key. The `device` field is empty: the router fills it.
    fn status(&self) -> ApiStatus;

    /// The server's id and name, which every answer names in `pf-server-id` and `pf-server-name`.
    /// The default takes the id from [`ApiHost::status`] and the name `server`.
    fn server(&self) -> ServerRef {
        let id = self.status().key_spki_sha256;
        ServerRef {
            id: (!id.is_empty()).then_some(id),
            name: "server".to_string(),
        }
    }

    /// Open the stream of a device: the router calls this after the token, the upgrade headers and
    /// the query are checked and before it answers `101`. `session` is the followed session, and
    /// `after` is the last `seq` the device has. The host subscribes to its live frames first, then
    /// sends `hello`, the stored items after `after`, `caught`, and the live frames (`api.md`, "The
    /// stream"). The default refuses with 404, so a host that has no chat compiles unchanged.
    fn open_stream(
        &self,
        _device: &DeviceRecord,
        _session: Option<&str>,
        _after: u64,
    ) -> Result<StreamFeed, StreamRefusal> {
        Err(StreamRefusal::NotFound(
            "there is no stream here".to_string(),
        ))
    }

    /// Record one answered request. The router calls it once for every request it answers, after
    /// the answer is decided, except that a blocked peer's 429s are coalesced into one record a
    /// minute. A host that keeps no request ledger keeps the default.
    fn audit(&self, _record: &RequestRecord) {}
}

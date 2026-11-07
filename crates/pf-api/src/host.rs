//! The host port: what the controller gives the router (ADR-0012).
//!
//! The router never reaches the store or the verbs itself. It calls this trait, and the controller
//! implements it. `call` is the controller's own verb handler, the code the Unix socket runs.

use std::net::IpAddr;

use crate::types::{ApiStatus, DeviceRecord, PairResult};

/// The verbs a device may call: exactly these. A later spine that adds a verb amends ADR-0012.
/// `halt` and every verb that acks, answers or claims stay off this list.
pub const ALLOWLIST: &[&str] = &["project_add", "project_list", "project_remove"];

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
    /// Any other path, and a path that is no route.
    Other,
}

impl Route {
    pub fn as_str(self) -> &'static str {
        match self {
            Route::Pair => "pair",
            Route::Status => "status",
            Route::Verb => "verb",
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

    /// Record one answered request. The router calls it once for every request it answers, after
    /// the answer is decided, except that a blocked peer's 429s are coalesced into one record a
    /// minute. A host that keeps no request ledger keeps the default.
    fn audit(&self, _record: &RequestRecord) {}
}

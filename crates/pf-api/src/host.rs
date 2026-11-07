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
}

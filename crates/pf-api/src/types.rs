//! The wire types of the API and of the verbs that the controller shares with the CLI (ADR-0004,
//! ADR-0012). Each type derives `Serialize`, `Deserialize` and `ts_rs::TS`: the clients take their
//! types from the generated files in `apps/api-client/src/gen/`, and a test fails when those drift.
//!
//! The project shapes moved here from the controller with the same JSON. The controller imports them.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The `schema_version` of every answer (ADR-0004).
pub const SCHEMA_VERSION: u32 = 1;

// ------------------------------------------------------------------ projects

/// The arguments of `project_add`. `root` is an absolute path. The controller makes it canonical.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ProjectAddArgs {
    pub root: String,
    /// The default is the last component of the canonical root.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub name: Option<String>,
    pub command_id: String,
}

/// The arguments of `project_remove`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ProjectRemoveArgs {
    pub id: String,
    pub command_id: String,
}

/// One project, as every verb shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ProjectRecord {
    pub id: String,
    pub name: String,
    pub root: String,
    pub created_at: String,
    pub removed_at: Option<String>,
}

/// The answer of `project_add` and `project_remove`. `replayed` is true when the command ID was
/// admitted before with the same content: the project is the first record's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ProjectChanged {
    pub schema_version: u32,
    pub replayed: bool,
    pub project: ProjectRecord,
}

/// The answer of `project_list`: the live projects, in creation order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ProjectList {
    pub schema_version: u32,
    pub projects: Vec<ProjectRecord>,
}

// ------------------------------------------------------------------ devices and pairing

/// One device, as `device list` and the API show it. The record never holds the token or its hash.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct DeviceRecord {
    pub id: String,
    pub name: String,
    pub paired_at: String,
    pub last_seen_at: Option<String>,
    pub last_peer: Option<String>,
    pub revoked_at: Option<String>,
}

/// The body of `POST /v1/pair`: the pairing code and a name for the device (1 to 64 characters).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct PairRequest {
    pub code: String,
    pub device_name: String,
}

/// The answer of `POST /v1/pair`. The token is 64 hex digits, and it appears here once and nowhere
/// else. The server keeps only its SHA-256.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct PairResult {
    pub schema_version: u32,
    pub device: DeviceRecord,
    pub token: String,
}

/// The answer of `GET /v1/status`. `key_spki_sha256` is 64 hex digits. `device` is the calling
/// device. A host fills the first three fields and the router fills the device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ApiStatus {
    pub schema_version: u32,
    pub version: String,
    pub key_spki_sha256: String,
    pub device: Option<DeviceRecord>,
}

/// The body of every error answer of the API. It never holds a token, a code or a key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ApiError {
    pub error: String,
}

/// The arguments of `device_revoke`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct DeviceRevokeArgs {
    pub id: String,
    pub command_id: String,
}

/// The answer of `device_revoke`. `replayed` is true when the command ID was admitted before with
/// the same content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct DeviceRevoked {
    pub schema_version: u32,
    pub replayed: bool,
    pub device: DeviceRecord,
}

/// The answer of `device_list`: live devices first, each group in pairing order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct DeviceList {
    pub schema_version: u32,
    pub devices: Vec<DeviceRecord>,
}

/// The answer of `device_pair`: one copyable code and the time it stops working.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct DevicePairing {
    pub schema_version: u32,
    pub code: String,
    pub expires_at: String,
}

// ------------------------------------------------------------------ the listener's state

/// Where the listener stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(rename_all = "lowercase")]
pub enum ListenState {
    /// No address is set. No key was made.
    Off,
    /// The listener is bound and serves.
    Listening,
    /// The address is not on this host yet (Tailscale may start after the controller). The
    /// controller tries again every 5 seconds.
    Waiting,
    /// The bind failed for another reason, or the key could not be made. The controller does not
    /// try again, and it keeps serving the socket.
    Failed,
}

/// The answer of `api_status`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ApiState {
    pub schema_version: u32,
    pub state: ListenState,
    pub listen: Option<String>,
    pub key_spki_sha256: Option<String>,
    /// Why the state is `failed`: one line that names no token, code or key.
    pub error: Option<String>,
}

/// Write the TypeScript file of every wire type, with its dependencies, as `cfg` says. The
/// generated files in `apps/api-client/src/gen/` are exactly this output.
pub fn export_all(cfg: &ts_rs::Config) -> Result<(), ts_rs::ExportError> {
    ProjectAddArgs::export_all(cfg)?;
    ProjectRemoveArgs::export_all(cfg)?;
    ProjectRecord::export_all(cfg)?;
    ProjectChanged::export_all(cfg)?;
    ProjectList::export_all(cfg)?;
    DeviceRecord::export_all(cfg)?;
    PairRequest::export_all(cfg)?;
    PairResult::export_all(cfg)?;
    ApiStatus::export_all(cfg)?;
    ApiError::export_all(cfg)?;
    DeviceRevokeArgs::export_all(cfg)?;
    DeviceRevoked::export_all(cfg)?;
    DeviceList::export_all(cfg)?;
    DevicePairing::export_all(cfg)?;
    ListenState::export_all(cfg)?;
    ApiState::export_all(cfg)?;
    Ok(())
}

/// The rules for a device name: 1 to 64 characters, with no control character and no line break.
pub fn check_device_name(name: &str) -> Result<(), String> {
    let n = name.chars().count();
    if n == 0 {
        return Err("the device name is empty".to_string());
    }
    if n > DEVICE_NAME_MAX_CHARS {
        return Err(format!(
            "the device name has {n} characters, and the limit is {DEVICE_NAME_MAX_CHARS}"
        ));
    }
    if name
        .chars()
        .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
    {
        return Err("the device name holds a control character or a line break".to_string());
    }
    Ok(())
}

/// A device name is 1 to 64 characters.
pub const DEVICE_NAME_MAX_CHARS: usize = 64;

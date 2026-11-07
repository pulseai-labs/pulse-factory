//! The network API inside the controller (ADR-0001, ADR-0012, `docs/contracts/api.md`).
//!
//! The API is off until the operator sets an address (`--api-listen` or `PF_API_LISTEN`). With an
//! address, the controller makes or loads its TLS key at start-up, and a task on the controller's
//! runtime binds the address and serves the router of `pf-api`. A bind that fails with "address not
//! available" is tried again every few seconds, so the controller can start before Tailscale does.
//! Any other bind error is printed, and the controller keeps serving the socket.
//!
//! `ApiBridge` is the host port of `pf-api`: it gives the router the pairing, the token check and
//! the verb handler that the socket uses.

use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use pf_api::types::{ApiState, ApiStatus, DeviceRecord, ListenState, PairResult, SCHEMA_VERSION};
use pf_api::{ApiHost, Refusal, tls};
use pf_ipc::{Handler, Request, Response};
use serde_json::Value;

use super::{Controller, lock};

/// Where the listener stands, inside the controller.
enum Standing {
    Off,
    Waiting,
    Listening(SocketAddr),
    Failed(String),
}

/// The one live pairing code: its secret and when it stops working. The code text is never kept.
pub(super) struct LiveCode {
    pub(super) secret: [u8; pf_api::code::SECRET_BYTES],
    pub(super) expires: Instant,
}

/// The API's state in the controller.
pub(super) struct ApiRuntime {
    /// The address from the settings. `None` when the API is off, or when the address is not one.
    pub(super) listen: Option<SocketAddr>,
    /// The server key, made or loaded at start-up when the API is on.
    pub(super) key: Option<tls::ServerKey>,
    standing: Mutex<Standing>,
    /// At most one code is live: a new code replaces it, and a restart voids it.
    pub(super) pairing: Mutex<Option<LiveCode>>,
}

impl ApiRuntime {
    /// Read the setting and, when the API is on, make or load the key. A setting that is no
    /// address, or a key that cannot be made, leaves the state `failed` with the reason. The
    /// controller still starts.
    pub(super) fn start(state_dir: &Path, setting: Option<&str>) -> ApiRuntime {
        let off = |standing: Standing, listen: Option<SocketAddr>| ApiRuntime {
            listen,
            key: None,
            standing: Mutex::new(standing),
            pairing: Mutex::new(None),
        };
        let Some(text) = setting.map(str::trim).filter(|t| !t.is_empty()) else {
            return off(Standing::Off, None);
        };
        let listen: SocketAddr = match text.parse() {
            Ok(a) => a,
            Err(e) => {
                let why = format!(
                    "the API address {text:?} is not an address and a port, such as 100.90.203.21:7443 ({e})"
                );
                eprintln!("pulse-factory: controller: {why}");
                return off(Standing::Failed(why), None);
            }
        };
        match tls::load_or_create(&tls::api_dir(state_dir), &listen) {
            Ok(key) => ApiRuntime {
                listen: Some(listen),
                key: Some(key),
                standing: Mutex::new(Standing::Waiting),
                pairing: Mutex::new(None),
            },
            Err(why) => {
                eprintln!("pulse-factory: controller: the API has no key: {why}");
                off(Standing::Failed(why), Some(listen))
            }
        }
    }

    fn set(&self, standing: Standing) {
        *lock(&self.standing) = standing;
    }

    /// The address a pairing code names: the bound one while listening, else the setting.
    pub(super) fn advertised(&self) -> Option<SocketAddr> {
        match &*lock(&self.standing) {
            Standing::Listening(addr) => Some(*addr),
            _ => self.listen,
        }
    }

    /// The state for `api_status`.
    pub(super) fn state(&self) -> ApiState {
        let (state, listen, error) = match &*lock(&self.standing) {
            Standing::Off => (ListenState::Off, None, None),
            Standing::Waiting => (ListenState::Waiting, self.listen, None),
            Standing::Listening(addr) => (ListenState::Listening, Some(*addr), None),
            Standing::Failed(why) => (ListenState::Failed, self.listen, Some(why.clone())),
        };
        ApiState {
            schema_version: SCHEMA_VERSION,
            state,
            listen: listen.map(|a| a.to_string()),
            key_spki_sha256: self.key.as_ref().map(tls::ServerKey::spki_sha256_hex),
            error,
        }
    }
}

/// How long a bind that is not possible yet waits before it tries again.
pub const DEFAULT_RETRY: std::time::Duration = std::time::Duration::from_secs(5);

/// Start the listener on the current Tokio runtime. It returns `None` when the API is off or has no
/// key. The task binds (and tries again while the address is not on this host), then serves until
/// the runtime ends.
pub fn spawn_api(controller: &Arc<Controller>) -> Option<tokio::task::JoinHandle<()>> {
    let (listen, key) = (controller.api.listen?, controller.api.key.as_ref()?);
    let config = match key.server_config() {
        Ok(c) => c,
        Err(why) => {
            eprintln!("pulse-factory: controller: the API has no usable key: {why}");
            controller.api.set(Standing::Failed(why));
            return None;
        }
    };
    let controller = controller.clone();
    Some(tokio::spawn(async move {
        let listener = loop {
            match tokio::net::TcpListener::bind(listen).await {
                Ok(l) => break l,
                Err(e) if e.kind() == std::io::ErrorKind::AddrNotAvailable => {
                    controller.api.set(Standing::Waiting);
                    tokio::time::sleep(controller.cfg.api_retry).await;
                }
                Err(e) => {
                    let why = format!("the API cannot listen on {listen}: {e}");
                    eprintln!("pulse-factory: controller: {why}");
                    controller.api.set(Standing::Failed(why));
                    return;
                }
            }
        };
        let bound = listener.local_addr().unwrap_or(listen);
        controller.api.set(Standing::Listening(bound));
        let host: Arc<dyn ApiHost> = Arc::new(ApiBridge {
            controller: controller.clone(),
        });
        pf_api::listener::serve(
            listener,
            config,
            pf_api::router(host),
            std::future::pending(),
        )
        .await;
    }))
}

/// The host port of `pf-api` over the controller.
pub struct ApiBridge {
    controller: Arc<Controller>,
}

impl ApiBridge {
    pub fn new(controller: Arc<Controller>) -> ApiBridge {
        ApiBridge { controller }
    }
}

impl ApiHost for ApiBridge {
    fn redeem(
        &self,
        code_secret: &str,
        device_name: &str,
        peer: IpAddr,
    ) -> Result<PairResult, Refusal> {
        self.controller.redeem_code(code_secret, device_name, peer)
    }

    fn device(&self, token_sha256: &str, peer: IpAddr) -> Option<DeviceRecord> {
        self.controller.device_for_token(token_sha256, peer)
    }

    fn call(&self, device: &DeviceRecord, verb: &str, args: Value) -> Response {
        // The router checked the allowlist. The bridge checks it again, so no other path through
        // here reaches a verb that the list does not name.
        if !pf_api::host::is_allowed(verb) {
            return Response::err(format!("the verb {verb:?} is not open to devices"));
        }
        // A device acts as the operator (ADR-0009), through the code the socket runs.
        let _ = device;
        self.controller.handle(Request::new(verb, args))
    }

    fn status(&self) -> ApiStatus {
        ApiStatus {
            schema_version: SCHEMA_VERSION,
            version: env!("CARGO_PKG_VERSION").to_string(),
            key_spki_sha256: self
                .controller
                .api
                .key
                .as_ref()
                .map(tls::ServerKey::spki_sha256_hex)
                .unwrap_or_default(),
            device: None,
        }
    }
}

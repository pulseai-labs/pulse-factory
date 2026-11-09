//! The network API inside the controller (ADR-0001, ADR-0012, `docs/contracts/api.md`).
//!
//! The API is off until the operator sets an address (`--api-listen` or `PF_API_LISTEN`). With an
//! address, the controller makes or loads its TLS key at start-up, and a task on the controller's
//! runtime binds the address and serves the router of `pf-api`. A bind that fails with "address not
//! available" is tried again every few seconds, so the controller can start before Tailscale does.
//! Any other bind error is printed, and the controller keeps serving the socket.
//!
//! The controller refuses an address outside loopback and the tailnet (`pf_api::guard::listen_allowed`)
//! before it makes a key, so the API is never on a LAN or a public address.
//!
//! `ApiBridge` is the host port of `pf-api`: it gives the router the pairing, the token check and
//! the verb handler that the socket uses. It also writes the request ledger, `api.ledger`: one line
//! for each API request that the router reports through `audit`.

use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use pf_api::guard::listen_allowed;
use pf_api::host::{StreamFeed, StreamRefusal};
use pf_api::types::{
    ApiState, ApiStatus, DeviceRecord, ListenState, PairResult, SCHEMA_VERSION, ServerRef,
};
use pf_api::{ApiHost, Outcome, Refusal, RequestRecord, tls};
use pf_ipc::{Handler, Request, Response};
use serde_json::Value;

use super::commands::{field, now};
use super::{Controller, lock};

/// The request ledger's file name in the state directory.
pub const REQUEST_LEDGER_FILE: &str = "api.ledger";

/// The verbs of the controller's own dispatch table (`Controller::handle`). The request ledger names
/// a verb only when it is one of these, so a token, a code or a line break sent as a verb name never
/// reaches the file. A verb added to the table is added here.
pub const VERBS: &[&str] = &[
    "runs",
    "arm",
    "hook",
    "status",
    "halt",
    "requests",
    "show",
    "project_add",
    "project_list",
    "project_remove",
    "session_start",
    "session_send",
    "session_answer",
    "session_interrupt",
    "session_set",
    "session_end",
    "session_redact",
    "session_list",
    "session_items",
    "device_list",
    "api_status",
    "claim",
    "answer",
    "clear",
    "ack",
    "device_pair",
    "device_revoke",
];

/// The text of the failure for an address outside the allowed ranges.
fn out_of_range(ip: IpAddr) -> String {
    format!(
        "the API address {ip} is outside loopback and the tailnet (127.0.0.0/8, ::1, 100.64.0.0/10, \
         fd7a:115c:a1e0::/48)"
    )
}

/// The request ledger and its one writer. The file is append-only, with no `fsync` per line, and it
/// is not in the store: a crash can lose the last lines (ADR-0012).
pub(super) struct RequestLedger {
    path: PathBuf,
    /// One writer at a time, so two lines never mix.
    writer: Mutex<()>,
}

impl RequestLedger {
    fn new(path: PathBuf) -> RequestLedger {
        RequestLedger {
            path,
            writer: Mutex::new(()),
        }
    }

    /// Append one line. A failure prints one line on stderr and costs the request nothing.
    fn append(&self, line: &str) {
        let _one = lock(&self.writer);
        let result = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.path)
            .and_then(|mut file| file.write_all(format!("{line}\n").as_bytes()));
        if let Err(e) = result {
            eprintln!(
                "pulse-factory: controller: the request ledger {} could not be appended to: {e}",
                self.path.display()
            );
        }
    }
}

/// One line of the request ledger: `<time> api route=<route> verb=<verb|-> device=<D-n|-> peer=<ip>
/// status=<code> outcome=<outcome>`. A verb that is not in the dispatch table is `verb=?` with its
/// length. A blocked record adds `count=<n>`, and the record of the failure that starts a block adds
/// `blocked_until=<time>`. Every value goes through `field()`.
pub(super) fn request_line(at: &str, record: &RequestRecord) -> String {
    let verb = match record.verb.as_deref() {
        None => "-".to_string(),
        Some(v) if VERBS.contains(&v) => field(v),
        Some(v) => format!("? verb_len={}", v.chars().count()),
    };
    let mut line = format!(
        "{at} api route={} verb={verb} device={} peer={} status={} outcome={}",
        record.route.as_str(),
        record
            .device
            .as_deref()
            .map_or_else(|| "-".to_string(), field),
        field(&record.peer.to_string()),
        record.status,
        record.outcome.as_str(),
    );
    if record.outcome == Outcome::Blocked {
        line.push_str(&format!(" count={}", record.count));
    }
    if let Some(until) = &record.blocked_until {
        line.push_str(&format!(" blocked_until={}", field(until)));
    }
    line
}

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
    /// The request ledger, `<state dir>/api.ledger`.
    requests: RequestLedger,
}

impl ApiRuntime {
    /// Read the setting and, when the API is on, make or load the key. A setting that is no
    /// address, or a key that cannot be made, leaves the state `failed` with the reason. The
    /// controller still starts.
    pub(super) fn start(state_dir: &Path, setting: Option<&str>) -> ApiRuntime {
        let requests = || RequestLedger::new(state_dir.join(REQUEST_LEDGER_FILE));
        let off = |standing: Standing, listen: Option<SocketAddr>| ApiRuntime {
            listen,
            key: None,
            standing: Mutex::new(standing),
            pairing: Mutex::new(None),
            requests: requests(),
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
        // The address is checked before a key is made: an address outside loopback and the tailnet
        // gets no key and no listener, and the controller keeps serving the socket.
        if !listen_allowed(listen.ip()) {
            let why = out_of_range(listen.ip());
            eprintln!("pulse-factory: controller: {why}");
            return off(Standing::Failed(why), Some(listen));
        }
        match tls::load_or_create(&tls::api_dir(state_dir), &listen) {
            Ok(key) => ApiRuntime {
                listen: Some(listen),
                key: Some(key),
                standing: Mutex::new(Standing::Waiting),
                pairing: Mutex::new(None),
                requests: requests(),
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

    fn audit(&self, record: &RequestRecord) {
        self.controller
            .api
            .requests
            .append(&request_line(&now(), record));
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
            server: Some(self.controller.server_ref()),
        }
    }

    fn server(&self) -> ServerRef {
        self.controller.server_ref()
    }

    fn open_stream(
        &self,
        device: &DeviceRecord,
        session: Option<&str>,
        after: u64,
    ) -> Result<StreamFeed, StreamRefusal> {
        self.controller.open_stream(device, session, after)
    }
}

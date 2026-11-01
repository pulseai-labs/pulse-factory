//! `pf-ipc`: the protocol between the CLI and the controller (ADR-0001, `docs/contracts/cli.md`).
//!
//! One request and one answer per connection, each one line of JSON, at most 1 MiB. Each message
//! carries `protocol_version`, and a different version is refused with a message that names both.
//! The socket is `<state dir>/controller.sock`, mode 0600, in a state directory of mode 0700. Only
//! the operator's OS user can open it.
//!
//! The crate knows no verb. The controller gives the server a [`Handler`], and a client calls a
//! verb by name with JSON arguments.

use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener as StdListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

/// The only protocol version this crate speaks.
pub const PROTOCOL_VERSION: u32 = 1;

/// The longest message, in bytes, in either direction. The newline is not counted.
pub const MAX_MESSAGE_BYTES: usize = 1 << 20;

/// The time a server waits for a client to send its line.
const READ_TIMEOUT: Duration = Duration::from_secs(15);

/// The time a client waits for the answer. An `arm` pins files, so it can take a while.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(300);

/// What a client asks for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub protocol_version: u32,
    pub verb: String,
    #[serde(default)]
    pub args: Value,
}

impl Request {
    pub fn new(verb: &str, args: Value) -> Request {
        Request {
            protocol_version: PROTOCOL_VERSION,
            verb: verb.to_string(),
            args,
        }
    }
}

/// What the controller answers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub protocol_version: u32,
    pub ok: bool,
    #[serde(default)]
    pub result: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    pub fn ok(result: Value) -> Response {
        Response {
            protocol_version: PROTOCOL_VERSION,
            ok: true,
            result,
            error: None,
        }
    }

    pub fn err(why: impl Into<String>) -> Response {
        Response {
            protocol_version: PROTOCOL_VERSION,
            ok: false,
            result: Value::Null,
            error: Some(why.into()),
        }
    }
}

/// The server side of a verb. A handler may block: the server calls it off the async threads.
pub trait Handler: Send + Sync + 'static {
    fn handle(&self, request: Request) -> Response;
}

// ------------------------------------------------------------------ the state directory

/// The state directory for given values of `$PF_STATE_DIR` and `$HOME`. An empty value counts as
/// unset.
pub fn state_dir_from(pf_state_dir: Option<OsString>, home: Option<OsString>) -> PathBuf {
    if let Some(dir) = pf_state_dir.filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    let home = home
        .filter(|h| !h.is_empty())
        .map_or_else(|| PathBuf::from("."), PathBuf::from);
    home.join(".local/state/pulse-factory")
}

/// The state directory of this process: `$PF_STATE_DIR`, else `~/.local/state/pulse-factory`.
pub fn state_dir() -> PathBuf {
    state_dir_from(std::env::var_os("PF_STATE_DIR"), std::env::var_os("HOME"))
}

/// The socket of a state directory.
pub fn socket_path(state_dir: &Path) -> PathBuf {
    state_dir.join("controller.sock")
}

/// Make the state directory, with mode 0700, and the parents it needs. An existing directory is
/// set to 0700 too.
pub fn prepare_state_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

// ------------------------------------------------------------------ binding

/// Why the socket cannot be bound.
#[derive(Debug)]
pub enum BindError {
    /// Another controller holds the state directory.
    AlreadyRunning(PathBuf),
    Io(String),
}

impl std::fmt::Display for BindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BindError::AlreadyRunning(socket) => write!(
                f,
                "another controller is already running on {}",
                socket.display()
            ),
            BindError::Io(why) => write!(f, "cannot bind the controller socket: {why}"),
        }
    }
}

impl std::error::Error for BindError {}

/// A bound socket. It holds the controller's lock on the state directory while it lives.
#[derive(Debug)]
pub struct Bound {
    listener: StdListener,
    _lock: File,
}

impl Bound {
    /// The listener, for a caller that serves the socket itself. The lock goes with it, so a
    /// second controller still refuses while the listener lives.
    pub fn into_std_listener(self) -> StdListener {
        // The lock file stays open as long as the process holds this value. Leak it on purpose:
        // the caller owns the listener and has nowhere to keep the lock.
        std::mem::forget(self._lock);
        self.listener
    }
}

/// Bind the controller socket of `dir`.
///
/// The state directory is made with mode 0700. One controller holds `controller.lock` there, so a
/// second one refuses. A socket file that no one answers is stale, and it is removed. The socket
/// gets mode 0600.
pub fn bind(dir: &Path) -> Result<Bound, BindError> {
    let io = |what: &str, e: std::io::Error| BindError::Io(format!("{what}: {e}"));
    prepare_state_dir(dir).map_err(|e| io(&format!("state directory {}", dir.display()), e))?;
    let socket = socket_path(dir);
    let lock = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("controller.lock"))
        .map_err(|e| io("lock file", e))?;
    if lock.try_lock().is_err() {
        return Err(BindError::AlreadyRunning(socket));
    }
    if socket.exists() {
        // The lock is ours, so no controller of this version serves the socket. An older one
        // might: if something answers, leave it alone.
        if UnixStream::connect(&socket).is_ok() {
            return Err(BindError::AlreadyRunning(socket));
        }
        std::fs::remove_file(&socket).map_err(|e| io("stale socket", e))?;
    }
    let listener = StdListener::bind(&socket).map_err(|e| io("socket", e))?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| io("socket mode", e))?;
    Ok(Bound {
        listener,
        _lock: lock,
    })
}

// ------------------------------------------------------------------ the server

/// Read one line of at most [`MAX_MESSAGE_BYTES`] bytes.
async fn read_line<R: AsyncReadExt + Unpin>(reader: &mut R) -> Result<Vec<u8>, String> {
    let mut reader = BufReader::new(reader.take(MAX_MESSAGE_BYTES as u64 + 2));
    let mut line = Vec::new();
    let read = tokio::time::timeout(READ_TIMEOUT, reader.read_until(b'\n', &mut line)).await;
    match read {
        Err(_) => Err("no complete line arrived in time".to_string()),
        Ok(Err(e)) => Err(format!("cannot read the message: {e}")),
        Ok(Ok(_)) => {
            let ended = line.last() == Some(&b'\n');
            if ended {
                line.pop();
            }
            if line.len() > MAX_MESSAGE_BYTES {
                Err("the message is over 1 MiB".to_string())
            } else if !ended && line.is_empty() {
                Err("the connection sent nothing".to_string())
            } else {
                Ok(line)
            }
        }
    }
}

/// Turn one line into a request, or into the answer that refuses it.
fn parse_request(line: &[u8]) -> Result<Request, Response> {
    let value: Value = serde_json::from_slice(line)
        .map_err(|e| Response::err(format!("the message is not JSON: {e}")))?;
    let version = value
        .as_object()
        .and_then(|o| o.get("protocol_version"))
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            Response::err("the message is not a JSON object with a numeric protocol_version")
        })?;
    if version != u64::from(PROTOCOL_VERSION) {
        return Err(Response::err(format!(
            "protocol version {version} is not supported; this controller speaks version {PROTOCOL_VERSION}"
        )));
    }
    serde_json::from_value(value)
        .map_err(|e| Response::err(format!("the message is not a request: {e}")))
}

async fn write_answer<W: AsyncWriteExt + Unpin>(stream: &mut W, answer: &Response) {
    let mut bytes = serde_json::to_vec(answer).unwrap_or_default();
    if bytes.len() > MAX_MESSAGE_BYTES {
        bytes = serde_json::to_vec(&Response::err("the answer is over 1 MiB")).unwrap_or_default();
    }
    bytes.push(b'\n');
    let _ = stream.write_all(&bytes).await;
    let _ = stream.shutdown().await;
}

async fn serve_one(stream: tokio::net::UnixStream, handler: Arc<dyn Handler>) {
    let (mut read, mut write) = stream.into_split();
    let line = read_line(&mut read).await;
    let answer = match line {
        Err(why) => Response::err(why),
        Ok(line) => match parse_request(&line) {
            Err(refusal) => refusal,
            Ok(request) => {
                let verb = request.verb.clone();
                tokio::task::spawn_blocking(move || handler.handle(request))
                    .await
                    .unwrap_or_else(|_| {
                        Response::err(format!("the verb {verb} failed inside the controller"))
                    })
            }
        },
    };
    write_answer(&mut write, &answer).await;
    // Closing a socket with unread input resets the connection, and the client could lose the
    // answer. Read what is left for a short time before the socket closes.
    let _ = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::io::copy(&mut read, &mut tokio::io::sink()),
    )
    .await;
}

/// Serve the socket until `shutdown` completes. Each connection gets its own task, so a slow verb
/// never blocks another.
pub async fn serve(
    bound: Bound,
    handler: Arc<dyn Handler>,
    shutdown: impl std::future::Future<Output = ()>,
) {
    if bound.listener.set_nonblocking(true).is_err() {
        return;
    }
    let Bound { listener, _lock } = bound;
    let Ok(listener) = UnixListener::from_std(listener) else {
        return;
    };
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            () = &mut shutdown => break,
            accepted = listener.accept() => {
                if let Ok((stream, _)) = accepted {
                    tokio::spawn(serve_one(stream, Arc::clone(&handler)));
                }
            }
        }
    }
}

// ------------------------------------------------------------------ the client

/// Why a call failed.
#[derive(Debug)]
pub enum ClientError {
    /// Nothing answers on the socket.
    NoController { socket: PathBuf, why: String },
    /// The request is over 1 MiB.
    TooBig,
    /// The connection broke, or the answer is not one line of JSON.
    Io(String),
    /// The controller speaks another protocol version.
    Version { ours: u32, theirs: u64 },
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::NoController { socket, why } => write!(
                f,
                "cannot reach the controller at {}: {why}. Check `systemctl --user status pulse-factory`.",
                socket.display()
            ),
            ClientError::TooBig => write!(f, "the request is over 1 MiB"),
            ClientError::Io(why) => write!(f, "{why}"),
            ClientError::Version { ours, theirs } => write!(
                f,
                "protocol version mismatch: this client speaks version {ours}, and the controller answered with version {theirs}"
            ),
        }
    }
}

impl std::error::Error for ClientError {}

/// Call one verb and read the answer. This blocks.
pub fn call(state_dir: &Path, verb: &str, args: Value) -> Result<Response, ClientError> {
    let socket = socket_path(state_dir);
    let mut stream = UnixStream::connect(&socket).map_err(|e| ClientError::NoController {
        socket: socket.clone(),
        why: e.to_string(),
    })?;
    stream
        .set_read_timeout(Some(ANSWER_TIMEOUT))
        .map_err(|e| ClientError::Io(e.to_string()))?;
    let mut bytes = serde_json::to_vec(&Request::new(verb, args))
        .map_err(|e| ClientError::Io(e.to_string()))?;
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(ClientError::TooBig);
    }
    bytes.push(b'\n');
    stream
        .write_all(&bytes)
        .map_err(|e| ClientError::Io(format!("cannot send the request: {e}")))?;
    let mut answer = Vec::new();
    stream
        .take(MAX_MESSAGE_BYTES as u64 + 2)
        .read_to_end(&mut answer)
        .map_err(|e| ClientError::Io(format!("cannot read the answer: {e}")))?;
    while matches!(answer.last(), Some(b'\n' | b'\r')) {
        answer.pop();
    }
    let value: Value = serde_json::from_slice(&answer)
        .map_err(|e| ClientError::Io(format!("the answer is not JSON: {e}")))?;
    let theirs = value
        .get("protocol_version")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if theirs != u64::from(PROTOCOL_VERSION) {
        return Err(ClientError::Version {
            ours: PROTOCOL_VERSION,
            theirs,
        });
    }
    serde_json::from_value(value)
        .map_err(|e| ClientError::Io(format!("the answer is not a response: {e}")))
}

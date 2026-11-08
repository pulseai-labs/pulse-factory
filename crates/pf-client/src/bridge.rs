//! The bridge: the bridge protocol of `docs/contracts/ui-transport.md`, served on `127.0.0.1`.
//!
//! This is the `desktop-bridge` fake (`SPINE.md`, Fakes) with the real client behind it: it stands in
//! for the Tauri shell's Rust side where the Mac app cannot run, such as headless Chromium on
//! draco-desk. It is for `scripts/demo/ui-pair.sh` (d12) and tests only. The Mac app does not use it.
//!
//! `serve(ui_dir, state_dir)` binds `127.0.0.1` on a free port and serves the bundle in `ui_dir` and
//! the routes of the protocol. The device is in a [`FileStore`] at `<state_dir>/device.json` (mode
//! 0600). The bridge writes nothing to stdout or stderr: the binary prints the one `listening` line.
//!
//! Who may call it: a request whose `Host` is not the bridge's own `127.0.0.1:<port>` or whose
//! `Origin` is not its own origin gets 403, and so does a `POST` to a bridge route that is not
//! `application/json`. A page on another origin cannot call the bridge, and a DNS rebinding cannot.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::Response;
use pf_api::types::ProjectAddArgs;
use serde_json::{Value, json};

use crate::error::{ClientError, ErrorKind};
use crate::store::FileStore;
use crate::transport::Transport;

/// The largest request body: 1 MiB and a little for the envelope.
const MAX_BODY_BYTES: usize = 1024 * 1024 + 4096;

/// The content security policy of every answer: the bundle's own origin only.
const CSP: &str = "default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'";

const BRIDGE_PREFIX: &str = "/bridge/";
const VERBS_PREFIX: &str = "/bridge/verbs/";

struct Shared {
    ui_root: PathBuf,
    own_host: String,
    transport: Transport<FileStore>,
}

/// A running bridge.
pub struct Bridge {
    addr: SocketAddr,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl Bridge {
    /// The address the bridge listens on.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// `http://127.0.0.1:<port>`.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Wait until the bridge ends. It ends when `shutdown` is called or the server fails.
    pub async fn wait(mut self) {
        let _ = (&mut self.task).await;
    }

    /// Stop the bridge and wait for its connections to close.
    pub async fn shutdown(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let _ = (&mut self.task).await;
    }
}

/// Serve `ui_dir` and the bridge protocol on `127.0.0.1` on a free port. The device file is
/// `<state_dir>/device.json`.
pub async fn serve(ui_dir: PathBuf, state_dir: PathBuf) -> Result<Bridge, String> {
    // File work stays off the async thread, here as in every request.
    let ui_root = tokio::task::spawn_blocking(move || {
        let ui_root = std::fs::canonicalize(&ui_dir)
            .map_err(|_| format!("the --ui directory {} cannot be read", ui_dir.display()))?;
        if !ui_root.join("index.html").is_file() {
            return Err("the --ui directory has no index.html".to_string());
        }
        Ok(ui_root)
    })
    .await
    .map_err(|_| "the --ui directory check did not finish".to_string())??;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| format!("cannot listen on 127.0.0.1: {e}"))?;
    let addr = listener
        .local_addr()
        .map_err(|e| format!("cannot read the listen address: {e}"))?;
    let shared = Arc::new(Shared {
        ui_root,
        own_host: format!("127.0.0.1:{}", addr.port()),
        transport: Transport::new(Arc::new(FileStore::new(state_dir.join("device.json")))),
    });
    let app = Router::new().fallback(handle).with_state(shared);
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = stopped.await;
            })
            .await;
    });
    Ok(Bridge {
        addr,
        stop: Some(stop),
        task,
    })
}

// ---- answers --------------------------------------------------------------------------------------

fn send(status: StatusCode, content_type: &str, body: impl Into<Body>) -> Response {
    let mut response = Response::new(body.into());
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(content_type)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    response
}

fn text(status: StatusCode, body: &'static str) -> Response {
    send(status, "text/plain", body)
}

/// A bridge answer: always HTTP 200, with the envelope.
fn envelope(answer: Result<Value, ClientError>) -> Response {
    let value = match answer {
        Ok(result) => json!({ "ok": true, "result": result }),
        Err(error) => {
            json!({ "ok": false, "error": { "kind": error.kind, "message": error.message } })
        }
    };
    send(StatusCode::OK, "application/json", value.to_string())
}

fn bad(message: &str) -> ClientError {
    ClientError::new(ErrorKind::BadRequest, message)
}

// ---- the request ----------------------------------------------------------------------------------

async fn handle(State(shared): State<Arc<Shared>>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    // Only the bundle's own page may call the bridge: no foreign Host (DNS rebinding), no foreign
    // Origin, and a JSON content type on every POST (a cross-site form cannot send one).
    let host = parts
        .headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok());
    if host != Some(shared.own_host.as_str()) {
        return text(StatusCode::FORBIDDEN, "forbidden");
    }
    if let Some(origin) = parts.headers.get(header::ORIGIN)
        && origin.to_str().ok() != Some(format!("http://{}", shared.own_host).as_str())
    {
        return text(StatusCode::FORBIDDEN, "forbidden");
    }
    // The raw path, not the decoded one: `..%2F` must not turn into a path.
    let path = parts.uri.path().to_string();
    if !path.starts_with(BRIDGE_PREFIX) {
        if parts.method != Method::GET && parts.method != Method::HEAD {
            return text(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
        }
        return serve_file(shared.ui_root.clone(), path).await;
    }
    if parts.method == Method::POST && !is_json(parts.headers.get(header::CONTENT_TYPE)) {
        return text(StatusCode::FORBIDDEN, "forbidden");
    }
    let body = if parts.method == Method::POST {
        match read_json(body).await {
            Ok(v) => Some(v),
            Err(e) => return envelope(Err(e)),
        }
    } else {
        None
    };
    envelope(dispatch(&shared.transport, &parts.method, &path, body).await)
}

/// Whether a content type is `application/json`, with or without parameters.
fn is_json(value: Option<&HeaderValue>) -> bool {
    let Some(text) = value.and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let lower = text.trim().to_ascii_lowercase();
    match lower.strip_prefix("application/json") {
        Some("") => true,
        Some(rest) => !rest.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_'),
        None => false,
    }
}

async fn read_json(body: Body) -> Result<Value, ClientError> {
    let bytes = to_bytes(body, MAX_BODY_BYTES)
        .await
        .map_err(|_| bad("the body is over 1 MiB"))?;
    if bytes.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_slice(&bytes).map_err(|_| bad("the body is not JSON"))
}

/// A verb name: `[a-z][a-z_]*`, at most 64 characters.
fn is_verb_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 64
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c == '_')
}

fn to_value<T: serde::Serialize>(v: T) -> Result<Value, ClientError> {
    serde_json::to_value(v)
        .map_err(|_| ClientError::new(ErrorKind::Server, "the answer cannot be written"))
}

async fn dispatch(
    transport: &Transport<FileStore>,
    method: &Method,
    path: &str,
    body: Option<Value>,
) -> Result<Value, ClientError> {
    if path == "/bridge/paired" && method == Method::GET {
        return to_value(transport.paired().await?);
    }
    let Some(body) = body else {
        return Err(bad("the bridge route needs POST"));
    };
    match path {
        "/bridge/pair" => {
            let (Some(code), Some(name)) = (
                body.get("code").and_then(Value::as_str),
                body.get("device_name").and_then(Value::as_str),
            ) else {
                return Err(bad("pair needs a code and a device name"));
            };
            to_value(transport.pair(code, name).await?)
        }
        "/bridge/status" => to_value(transport.status().await?),
        "/bridge/forget" => {
            transport.forget().await?;
            Ok(Value::Null)
        }
        _ => match path.strip_prefix(VERBS_PREFIX) {
            Some(verb) => {
                if !is_verb_name(verb) {
                    return Err(bad("the verb name is not valid"));
                }
                if !body.is_object() {
                    return Err(bad("the verb arguments must be a JSON object"));
                }
                // The transport refuses a verb that is not on the allowlist before it connects.
                // `project_add` is typed so a malformed argument fails here and not at the server.
                if verb == "project_add" {
                    let args: ProjectAddArgs = serde_json::from_value(body)
                        .map_err(|_| bad("project_add needs a root and a command id"))?;
                    return to_value(transport.project_add(args).await?);
                }
                transport.verb(verb, body).await
            }
            None => Err(bad("unknown bridge route")),
        },
    }
}

// ---- the files ------------------------------------------------------------------------------------

fn content_type(file: &Path) -> &'static str {
    match file
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        _ => "application/octet-stream",
    }
}

fn percent_decode(raw: &str) -> Option<Vec<u8>> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let pair = raw.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(pair, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(out)
}

/// Serve one file of the bundle, and nothing outside `root`. The file work runs on the blocking
/// pool, so a slow read holds up no other request.
async fn serve_file(root: PathBuf, raw_path: String) -> Response {
    match tokio::task::spawn_blocking(move || read_bundle_file(&root, &raw_path)).await {
        Ok(response) => response,
        Err(_) => text(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
    }
}

fn read_bundle_file(root: &Path, raw_path: &str) -> Response {
    let Some(decoded) = percent_decode(raw_path).and_then(|b| String::from_utf8(b).ok()) else {
        return text(StatusCode::BAD_REQUEST, "bad path");
    };
    if decoded.contains('\0') {
        return text(StatusCode::BAD_REQUEST, "bad path");
    }
    let mut file = root.to_path_buf();
    for part in decoded.split('/').filter(|p| !p.is_empty() && *p != ".") {
        if part == ".." {
            return text(StatusCode::FORBIDDEN, "forbidden");
        }
        file.push(part);
    }
    if file.is_dir() {
        file.push("index.html");
    }
    // A link inside the bundle that leads outside it is not followed.
    let Ok(real) = std::fs::canonicalize(&file) else {
        return text(StatusCode::NOT_FOUND, "not found");
    };
    if !real.starts_with(root) {
        return text(StatusCode::FORBIDDEN, "forbidden");
    }
    match std::fs::read(&real) {
        Ok(data) => send(StatusCode::OK, content_type(&real), data),
        Err(_) => text(StatusCode::NOT_FOUND, "not found"),
    }
}

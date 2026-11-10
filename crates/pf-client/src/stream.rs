//! The chat stream of the pinned client (`docs/contracts/ui-transport.md`, "The chat stream").
//!
//! `Transport::open_stream` opens `GET /v1/stream` over the same pinned TLS 1.3 connection as the
//! verbs ([`crate::http::connect_pinned`]), with the device token in the upgrade's `Authorization`
//! header and nowhere else. It decodes each text message into [`pf_api::types::ChatFrame`], drops an
//! item it already holds, and reconnects after a drop with `after=<the last seq it holds>`. A revoke,
//! a changed key and every HTTP refusal end the stream as one `closed` event, and no retry follows.
//!
//! This is not polling. While a connection is open no timer makes a request: the server pushes, and
//! the silence limit is a local deadline that sends nothing. The reconnect schedule runs only while
//! there is no connection.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use pf_api::types::{ChatFrame, RequestFrame};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, WebSocketConfig};
use tokio_tungstenite::tungstenite::{self, Message};

use crate::error::{ClientError, ErrorKind, Route, error_from_status};
use crate::http::connect_pinned;
use crate::pin::parse_pin;
use crate::store::DeviceStore;
use crate::transport::Transport;

/// The close code of a device that was revoked.
const CLOSE_REVOKED: u16 = 4001;
/// The close code of a connection that fell behind the live frames.
const CLOSE_LAGGED: u16 = 4002;
/// The close code of a server that stops.
const CLOSE_GOING_AWAY: u16 = 1001;
/// The close code of a stream that got data from the client.
const CLOSE_POLICY: u16 = 1008;

/// The largest message the client reads.
const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;

/// How many events wait for the reader before the stream stops reading the socket.
const EVENT_QUEUE: usize = 256;

/// The `type` of every chat frame this client knows (`ChatFrame`). Another `type` is skipped.
const KNOWN_FRAMES: [&str; 6] = [
    "hello",
    "session",
    "item",
    "caught",
    "delta",
    "tool_running",
];

/// The `type` of every request frame (`RequestFrame`, `r1.s4.w3`). They ride every stream, whatever
/// session it follows.
const REQUEST_FRAMES: [&str; 3] = ["requests", "request", "claim"];

/// What to follow: one session (and its items after `after`), or none.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamOptions {
    #[serde(default)]
    pub session: Option<String>,
    /// The last `seq` the caller holds. An omitted `after` is 0.
    #[serde(default)]
    pub after: u64,
}

/// One thing a stream tells its reader. `closed` is the last event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
// A frame lives for one send, as in `pf-api`.
#[allow(clippy::large_enum_variant)]
pub enum StreamEvent {
    /// A frame of the server, decoded.
    Frame { frame: ChatFrame },
    /// A request frame of the server, decoded: a separate event, so every `ChatFrame` reader stays
    /// as it is (`r1.s4.w3`, W2). It never moves the `seq` a reconnect asks `after`.
    RequestFrame { frame: RequestFrame },
    /// The connection is gone and the client will try again in `retry_in_ms`.
    Reconnecting {
        after: u64,
        retry_in_ms: u64,
        reason: String,
    },
    /// The stream ended and will not try again.
    Closed { error: ClientError },
}

/// The time limits of a stream: the reconnect schedule, when a long connection resets it, and the
/// silence limit. They are a test seam (`Transport::with_stream_tuning`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamTuning {
    /// The waits between attempts, in order. The last one repeats.
    pub schedule: Vec<Duration>,
    /// A connection open this long resets the schedule.
    pub reset_after: Duration,
    /// A connection that receives nothing, not even a ping, for this long counts as dropped.
    pub silence: Duration,
}

impl Default for StreamTuning {
    fn default() -> StreamTuning {
        StreamTuning {
            schedule: [4, 8, 16, 32, 60]
                .into_iter()
                .map(Duration::from_secs)
                .collect(),
            reset_after: Duration::from_secs(60),
            silence: Duration::from_secs(75),
        }
    }
}

impl StreamTuning {
    /// The wait of the `step`th retry, counting from 0.
    fn delay(&self, step: usize) -> Duration {
        self.schedule
            .get(step.min(self.schedule.len().saturating_sub(1)))
            .copied()
            .unwrap_or(Duration::from_secs(60))
    }
}

/// An open stream. `next` gives the next event, and dropping the stream closes the socket (a close
/// frame, then the TCP close).
pub struct ChatStream {
    events: mpsc::Receiver<StreamEvent>,
}

// The stream holds no token: it reads the device store for each connection.
impl std::fmt::Debug for ChatStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatStream").finish_non_exhaustive()
    }
}

impl ChatStream {
    /// The next event, or `None` after `closed`.
    pub async fn next(&mut self) -> Option<StreamEvent> {
        self.events.recv().await
    }
}

impl<S: DeviceStore> Transport<S> {
    /// Open the chat stream. It spawns on the current tokio runtime and returns at once: every
    /// failure, a bad session ID included, arrives as a `closed` event.
    pub fn open_stream(&self, options: StreamOptions) -> ChatStream {
        let (tx, events) = mpsc::channel(EVENT_QUEUE);
        // The generation is read now, so a `forget()` after this call always ends the stream.
        let epoch = self.subscribe_forget();
        let transport = self.clone();
        tokio::spawn(run(transport, options, tx, epoch));
        ChatStream { events }
    }
}

fn not_paired() -> ClientError {
    ClientError::new(ErrorKind::Unauthorized, "this device is not paired")
}

/// `S-<n>` with n at least 1.
fn is_session_id(text: &str) -> bool {
    match text.strip_prefix("S-") {
        Some(digits) => {
            !digits.is_empty()
                && digits.len() <= 18
                && digits.bytes().all(|b| b.is_ascii_digit())
                && digits.bytes().any(|b| b != b'0')
        }
        None => false,
    }
}

/// How one connection attempt ended.
enum Attempt {
    /// The stream ends with this error. No retry.
    Closed(ClientError),
    /// The connection is gone. `open_for` is how long it was open, when it was.
    Retry {
        reason: String,
        immediate: bool,
        open_for: Option<Duration>,
    },
    /// The reader is gone.
    Gone,
}

fn dropped(open_for: Duration) -> Attempt {
    Attempt::Retry {
        reason: "the connection dropped".to_string(),
        immediate: false,
        open_for: Some(open_for),
    }
}

async fn run<S: DeviceStore>(
    transport: Transport<S>,
    options: StreamOptions,
    tx: mpsc::Sender<StreamEvent>,
    mut forgotten: watch::Receiver<u64>,
) {
    let closed = |error: ClientError| StreamEvent::Closed { error };
    if let Some(session) = &options.session
        && !is_session_id(session)
    {
        let _ = tx
            .send(closed(ClientError::new(
                ErrorKind::BadRequest,
                "the session is a chat session ID such as S-1",
            )))
            .await;
        return;
    }
    let tuning = transport.stream_tuning();
    let mut last_seq = options.after;
    // The request frames this stream dropped, over every connection.
    let mut dropped_requests = 0u64;
    let mut step = 0usize;
    let mut last_was_immediate = false;
    loop {
        // The reader leaving is seen inside `connection`, so an open socket gets its close frame.
        let outcome = tokio::select! {
            biased;
            _ = forgotten.changed() => Attempt::Closed(not_paired()),
            outcome = connection(
                &transport,
                &options.session,
                &mut last_seq,
                &mut dropped_requests,
                &tx,
            ) => outcome,
        };
        match outcome {
            Attempt::Gone => return,
            Attempt::Closed(error) => {
                let _ = tx.send(closed(error)).await;
                return;
            }
            Attempt::Retry {
                reason,
                immediate,
                open_for,
            } => {
                if open_for.is_some_and(|open| open >= tuning.reset_after) {
                    step = 0;
                    last_was_immediate = false;
                }
                // The first retry after a 4002 is at once. A second one in a row waits its step.
                let delay = if immediate && !last_was_immediate {
                    last_was_immediate = true;
                    Duration::ZERO
                } else {
                    last_was_immediate = false;
                    let wait = tuning.delay(step);
                    step += 1;
                    wait
                };
                let event = StreamEvent::Reconnecting {
                    after: last_seq,
                    retry_in_ms: u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                    reason,
                };
                if tx.send(event).await.is_err() {
                    return;
                }
                tokio::select! {
                    biased;
                    _ = tx.closed() => return,
                    _ = forgotten.changed() => {
                        let _ = tx.send(closed(not_paired())).await;
                        return;
                    }
                    _ = tokio::time::sleep(delay) => {}
                }
            }
        }
    }
}

/// What a text message is.
enum Decoded {
    Skip,
    Frame(Box<ChatFrame>),
    Request(Box<RequestFrame>),
    /// A request frame of a known `type` that does not decode. It is dropped: the requests state is
    /// a cache that the next `requests` snapshot repairs, and ending the stream would cut the chat.
    DroppedRequest,
    Unreadable,
}

fn decode(text: &str) -> Decoded {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return Decoded::Unreadable;
    };
    let Some(kind) = value.get("type").and_then(Value::as_str) else {
        return Decoded::Skip;
    };
    if REQUEST_FRAMES.contains(&kind) {
        return match serde_json::from_value::<RequestFrame>(value) {
            Ok(frame) => Decoded::Request(Box::new(frame)),
            Err(_) => Decoded::DroppedRequest,
        };
    }
    if !KNOWN_FRAMES.contains(&kind) {
        return Decoded::Skip;
    }
    match serde_json::from_value::<ChatFrame>(value) {
        Ok(frame) => Decoded::Frame(Box::new(frame)),
        Err(_) => Decoded::Unreadable,
    }
}

/// The outcome of a close frame from the server.
fn after_close(frame: Option<CloseFrame>, open_for: Duration) -> Attempt {
    let code = frame.map(|f| u16::from(f.code));
    match code {
        Some(CLOSE_REVOKED) => Attempt::Closed(ClientError::new(
            ErrorKind::Unauthorized,
            "this device was revoked; pair it again",
        )),
        Some(CLOSE_LAGGED) => Attempt::Retry {
            reason: "lagged".to_string(),
            immediate: true,
            open_for: Some(open_for),
        },
        Some(CLOSE_GOING_AWAY) => Attempt::Retry {
            reason: "the server is stopping".to_string(),
            immediate: false,
            open_for: Some(open_for),
        },
        Some(CLOSE_POLICY) => Attempt::Closed(ClientError::new(
            ErrorKind::Server,
            "the server closed the stream: policy violation",
        )),
        _ => dropped(open_for),
    }
}

/// One connection: connect, upgrade, read until it ends.
async fn connection<S: DeviceStore>(
    transport: &Transport<S>,
    session: &Option<String>,
    last_seq: &mut u64,
    dropped_requests: &mut u64,
    tx: &mpsc::Sender<StreamEvent>,
) -> Attempt {
    let paired = match transport.on_store(|store| store.load()).await {
        Ok(Some(paired)) => paired,
        Ok(None) => return Attempt::Closed(not_paired()),
        Err(error) => return Attempt::Closed(error),
    };
    let Some(pin) = parse_pin(&paired.pin) else {
        return Attempt::Closed(ClientError::new(
            ErrorKind::Server,
            "the stored pin is not valid",
        ));
    };
    let timeouts = transport.timeouts();
    let tuning = transport.stream_tuning();

    // The request is formed first: a request that cannot be formed never connects.
    let mut target = "/v1/stream".to_string();
    let mut query = Vec::new();
    if let Some(session) = session {
        query.push(format!("session={session}"));
    }
    if session.is_some() || *last_seq > 0 {
        query.push(format!("after={}", *last_seq));
    }
    if !query.is_empty() {
        target.push('?');
        target.push_str(&query.join("&"));
    }
    let Ok(mut request) = format!("wss://{}{target}", paired.address).into_client_request() else {
        return Attempt::Closed(ClientError::new(
            ErrorKind::BadRequest,
            "the stream address is not valid",
        ));
    };
    let Ok(mut bearer) = HeaderValue::from_str(&format!("Bearer {}", paired.token)) else {
        return Attempt::Closed(ClientError::new(
            ErrorKind::BadRequest,
            "the credential cannot be sent",
        ));
    };
    bearer.set_sensitive(true);
    request.headers_mut().insert("Authorization", bearer);

    let connected = tokio::select! {
        biased;
        _ = tx.closed() => return Attempt::Gone,
        connected = connect_pinned(&paired.address, &pin, timeouts.connect) => connected,
    };
    let tls = match connected {
        Ok(tls) => tls,
        Err(error) if error.kind == ErrorKind::KeyChanged => return Attempt::Closed(error),
        Err(error) => {
            return Attempt::Retry {
                reason: error.message,
                immediate: false,
                open_for: None,
            };
        }
    };
    let unreachable = || Attempt::Retry {
        reason: format!("cannot reach {}", paired.address),
        immediate: false,
        open_for: None,
    };
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_MESSAGE_BYTES));
    let handshake = tokio::select! {
        biased;
        _ = tx.closed() => return Attempt::Gone,
        handshake = tokio::time::timeout(
            timeouts.request,
            tokio_tungstenite::client_async_with_config(request, tls, Some(config)),
        ) => handshake,
    };
    let (mut socket, response) = match handshake {
        Ok(Ok(upgraded)) => upgraded,
        Ok(Err(tungstenite::Error::Http(response))) => {
            transport.note_server_headers(&paired.pin, response.headers());
            let body = response.body().as_deref().unwrap_or_default();
            return Attempt::Closed(error_from_status(
                response.status().as_u16(),
                Route::Token,
                body,
                &[&paired.token],
            ));
        }
        // An answer that is no WebSocket upgrade is not this server's.
        Ok(Err(tungstenite::Error::Protocol(_))) => {
            return Attempt::Closed(ClientError::new(
                ErrorKind::Server,
                "the server did not answer the stream as documented",
            ));
        }
        Ok(Err(_)) | Err(_) => return unreachable(),
    };
    transport.note_server_headers(&paired.pin, response.headers());

    let opened = Instant::now();
    // One stderr line for each connection at most, so a server that sends many bad frames cannot
    // fill a log.
    let mut logged = false;
    loop {
        let next = tokio::select! {
            biased;
            _ = tx.closed() => {
                let _ = tokio::time::timeout(Duration::from_millis(300), socket.close(None)).await;
                return Attempt::Gone;
            }
            next = tokio::time::timeout(tuning.silence, socket.next()) => next,
        };
        let message = match next {
            Ok(Some(Ok(message))) => message,
            Ok(Some(Err(_)) | None) | Err(_) => return dropped(opened.elapsed()),
        };
        match message {
            Message::Text(text) => match decode(text.as_str()) {
                Decoded::Skip => {}
                Decoded::DroppedRequest => {
                    *dropped_requests += 1;
                    if !logged {
                        logged = true;
                        // No frame text: a request carries text a seat wrote.
                        eprintln!(
                            "pf-client: dropped a request frame this client cannot read ({} so far on this stream)",
                            *dropped_requests
                        );
                    }
                }
                Decoded::Request(frame) => {
                    if tx
                        .send(StreamEvent::RequestFrame { frame: *frame })
                        .await
                        .is_err()
                    {
                        return Attempt::Gone;
                    }
                }
                Decoded::Unreadable => {
                    return Attempt::Closed(ClientError::new(
                        ErrorKind::Server,
                        "the server sent a frame this client cannot read",
                    ));
                }
                Decoded::Frame(frame) => {
                    match &*frame {
                        ChatFrame::Hello { server, .. } => {
                            transport.note_server(&paired.pin, server)
                        }
                        ChatFrame::Item { item }
                            if session.as_deref() == Some(&item.session_id) =>
                        {
                            // A redaction's re-send passes. Every other item is new only above the
                            // last `seq` this client holds.
                            if item.seq <= *last_seq && item.redacted_at.is_none() {
                                continue;
                            }
                            *last_seq = (*last_seq).max(item.seq);
                        }
                        _ => {}
                    }
                    if tx.send(StreamEvent::Frame { frame: *frame }).await.is_err() {
                        return Attempt::Gone;
                    }
                }
            },
            Message::Close(frame) => return after_close(frame, opened.elapsed()),
            // A ping is answered by the library; a binary message and a pong are skipped.
            _ => {}
        }
    }
}

// ---- the registry of the shell ----------------------------------------------------------------

#[derive(Default)]
struct Registry {
    next: AtomicU64,
    open: Mutex<HashMap<u64, tokio::task::AbortHandle>>,
}

/// The open streams of the Mac app's shell, by ID: the glue between a stream and the webview's
/// channel. Clones share the registry.
#[derive(Clone, Default)]
pub struct Streams {
    registry: Arc<Registry>,
}

impl std::fmt::Debug for Streams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Streams").finish_non_exhaustive()
    }
}

impl Streams {
    pub fn new() -> Streams {
        Streams::default()
    }

    /// Open a stream and forward each event to `sink`. A sink that answers `false` (the receiver is
    /// gone) ends the stream, and so does `closed`. Returns the stream's ID. It spawns on the current
    /// tokio runtime.
    pub fn open<S: DeviceStore>(
        &self,
        transport: &Transport<S>,
        options: StreamOptions,
        mut sink: impl FnMut(StreamEvent) -> bool + Send + 'static,
    ) -> u64 {
        let id = self.registry.next.fetch_add(1, Ordering::SeqCst) + 1;
        let mut stream = transport.open_stream(options);
        let registry = self.registry.clone();
        let mut open = self.registry.open.lock().unwrap_or_else(|e| e.into_inner());
        let task = tokio::spawn(async move {
            while let Some(event) = stream.next().await {
                let last = matches!(event, StreamEvent::Closed { .. });
                if !sink(event) || last {
                    break;
                }
            }
            registry
                .open
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
        });
        open.insert(id, task.abort_handle());
        id
    }

    /// End one stream. `false` when there is none with this ID.
    pub fn close(&self, id: u64) -> bool {
        let handle = self
            .registry
            .open
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
        match handle {
            Some(handle) => {
                handle.abort();
                true
            }
            None => false,
        }
    }

    /// End every stream.
    pub fn close_all(&self) {
        let handles: Vec<_> = self
            .registry
            .open
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
            .map(|(_, handle)| handle)
            .collect();
        for handle in handles {
            handle.abort();
        }
    }
}

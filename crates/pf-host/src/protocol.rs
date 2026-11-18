//! The host protocol, version 1 (`docs/contracts/session-host.md`).
//!
//! A control message is one JSON object on one line. Output (`out`) and input (`input`) are a JSON
//! header line, `{"t":"out","at":<cursor>,"n":<len>}`, followed by exactly `<len>` raw bytes. The
//! cursor is the byte offset since the child started. A frame holds at most [`FRAME_MAX`] bytes.
//!
//! The encoders here are the only place that writes a message, so the client and the host cannot
//! drift: the golden transcript (`tests/fixtures/v1-transcript.jsonl`) freezes their output.

use serde::{Deserialize, Serialize};

/// The lowest protocol version this crate speaks.
pub const PROTO_MIN: u32 = 1;
/// The highest protocol version this crate speaks.
pub const PROTO_MAX: u32 = 1;
/// The most bytes in one `out` or `input` frame.
pub const FRAME_MAX: usize = 256 * 1024;
/// The longest control line, newline excluded.
pub const LINE_MAX: usize = 64 * 1024;

/// A signal a controller may send to the child's process group.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalKind {
    Interrupt,
    Terminate,
    Kill,
}

impl SignalKind {
    pub fn number(self) -> i32 {
        match self {
            SignalKind::Interrupt => libc::SIGINT,
            SignalKind::Terminate => libc::SIGTERM,
            SignalKind::Kill => libc::SIGKILL,
        }
    }
}

/// How the child ended. `at` is the cursor after its last output byte.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitInfo {
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub at: u64,
}

/// What a client sends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientMsg {
    /// The first message of a connection: a protocol version and the attach key.
    Attach {
        proto: u32,
        #[serde(default)]
        key: String,
    },
    /// Stream output from this cursor.
    Read { from: u64 },
    /// The output below this cursor is consumed: the host may drop it.
    Ack { to: u64 },
    /// `n` raw bytes follow this header: input for the child.
    Input { n: usize },
    /// A new size for a PTY child.
    Resize { rows: u16, cols: u16 },
    /// A signal for the child's process group.
    Signal { sig: SignalKind },
    /// A screen snapshot. Version 1 answers `unsupported`.
    Snapshot,
    /// The exit was consumed.
    ExitAck,
}

/// What the host sends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum HostMsg {
    /// The only message before a valid attach.
    Version {
        proto_min: u32,
        proto_max: u32,
    },
    /// After a valid attach.
    Hello(Hello),
    /// `n` raw bytes follow this header: output from cursor `at`.
    Out {
        at: u64,
        n: usize,
    },
    /// Output from `from` to `to` was dropped by an overflow before it was acknowledged.
    Gap {
        from: u64,
        to: u64,
    },
    /// The child ended and the stream reached its last byte.
    Exit(ExitInfo),
    Error {
        code: String,
        text: String,
    },
}

/// The host's answer to a valid attach.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub proto: u32,
    pub session: String,
    pub incarnation: u64,
    /// The unit's `INVOCATION_ID`; empty under the detached launcher.
    pub invocation_id: String,
    pub child_pid: u32,
    pub record_sha256: String,
    /// Present when the child already ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<ExitInfo>,
}

/// A frame that cannot be read. The host answers it with an `error` and closes.
#[derive(Debug, PartialEq, Eq)]
pub struct ProtoError {
    pub code: &'static str,
    pub text: String,
}

fn header_line<T: Serialize>(msg: &T) -> Vec<u8> {
    let mut line = serde_json::to_vec(msg).expect("a message always serializes");
    line.push(b'\n');
    line
}

/// The bytes of one client message. `data` is the input for `Input` and empty for the others.
pub fn encode_client(msg: &ClientMsg, data: &[u8]) -> Vec<u8> {
    let mut out = header_line(msg);
    if let ClientMsg::Input { n } = msg {
        assert_eq!(*n, data.len(), "an input header names the bytes it carries");
        out.extend_from_slice(data);
    }
    out
}

/// The bytes of one host message. `data` is the output for `Out` and empty for the others.
pub fn encode_host(msg: &HostMsg, data: &[u8]) -> Vec<u8> {
    let mut out = header_line(msg);
    if let HostMsg::Out { n, .. } = msg {
        assert_eq!(*n, data.len(), "an out header names the bytes it carries");
        out.extend_from_slice(data);
    }
    out
}

/// Take one frame off the front of `buf`: the message and its raw bytes. `Ok(None)` when the frame
/// is not all there yet. `body_len` says how many raw bytes follow a header.
fn take<M: for<'a> Deserialize<'a>>(
    buf: &mut Vec<u8>,
    body_len: impl Fn(&M) -> usize,
) -> Result<Option<(M, Vec<u8>)>, ProtoError> {
    let Some(nl) = buf.iter().position(|b| *b == b'\n') else {
        if buf.len() > LINE_MAX {
            return Err(ProtoError {
                code: "line_too_long",
                text: format!("a control line is at most {LINE_MAX} bytes"),
            });
        }
        return Ok(None);
    };
    if nl > LINE_MAX {
        return Err(ProtoError {
            code: "line_too_long",
            text: format!("a control line is at most {LINE_MAX} bytes"),
        });
    }
    let msg: M = serde_json::from_slice(&buf[..nl]).map_err(|e| ProtoError {
        code: "bad_message",
        text: format!("cannot read the message: {e}"),
    })?;
    let n = body_len(&msg);
    if n > FRAME_MAX {
        return Err(ProtoError {
            code: "frame_too_large",
            text: format!("a frame is at most {FRAME_MAX} bytes"),
        });
    }
    if buf.len() < nl + 1 + n {
        return Ok(None);
    }
    let body = buf[nl + 1..nl + 1 + n].to_vec();
    buf.drain(..nl + 1 + n);
    Ok(Some((msg, body)))
}

/// The next client message in `buf`, and the raw bytes after an `input` header.
pub fn take_client(buf: &mut Vec<u8>) -> Result<Option<(ClientMsg, Vec<u8>)>, ProtoError> {
    take(buf, |m: &ClientMsg| match m {
        ClientMsg::Input { n } => *n,
        _ => 0,
    })
}

/// The next host message in `buf`, and the raw bytes after an `out` header.
pub fn take_host(buf: &mut Vec<u8>) -> Result<Option<(HostMsg, Vec<u8>)>, ProtoError> {
    take(buf, |m: &HostMsg| match m {
        HostMsg::Out { n, .. } => *n,
        _ => 0,
    })
}

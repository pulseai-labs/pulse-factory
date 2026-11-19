//! The client the controller uses to talk to a host: connect and attach, read output from a
//! cursor, acknowledge it, send input, resize and signals, and check the host it reached.
//!
//! Every call has a timeout (5 s unless [`Conn::set_timeout`] says otherwise). A connection that
//! another controller superseded fails with [`ClientError::Superseded`], so the caller can tell it
//! from a lost connection.
//!
//! [`Conn::peer_check`] is the controller's half of the attach check: it reads the host's PID
//! through `SO_PEERPIDFD` (falling back to `SO_PEERCRED` on a kernel without it) and its cgroup
//! from `/proc`. Like the host's checks, it stops mistakes, not a hostile process of the same user.

use std::fmt;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::peer;
use crate::protocol::{
    ClientMsg, ExitInfo, Hello, HostMsg, PROTO_MAX, PROTO_MIN, SignalKind, encode_client, take_host,
};

/// The default timeout of every call.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// Why a call failed.
#[derive(Debug)]
pub enum ClientError {
    Io(std::io::Error),
    /// The call did not finish in time. The connection is still usable.
    Timeout,
    /// The host closed the connection.
    Closed,
    /// The host refused the attach: a wrong key, a wrong version or a wrong peer.
    Refused {
        code: String,
        text: String,
    },
    /// Another controller attached and took this connection's place.
    Superseded,
    /// The host sent something the protocol does not allow.
    Protocol(String),
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClientError::Io(e) => write!(f, "host connection: {e}"),
            ClientError::Timeout => f.write_str("the host did not answer in time"),
            ClientError::Closed => f.write_str("the host closed the connection"),
            ClientError::Refused { code, text } => {
                write!(f, "the host refused the attach ({code}): {text}")
            }
            ClientError::Superseded => f.write_str("another controller attached to the host"),
            ClientError::Protocol(why) => write!(f, "host protocol error: {why}"),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<std::io::Error> for ClientError {
    fn from(e: std::io::Error) -> ClientError {
        match e.kind() {
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => ClientError::Timeout,
            std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::UnexpectedEof => ClientError::Closed,
            _ => ClientError::Io(e),
        }
    }
}

/// What the host streams after a `read`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Output bytes that start at cursor `at`.
    Out { at: u64, data: Vec<u8> },
    /// Output from `from` to `to` was lost to an overflow.
    Gap { from: u64, to: u64 },
    /// The child ended and every output byte was sent.
    Exit(ExitInfo),
    /// The host answered a request with an error: `unsupported`, `not_pty` and so on.
    Error { code: String, text: String },
}

/// Why [`Conn::peer_check`] refused a host.
#[derive(Debug, PartialEq, Eq)]
pub enum PeerCheckError {
    /// The kernel gave no PID for the host.
    NoPid(String),
    /// The host's cgroup could not be read.
    NoCgroup(i32),
    /// The host is in another cgroup than the one expected.
    WrongCgroup { found: String, expected: String },
}

impl fmt::Display for PeerCheckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PeerCheckError::NoPid(why) => write!(f, "cannot learn the host's PID: {why}"),
            PeerCheckError::NoCgroup(pid) => {
                write!(f, "cannot read the cgroup of the host (pid {pid})")
            }
            PeerCheckError::WrongCgroup { found, expected } => {
                write!(f, "the host is in cgroup {found}, not {expected}")
            }
        }
    }
}

/// The controller's half of the attach check, as a function: the host behind `conn` must be in
/// `expected_cgroup`, the cgroup the registry row names. It is [`Conn::peer_check`].
pub fn peer_check(conn: &Conn, expected_cgroup: &str) -> Result<(), PeerCheckError> {
    conn.peer_check(expected_cgroup)
}

/// One attached connection to a host.
pub struct Conn {
    stream: UnixStream,
    rbuf: Vec<u8>,
    timeout: Duration,
}

/// Connect to a host's socket and attach with `key`. The client takes the highest protocol both
/// sides speak. Returns the connection and the host's `hello`.
pub fn connect(socket: &Path, key: &str) -> Result<(Conn, Hello), ClientError> {
    connect_with(socket, key, None)
}

/// As [`connect`], with a protocol version to send in place of the negotiated one. A test of the
/// host's refusal of a version it does not speak uses it.
pub fn connect_with(
    socket: &Path,
    key: &str,
    proto: Option<u32>,
) -> Result<(Conn, Hello), ClientError> {
    let stream = UnixStream::connect(socket)?;
    let mut conn = Conn {
        stream,
        rbuf: Vec::new(),
        timeout: DEFAULT_TIMEOUT,
    };
    conn.set_timeout(DEFAULT_TIMEOUT);
    let (lo, hi) = match conn.next_msg()?.0 {
        HostMsg::Version {
            proto_min,
            proto_max,
        } => (proto_min, proto_max),
        HostMsg::Error { code, text } => return Err(ClientError::Refused { code, text }),
        other => {
            return Err(ClientError::Protocol(format!(
                "expected the host's version, got {other:?}"
            )));
        }
    };
    let common = hi.min(PROTO_MAX);
    if common < lo.max(PROTO_MIN) && proto.is_none() {
        return Err(ClientError::Protocol(format!(
            "the host speaks protocol {lo} to {hi}; this client speaks {PROTO_MIN} to {PROTO_MAX}"
        )));
    }
    // A host that refused the peer closed the connection already, so the write can fail while its
    // `error` is waiting to be read. Read first; report the write error only if nothing explains it.
    let sent = conn.send(
        &ClientMsg::Attach {
            proto: proto.unwrap_or(common),
            key: key.to_string(),
        },
        &[],
    );
    match conn.next_msg() {
        Ok((HostMsg::Hello(hello), _)) => Ok((conn, hello)),
        Ok((HostMsg::Error { code, text }, _)) => Err(ClientError::Refused { code, text }),
        Ok((other, _)) => Err(ClientError::Protocol(format!(
            "expected hello after attach, got {other:?}"
        ))),
        Err(e) => Err(sent.err().unwrap_or(e)),
    }
}

impl Conn {
    /// Change the timeout of every later call.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
        let _ = self.stream.set_read_timeout(Some(timeout));
        let _ = self.stream.set_write_timeout(Some(timeout));
    }

    fn send(&mut self, msg: &ClientMsg, data: &[u8]) -> Result<(), ClientError> {
        self.stream.write_all(&encode_client(msg, data))?;
        Ok(())
    }

    /// The next message from the host, with its raw bytes. Waits up to the timeout.
    fn next_msg(&mut self) -> Result<(HostMsg, Vec<u8>), ClientError> {
        let end = Instant::now() + self.timeout;
        loop {
            match take_host(&mut self.rbuf) {
                Ok(Some(frame)) => return Ok(frame),
                Ok(None) => {}
                Err(e) => return Err(ClientError::Protocol(e.text)),
            }
            if Instant::now() >= end {
                return Err(ClientError::Timeout);
            }
            let mut chunk = vec![0u8; 64 * 1024];
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err(ClientError::Closed),
                Ok(n) => self.rbuf.extend_from_slice(&chunk[..n]),
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Ask the host to stream output from `from`.
    pub fn read_from(&mut self, from: u64) -> Result<(), ClientError> {
        self.send(&ClientMsg::Read { from }, &[])
    }

    /// Say the output below `to` is consumed. Call it only after the commit that consumed it.
    pub fn ack(&mut self, to: u64) -> Result<(), ClientError> {
        self.send(&ClientMsg::Ack { to }, &[])
    }

    /// Send input to the child. At most 256 KiB at once.
    pub fn input(&mut self, data: &[u8]) -> Result<(), ClientError> {
        self.send(&ClientMsg::Input { n: data.len() }, data)
    }

    /// Resize a PTY child.
    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<(), ClientError> {
        self.send(&ClientMsg::Resize { rows, cols }, &[])
    }

    /// Signal the child's process group.
    pub fn signal(&mut self, sig: SignalKind) -> Result<(), ClientError> {
        self.send(&ClientMsg::Signal { sig }, &[])
    }

    /// Ask for a screen snapshot. Version 1 answers an `unsupported` error event.
    pub fn snapshot(&mut self) -> Result<(), ClientError> {
        self.send(&ClientMsg::Snapshot, &[])
    }

    /// Say the exit was consumed. The host ends once the last byte is acknowledged too.
    pub fn exit_ack(&mut self) -> Result<(), ClientError> {
        self.send(&ClientMsg::ExitAck, &[])
    }

    /// The next event the host streams. [`ClientError::Timeout`] when none came in time.
    pub fn next_event(&mut self) -> Result<Event, ClientError> {
        let (msg, body) = self.next_msg()?;
        match msg {
            HostMsg::Out { at, .. } => Ok(Event::Out { at, data: body }),
            HostMsg::Gap { from, to } => Ok(Event::Gap { from, to }),
            HostMsg::Exit(exit) => Ok(Event::Exit(exit)),
            HostMsg::Error { code, text } if code == "superseded" => {
                let _ = text;
                Err(ClientError::Superseded)
            }
            HostMsg::Error { code, text } => Ok(Event::Error { code, text }),
            other => Err(ClientError::Protocol(format!("unexpected {other:?}"))),
        }
    }

    /// The controller's half of the attach check: the host must be in `expected_cgroup`. The PID
    /// comes from `SO_PEERPIDFD`, or from `SO_PEERCRED` where the kernel lacks the option.
    pub fn peer_check(&self, expected_cgroup: &str) -> Result<(), PeerCheckError> {
        let fd = self.stream.as_raw_fd();
        let pid = match peer::peer_pid_by_pidfd(fd) {
            Ok(pid) => pid,
            Err(_) => peer::peer_cred(fd)
                .map(|p| p.pid)
                .map_err(|e| PeerCheckError::NoPid(e.to_string()))?,
        };
        let found = peer::cgroup_of(pid).ok_or(PeerCheckError::NoCgroup(pid))?;
        if found == expected_cgroup {
            Ok(())
        } else {
            Err(PeerCheckError::WrongCgroup {
                found,
                expected: expected_cgroup.to_string(),
            })
        }
    }
}

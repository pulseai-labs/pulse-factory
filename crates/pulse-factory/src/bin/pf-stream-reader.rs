//! `pf-stream-reader`: a small client of the chat stream (`GET /v1/stream`) for the demo and the
//! tests (`r1.s3.w3`). A shell has no pinned WSS client on every runner (CI's `curl` may lack
//! `wss`), so this binary stands in. It is not a product surface: w4's `pf-client` stream may
//! replace it later.
//!
//! ```text
//! pf-stream-reader --url wss://<addr>/v1/stream?... --pin <spki hex> --token-file <file> [--max-seconds <n>]
//!                  [--ack-notices] [--withhold-acks <n>]
//! ```
//!
//! It builds its TLS config with `pf_client::pin::client_config(PinVerifier::new(pin))`, so the pin
//! check is pf-client's and not a copy. It prints each frame as one line of JSON on stdout. On a
//! close it prints `close <code> <reason>` on stderr and exits 0. A refused upgrade prints
//! `status <code>` and exits 1. A key that is not the pinned one prints `server key changed:
//! re-pair` and exits 90. The time limit exits 2. The token is read from a file, never from argv; an
//! empty file sends no token.
//!
//! With `--ack-notices` it answers each `notice` frame it prints with `notice_ack` (`shown`, at the
//! frame's position), through `pf_client::http::pinned_call` with the token and the pin it has
//! already, except the first `--withhold-acks <n>` frames. The demo d32 uses that as its fault: a
//! device that never acknowledges, and then one that does (`r2.s11.w3`).

use std::io::Write;
use std::net::TcpStream;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use pf_client::http::{Timeouts, pinned_call};
use pf_client::pin::{PinVerifier, client_config, parse_pin};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::tungstenite::{self, Error, HandshakeError};

fn usage() -> ExitCode {
    eprintln!(
        "usage: pf-stream-reader --url wss://<addr>/v1/stream[?session=S-1&after=0] --pin <spki hex> --token-file <file> [--max-seconds <n>] [--ack-notices] [--withhold-acks <n>]"
    );
    ExitCode::from(64)
}

struct Args {
    url: String,
    pin: String,
    token_file: String,
    max_seconds: Option<u64>,
    /// Acknowledge each `notice` frame as `shown`.
    ack_notices: bool,
    /// The first frames that are not acknowledged.
    withhold_acks: u64,
}

fn parse_args() -> Option<Args> {
    let mut url = None;
    let mut pin = None;
    let mut token_file = None;
    let mut max_seconds = None;
    let mut ack_notices = false;
    let mut withhold_acks = 0;
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        if flag == "--ack-notices" {
            ack_notices = true;
            continue;
        }
        let value = it.next()?;
        match flag.as_str() {
            "--url" => url = Some(value),
            "--pin" => pin = Some(value),
            "--token-file" => token_file = Some(value),
            "--max-seconds" => max_seconds = Some(value.parse().ok()?),
            "--withhold-acks" => withhold_acks = value.parse().ok()?,
            _ => return None,
        }
    }
    Some(Args {
        url: url?,
        pin: pin?,
        token_file: token_file?,
        max_seconds,
        ack_notices,
        withhold_acks,
    })
}

/// The position of a `notice` frame line, or `None` for any other line.
fn notice_position(line: &str) -> Option<u64> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    if value.get("type")?.as_str()? != "notice" {
        return None;
    }
    value.get("notice")?.get("position")?.as_u64()
}

/// `notice_ack` as `shown`, at `position`. A failure prints one line on stderr and costs the read
/// nothing: the server sends the notice again.
fn acknowledge(addr: &str, pin: &[u8; 32], token: &str, position: u64) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("pf-stream-reader: cannot acknowledge: {e}");
            return;
        }
    };
    let body = serde_json::json!({ "position": position, "outcome": "shown" })
        .to_string()
        .into_bytes();
    let bearer = (!token.is_empty()).then_some(token);
    let sent = runtime.block_on(pinned_call(
        addr,
        pin,
        "POST",
        "/v1/verbs/notice_ack",
        bearer,
        Some(body),
        Timeouts::default(),
    ));
    match sent {
        Ok(response) if response.status == 200 => {}
        Ok(response) => eprintln!(
            "pf-stream-reader: notice_ack {position} answered {}",
            response.status
        ),
        Err(e) => eprintln!(
            "pf-stream-reader: notice_ack {position} failed: {}",
            e.message
        ),
    }
}

/// `host:port` of a `wss://` URL.
fn authority(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("wss://")?;
    Some(rest.split(['/', '?']).next().unwrap_or(rest))
}

fn main() -> ExitCode {
    let Some(args) = parse_args() else {
        return usage();
    };
    let Some(pin) = parse_pin(args.pin.trim()) else {
        eprintln!("pf-stream-reader: --pin is not 64 hex digits");
        return ExitCode::from(64);
    };
    // An empty token file means no token: the upgrade is sent with no `Authorization` header, which
    // lets the demo show the 401 of a reader that has none.
    let token = match std::fs::read_to_string(&args.token_file) {
        Ok(t) => t.trim().to_string(),
        Err(e) => {
            eprintln!("pf-stream-reader: cannot read the token file: {e}");
            return ExitCode::from(64);
        }
    };
    let Some(addr) = authority(&args.url) else {
        eprintln!("pf-stream-reader: --url must start with wss://");
        return ExitCode::from(64);
    };
    let verifier = Arc::new(PinVerifier::new(pin));
    let config = match client_config(verifier.clone()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("pf-stream-reader: {e}");
            return ExitCode::from(64);
        }
    };
    let host = addr.rsplit_once(':').map_or(addr, |(h, _)| h);
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let Ok(name) = rustls::pki_types::ServerName::try_from(host.to_string()) else {
        eprintln!("pf-stream-reader: {host:?} is not a host name or an address");
        return ExitCode::from(64);
    };
    let tcp = match TcpStream::connect(addr) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("pf-stream-reader: cannot connect to {addr}: {e}");
            return ExitCode::from(1);
        }
    };
    let conn = match rustls::ClientConnection::new(config, name) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("pf-stream-reader: {e}");
            return ExitCode::from(1);
        }
    };
    let tls = rustls::StreamOwned::new(conn, tcp);
    let mut request = match args.url.as_str().into_client_request() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("pf-stream-reader: the URL is no good: {e}");
            return ExitCode::from(64);
        }
    };
    if !token.is_empty() {
        let Ok(bearer) = format!("Bearer {token}").parse() else {
            eprintln!("pf-stream-reader: the token is no header value");
            return ExitCode::from(64);
        };
        request.headers_mut().insert("Authorization", bearer);
    }

    let mut socket = match tungstenite::client(request, tls) {
        Ok((socket, _)) => socket,
        Err(HandshakeError::Failure(Error::Http(response))) => {
            eprintln!("status {}", response.status().as_u16());
            return ExitCode::from(1);
        }
        Err(HandshakeError::Failure(e)) => {
            if verifier.mismatch() {
                eprintln!("server key changed: re-pair");
                return ExitCode::from(90);
            }
            eprintln!("pf-stream-reader: the handshake failed: {e}");
            return ExitCode::from(1);
        }
        Err(HandshakeError::Interrupted(_)) => {
            eprintln!("pf-stream-reader: the handshake was interrupted");
            return ExitCode::from(1);
        }
    };
    let _ = socket
        .get_mut()
        .sock
        .set_read_timeout(Some(Duration::from_millis(200)));
    let deadline = args
        .max_seconds
        .map(|s| Instant::now() + Duration::from_secs(s));
    let stdout = std::io::stdout();
    let mut notices_seen: u64 = 0;
    loop {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return ExitCode::from(2);
        }
        match socket.read() {
            Ok(Message::Text(text)) => {
                let mut out = stdout.lock();
                let _ = writeln!(out, "{}", text.as_str());
                let _ = out.flush();
                drop(out);
                if args.ack_notices
                    && let Some(position) = notice_position(text.as_str())
                {
                    notices_seen += 1;
                    if notices_seen > args.withhold_acks {
                        acknowledge(addr, &pin, &token, position);
                    }
                }
            }
            Ok(Message::Close(frame)) => {
                match frame {
                    Some(f) => eprintln!("close {} {}", u16::from(f.code), f.reason),
                    None => eprintln!("close 1005 "),
                }
                let _ = socket.flush();
                return ExitCode::SUCCESS;
            }
            Ok(_) => {}
            Err(Error::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(Error::ConnectionClosed | Error::AlreadyClosed) => {
                eprintln!("close 1006 the connection closed");
                return ExitCode::SUCCESS;
            }
            Err(e) => {
                eprintln!("close 1006 {e}");
                return ExitCode::SUCCESS;
            }
        }
    }
}

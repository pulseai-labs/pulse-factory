//! One request on one pinned TLS connection.
//!
//! Every call connects, checks the pin, sends one request with `Connection: close`, reads the answer
//! and closes. There is a 10-second connect timeout (TCP and the handshake) and a 30-second request
//! timeout. A pin that does not match fails the handshake before this module writes a byte.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::header::{AUTHORIZATION, CONNECTION, CONTENT_TYPE, HOST, HeaderValue};
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;

use crate::error::{ClientError, ErrorKind};
use crate::pin::{PinVerifier, client_config};

/// The longest answer the client reads.
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

/// The time limits of one call.
#[derive(Clone, Copy, Debug)]
pub struct Timeouts {
    pub connect: Duration,
    pub request: Duration,
}

impl Default for Timeouts {
    fn default() -> Timeouts {
        Timeouts {
            connect: Duration::from_secs(10),
            request: Duration::from_secs(30),
        }
    }
}

/// The status and the body of an answer, and the server it names (`pf-server-id` and
/// `pf-server-name`). A server of an older version names none.
#[derive(Debug)]
pub struct RawResponse {
    pub status: u16,
    pub body: Vec<u8>,
    pub server_id: Option<String>,
    pub server_name: Option<String>,
}

fn unreachable(address: &str) -> ClientError {
    ClientError::new(ErrorKind::Unreachable, format!("cannot reach {address}"))
}

/// The name in the TLS handshake. The verifier ignores it, so a name that does not parse falls back
/// to a fixed one.
fn server_name(address: &str) -> ServerName<'static> {
    let host = address
        .rsplit_once(':')
        .map_or(address, |(h, _)| h)
        .trim_start_matches('[')
        .trim_end_matches(']');
    if let Ok(ip) = host.parse::<IpAddr>() {
        return ServerName::IpAddress(ip.into());
    }
    ServerName::try_from(host.to_string()).unwrap_or_else(|_| {
        ServerName::try_from("pulse-factory.invalid").expect("a fixed valid name")
    })
}

/// The text of a header, when it is there and plain text.
pub(crate) fn header_text(headers: &hyper::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

/// Connect to `address` and finish the TLS 1.3 handshake with the key pinned to `pin`. A key that is
/// not the pin is `key_changed`, and the handshake fails before this function writes a byte. Every
/// other failure (a refused connection, a timeout, a TLS failure before the pin check) is
/// `unreachable`. The verbs and the chat stream both connect through here.
pub async fn connect_pinned(
    address: &str,
    pin: &[u8; 32],
    connect_timeout: Duration,
) -> Result<tokio_rustls::client::TlsStream<tokio::net::TcpStream>, ClientError> {
    let verifier = Arc::new(PinVerifier::new(*pin));
    let connector = tokio_rustls::TlsConnector::from(client_config(verifier.clone())?);
    let connect = async {
        let tcp = tokio::net::TcpStream::connect(address).await?;
        connector.connect(server_name(address), tcp).await
    };
    match tokio::time::timeout(connect_timeout, connect).await {
        Ok(Ok(tls)) => Ok(tls),
        Ok(Err(_)) if verifier.mismatch() => Err(ClientError::key_changed()),
        Ok(Err(_)) | Err(_) => Err(unreachable(address)),
    }
}

/// Send one request to `address` on a connection pinned to `pin`.
pub async fn pinned_call(
    address: &str,
    pin: &[u8; 32],
    method: &str,
    path: &str,
    bearer: Option<&str>,
    body: Option<Vec<u8>>,
    timeouts: Timeouts,
) -> Result<RawResponse, ClientError> {
    // The request is formed first: a request that cannot be formed never connects.
    let mut builder = hyper::Request::builder()
        .method(method)
        .uri(path)
        .header(HOST, address)
        .header(CONNECTION, "close");
    if let Some(token) = bearer {
        let mut value = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| {
            ClientError::new(ErrorKind::BadRequest, "the credential cannot be sent")
        })?;
        value.set_sensitive(true);
        builder = builder.header(AUTHORIZATION, value);
    }
    if body.is_some() {
        builder = builder.header(CONTENT_TYPE, "application/json");
    }
    let request = builder
        .body(Full::new(Bytes::from(body.unwrap_or_default())))
        .map_err(|_| ClientError::new(ErrorKind::BadRequest, "the request cannot be formed"))?;

    let tls = connect_pinned(address, pin, timeouts.connect).await?;

    let exchange = async {
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake(TokioIo::new(tls)).await?;
        let driver = tokio::spawn(async move {
            let _ = connection.await;
        });
        let result = async {
            let response = sender.send_request(request).await?;
            let status = response.status().as_u16();
            let server_id = header_text(response.headers(), "pf-server-id");
            let server_name = header_text(response.headers(), "pf-server-name");
            let body = Limited::new(response.into_body(), MAX_RESPONSE_BYTES)
                .collect()
                .await
                .map_err(|e| e.to_string())
                .map(|c| c.to_bytes().to_vec());
            Ok::<_, hyper::Error>((status, server_id, server_name, body))
        }
        .await;
        driver.abort();
        result
    };
    match tokio::time::timeout(timeouts.request, exchange).await {
        Ok(Ok((status, server_id, server_name, Ok(body)))) => Ok(RawResponse {
            status,
            body,
            server_id,
            server_name,
        }),
        Ok(Ok((_, _, _, Err(_)))) => Err(ClientError::new(
            ErrorKind::Server,
            "the answer of the server could not be read",
        )),
        Ok(Err(_)) | Err(_) => Err(unreachable(address)),
    }
}

//! AC-1 of `r1.s2.w4`: the verifier accepts a self-signed certificate whose SPKI hash is the pin and
//! refuses one whose hash is not, with no chain or host-name check either way. It refuses TLS 1.2. A
//! failed pin closes the connection before any HTTP byte is written.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use pf_client::error::ErrorKind;
use pf_client::http::{Timeouts, pinned_call};
use pf_client::pin::{PinVerifier, spki_sha256};
use rcgen::{CertificateParams, KeyPair, PublicKeyData};
use rustls::client::danger::ServerCertVerifier;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};

struct Identity {
    cert: Vec<u8>,
    key: Vec<u8>,
    pin: [u8; 32],
}

fn identity(name: &str) -> Identity {
    let key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
    let params = CertificateParams::new(vec![name.to_string()]).unwrap();
    let cert = params.self_signed(&key).unwrap();
    let hash = pf_engine::claim::sha256_hex(&key.subject_public_key_info());
    let pin = <[u8; 32]>::try_from(pf_api::code::unhex(&hash).unwrap()).unwrap();
    Identity {
        cert: cert.der().to_vec(),
        key: key.serialize_der(),
        pin,
    }
}

fn server_config(
    id: &Identity,
    versions: &[&'static rustls::SupportedProtocolVersion],
) -> Arc<rustls::ServerConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(id.key.clone()));
    Arc::new(
        rustls::ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(versions)
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![CertificateDer::from(id.cert.clone())], key)
            .unwrap(),
    )
}

/// A TLS server on a free loopback port. It answers one HTTP request with `{"ok":1}` and counts the
/// bytes it read after the handshake.
struct Server {
    addr: String,
    bytes_read: Arc<AtomicUsize>,
}

async fn serve(config: Arc<rustls::ServerConfig>) -> Server {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let bytes_read = Arc::new(AtomicUsize::new(0));
    let counter = bytes_read.clone();
    let acceptor = tokio_rustls::TlsAcceptor::from(config);
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            let acceptor = acceptor.clone();
            let counter = counter.clone();
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(tcp).await else {
                    return;
                };
                let mut buf = vec![0u8; 4096];
                let mut seen = Vec::new();
                loop {
                    match tls.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => {
                            counter.fetch_add(n, Ordering::SeqCst);
                            seen.extend_from_slice(&buf[..n]);
                            if seen.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                    }
                }
                let body = br#"{"ok":1}"#;
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = tls.write_all(head.as_bytes()).await;
                let _ = tls.write_all(body).await;
                let _ = tls.shutdown().await;
            });
        }
    });
    Server { addr, bytes_read }
}

fn timeouts() -> Timeouts {
    Timeouts {
        connect: Duration::from_secs(5),
        request: Duration::from_secs(5),
    }
}

#[test]
fn the_spki_hash_of_a_certificate_is_the_hash_of_its_key_info() {
    let id = identity("127.0.0.1");
    assert_eq!(spki_sha256(&id.cert), Some(id.pin));
    assert_eq!(spki_sha256(b"not a certificate"), None);
    assert_eq!(spki_sha256(&[]), None);
    assert_eq!(spki_sha256(&id.cert[..40]), None);
}

#[test]
fn the_verifier_accepts_the_pinned_key_and_refuses_another() {
    let id = identity("127.0.0.1");
    let other = identity("127.0.0.1");
    let verifier = PinVerifier::new(id.pin);
    let cert = CertificateDer::from(id.cert.clone());
    // No chain, so a self-signed certificate passes. No host-name check, so any name passes. No clock
    // check, so any time passes.
    for name in ["127.0.0.1", "other.example", "pulse-factory.invalid"] {
        let server = ServerName::try_from(name).unwrap().to_owned();
        for now in [0u64, 1_800_000_000, 9_000_000_000] {
            let ok = verifier.verify_server_cert(
                &cert,
                &[],
                &server,
                &[],
                UnixTime::since_unix_epoch(Duration::from_secs(now)),
            );
            assert!(ok.is_ok(), "{name} at {now}: {ok:?}");
        }
    }
    let foreign = CertificateDer::from(other.cert.clone());
    let server = ServerName::try_from("127.0.0.1").unwrap().to_owned();
    let refused = verifier.verify_server_cert(&foreign, &[], &server, &[], UnixTime::now());
    assert!(refused.is_err());
    assert!(
        verifier.mismatch(),
        "the verifier records that the pin failed"
    );
    // A certificate that is not a certificate is refused too.
    let junk = CertificateDer::from(vec![1, 2, 3]);
    assert!(
        PinVerifier::new(id.pin)
            .verify_server_cert(&junk, &[], &server, &[], UnixTime::now())
            .is_err()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_pinned_call_reaches_a_server_with_the_pinned_key() {
    let id = identity("127.0.0.1");
    let server = serve(server_config(&id, &[&rustls::version::TLS13])).await;
    let r = pinned_call(
        &server.addr,
        &id.pin,
        "GET",
        "/v1/status",
        Some("tok"),
        None,
        timeouts(),
    )
    .await
    .unwrap();
    assert_eq!(r.status, 200);
    assert_eq!(r.body, br#"{"ok":1}"#);
}

#[tokio::test(flavor = "current_thread")]
async fn a_wrong_pin_fails_closed_before_any_http_byte_is_written() {
    let id = identity("127.0.0.1");
    let server = serve(server_config(&id, &[&rustls::version::TLS13])).await;
    let wrong = identity("127.0.0.1").pin;
    let e = pinned_call(
        &server.addr,
        &wrong,
        "POST",
        "/v1/pair",
        None,
        Some(b"{\"code\":\"x\"}".to_vec()),
        timeouts(),
    )
    .await
    .unwrap_err();
    assert_eq!(e.kind, ErrorKind::KeyChanged);
    assert_eq!(e.message, "server key changed: re-pair");
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        server.bytes_read.load(Ordering::SeqCst),
        0,
        "no byte reached the server"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn tls_12_is_refused() {
    let id = identity("127.0.0.1");
    let server = serve(server_config(&id, &[&rustls::version::TLS12])).await;
    let e = pinned_call(
        &server.addr,
        &id.pin,
        "GET",
        "/v1/status",
        None,
        None,
        timeouts(),
    )
    .await
    .unwrap_err();
    // The pin matched. The handshake failed for the protocol, which is no pin failure.
    assert_eq!(e.kind, ErrorKind::Unreachable);
    assert_eq!(server.bytes_read.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn a_closed_port_is_unreachable() {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let e = pinned_call(
        &format!("127.0.0.1:{port}"),
        &[0; 32],
        "GET",
        "/v1/status",
        None,
        None,
        timeouts(),
    )
    .await
    .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Unreachable);
    assert!(!e.message.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn a_silent_server_times_out_as_unreachable() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        // Accept and say nothing.
        let mut held = Vec::new();
        while let Ok(c) = listener.accept().await {
            held.push(c);
        }
    });
    let t = Timeouts {
        connect: Duration::from_millis(300),
        request: Duration::from_millis(300),
    };
    let e = pinned_call(&addr, &[0; 32], "GET", "/v1/status", None, None, t)
        .await
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Unreachable);
}

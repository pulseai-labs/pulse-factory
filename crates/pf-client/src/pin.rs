//! The pin check (ADR-0012, `docs/contracts/api.md` "The pin check").
//!
//! The server's certificate is self-signed, so a chain says nothing. The client pins the key at
//! pairing: the SHA-256 of the DER SubjectPublicKeyInfo (SPKI) of the end-entity certificate. A
//! [`PinVerifier`] ignores the chain, the host name and the clock, takes the end-entity certificate,
//! hashes its SPKI, and fails the handshake unless that hash is the pin. It still checks the
//! handshake signature against the certificate's key, so the peer must hold the pinned key. TLS 1.3
//! only. The handshake fails on the client before any HTTP byte is written.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

use crate::error::{ClientError, ErrorKind};

/// A DER element: its tag, the length of its header and the length of its content. `None` when the
/// bytes end early or the length is not in the short or long definite form of at most 4 bytes.
fn tlv(input: &[u8]) -> Option<(u8, usize, usize)> {
    let tag = *input.first()?;
    let first = *input.get(1)?;
    let (header, len) = if first < 0x80 {
        (2usize, usize::from(first))
    } else {
        let n = usize::from(first & 0x7f);
        if n == 0 || n > 4 {
            return None;
        }
        let bytes = input.get(2..2 + n)?;
        let len = bytes
            .iter()
            .fold(0usize, |acc, b| (acc << 8) | usize::from(*b));
        (2 + n, len)
    };
    if input.len() < header.checked_add(len)? {
        return None;
    }
    Some((tag, header, len))
}

/// The DER of the SubjectPublicKeyInfo of an X.509 certificate, the whole element.
fn spki_der(cert: &[u8]) -> Option<&[u8]> {
    let (tag, h, l) = tlv(cert)?;
    if tag != 0x30 {
        return None;
    }
    let body = &cert[h..h + l];
    let (tag, h, l) = tlv(body)?;
    if tag != 0x30 {
        return None;
    }
    let mut tbs = &body[h..h + l];
    // TBSCertificate: [0] version (optional), serial, signature, issuer, validity, subject, SPKI.
    if tlv(tbs)?.0 == 0xA0 {
        let (_, h, l) = tlv(tbs)?;
        tbs = &tbs[h + l..];
    }
    for _ in 0..5 {
        let (_, h, l) = tlv(tbs)?;
        tbs = &tbs[h + l..];
    }
    let (tag, h, l) = tlv(tbs)?;
    if tag != 0x30 {
        return None;
    }
    Some(&tbs[..h + l])
}

/// The SHA-256 of the SPKI of a DER certificate, or `None` when the bytes are not a certificate.
pub fn spki_sha256(cert_der: &[u8]) -> Option<[u8; 32]> {
    let spki = spki_der(cert_der)?;
    let hex = pf_engine::claim::sha256_hex(spki);
    <[u8; 32]>::try_from(pf_api::code::unhex(&hex)?.as_slice()).ok()
}

/// The 32 bytes of a pin written as 64 lower-case hex digits.
pub fn parse_pin(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    <[u8; 32]>::try_from(pf_api::code::unhex(hex)?.as_slice()).ok()
}

/// The certificate verifier of a pinned connection.
#[derive(Debug)]
pub struct PinVerifier {
    pin: [u8; 32],
    provider: Arc<CryptoProvider>,
    mismatch: AtomicBool,
}

impl PinVerifier {
    pub fn new(pin: [u8; 32]) -> PinVerifier {
        PinVerifier {
            pin,
            provider: Arc::new(rustls::crypto::ring::default_provider()),
            mismatch: AtomicBool::new(false),
        }
    }

    /// Whether this verifier refused a certificate for its key. The caller reads it after a failed
    /// handshake to tell `key_changed` from `unreachable`.
    pub fn mismatch(&self) -> bool {
        self.mismatch.load(Ordering::SeqCst)
    }
}

impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        match spki_sha256(end_entity) {
            Some(hash) if hash == self.pin => Ok(ServerCertVerified::assertion()),
            _ => {
                self.mismatch.store(true, Ordering::SeqCst);
                Err(rustls::Error::General(
                    "the server key is not the pinned one".into(),
                ))
            }
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 is not used".into()))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// The client configuration of a pinned connection: TLS 1.3 only, this verifier, no client
/// certificate.
pub fn client_config(verifier: Arc<PinVerifier>) -> Result<Arc<rustls::ClientConfig>, ClientError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|_| ClientError::new(ErrorKind::Server, "cannot build the TLS configuration"))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

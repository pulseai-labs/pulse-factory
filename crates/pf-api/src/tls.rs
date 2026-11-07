//! The server key and its self-signed certificate (ADR-0012).
//!
//! On the first start with the API on, the controller makes an ECDSA P-256 key and a self-signed
//! certificate with `rcgen` in `<state dir>/api/`: `key.pem` (mode 0600) and `cert.pem`. Later starts
//! load them. The certificate names the listen address. The SHA-256 of the key's SubjectPublicKeyInfo
//! (the SPKI hash) is what a client pins at pairing. A client that checks the pin ignores the
//! certificate chain, so a self-signed certificate is enough.
//!
//! A new key changes the SPKI hash, and every paired client then fails closed. That is the key loss
//! rule: the operator revokes the old devices and pairs again.

use std::io::Write;
use std::net::SocketAddr;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rcgen::{
    CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, KeyPair, PublicKeyData,
};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

use crate::code::{hex, unhex};

/// The key file's name in the API directory.
pub const KEY_FILE: &str = "key.pem";

/// The certificate file's name in the API directory.
pub const CERT_FILE: &str = "cert.pem";

/// The API directory of a state directory.
pub fn api_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("api")
}

/// The server's key, its certificate and the hash that clients pin.
pub struct ServerKey {
    cert_der: Vec<u8>,
    key_pkcs8_der: Vec<u8>,
    spki_sha256: [u8; 32],
}

// The private key never shows in a debug print.
impl std::fmt::Debug for ServerKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerKey")
            .field("key", &"<hidden>")
            .field("spki_sha256", &self.spki_sha256_hex())
            .finish()
    }
}

impl ServerKey {
    /// The SPKI hash as 64 lower-case hex digits.
    pub fn spki_sha256_hex(&self) -> String {
        hex(&self.spki_sha256)
    }

    /// The SPKI hash as bytes.
    pub fn spki_sha256(&self) -> [u8; 32] {
        self.spki_sha256
    }

    /// The rustls configuration of the listener: TLS 1.3, no client certificate, this key.
    pub fn server_config(&self) -> Result<Arc<rustls::ServerConfig>, String> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key_pkcs8_der.clone()));
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|e| format!("cannot build the TLS configuration: {e}"))?
            .with_no_client_auth()
            .with_single_cert(vec![CertificateDer::from(self.cert_der.clone())], key)
            .map_err(|e| format!("the key and the certificate do not fit: {e}"))?;
        Ok(Arc::new(config))
    }
}

fn spki_hash(key: &KeyPair) -> Result<[u8; 32], String> {
    let spki = key.subject_public_key_info();
    let hash = pf_engine::claim::sha256_hex(&spki);
    let bytes = unhex(&hash).ok_or("the SPKI hash is not hex")?;
    <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| "the SPKI hash is not 32 bytes".to_string())
}

/// Write a file that is made now, with this mode, through a temporary file of the same mode, so it
/// is never readable by others, not even for a moment.
fn write_new(path: &Path, mode: u32, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let io = |what: &str, e: std::io::Error| format!("cannot {what} {}: {e}", path.display());
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&tmp)
        .map_err(|e| io("write", e))?;
    file.write_all(bytes).map_err(|e| io("write", e))?;
    file.sync_all().map_err(|e| io("sync", e))?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))
        .map_err(|e| io("set the mode of", e))?;
    std::fs::rename(&tmp, path).map_err(|e| io("replace", e))
}

fn make_cert(key: &KeyPair, listen: &SocketAddr) -> Result<(String, Vec<u8>), String> {
    let mut params = CertificateParams::new(vec![listen.ip().to_string()])
        .map_err(|e| format!("cannot describe the certificate: {e}"))?;
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, "pulse-factory controller");
    params.distinguished_name = name;
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let cert = params
        .self_signed(key)
        .map_err(|e| format!("cannot sign the certificate: {e}"))?;
    Ok((cert.pem(), cert.der().to_vec()))
}

/// Load the key and the certificate from `dir`, or make them. The directory is made with mode 0700.
///
/// - Both files present: load them. A file that cannot be read is an error. It is never replaced,
///   because the key may be the only copy.
/// - Neither present: make a key and a certificate.
/// - The key present and the certificate missing: sign a new certificate with the same key, so the
///   SPKI hash stays and the paired clients keep working.
/// - The certificate present and the key missing: make a new key and a new certificate.
pub fn load_or_create(dir: &Path, listen: &SocketAddr) -> Result<ServerKey, String> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
    let key_path = dir.join(KEY_FILE);
    let cert_path = dir.join(CERT_FILE);
    let (key, key_is_new) = if key_path.exists() {
        let pem = std::fs::read_to_string(&key_path)
            .map_err(|e| format!("cannot read {}: {e}", key_path.display()))?;
        let key = KeyPair::from_pem(&pem)
            .map_err(|_| format!("{} is not a usable key", key_path.display()))?;
        (key, false)
    } else {
        let key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
            .map_err(|e| format!("cannot make the key: {e}"))?;
        write_new(&key_path, 0o600, key.serialize_pem().as_bytes())?;
        (key, true)
    };
    // A certificate that an older key signed is not loaded: a new key gets a new certificate.
    let cert_der = if !key_is_new && cert_path.exists() {
        let pem = std::fs::read(&cert_path)
            .map_err(|e| format!("cannot read {}: {e}", cert_path.display()))?;
        CertificateDer::from_pem_slice(&pem)
            .map_err(|_| format!("{} is not a usable certificate", cert_path.display()))?
            .to_vec()
    } else {
        let (pem, der) = make_cert(&key, listen)?;
        write_new(&cert_path, 0o644, pem.as_bytes())?;
        der
    };
    // A certificate that does not fit the key fails here, in `server_config`, and not in a handshake.
    let server_key = ServerKey {
        cert_der,
        key_pkcs8_der: key.serialize_der(),
        spki_sha256: spki_hash(&key)?,
    };
    server_key.server_config()?;
    Ok(server_key)
}

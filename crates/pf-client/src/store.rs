//! The device store: the paired server, its pin, the device record and the token.
//!
//! [`FileStore`] keeps everything in one JSON file of mode 0600, for the bridge and the tests.
//! `KeychainStore` (macOS only) keeps the token in the login Keychain and the rest in a file that
//! holds no secret.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{ClientError, ErrorKind};
use pf_api::types::DeviceRecord;

/// The paired server. `pin` is the SPKI SHA-256 as 64 lower-case hex digits.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Paired {
    pub address: String,
    pub pin: String,
    pub token: String,
    pub device: DeviceRecord,
}

// The token never shows in a debug print.
impl std::fmt::Debug for Paired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Paired")
            .field("address", &self.address)
            .field("pin", &self.pin)
            .field("token", &"<hidden>")
            .field("device", &self.device)
            .finish()
    }
}

impl Paired {
    /// Whether the fields have the documented shape: a pin and a token of 64 hex digits.
    pub fn is_valid(&self) -> bool {
        is_hex64(&self.pin) && is_hex64(&self.token) && !self.address.is_empty()
    }
}

fn is_hex64(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Where a device keeps its pairing. A call that reads or writes the store never puts a secret in
/// an error message. The calls are synchronous and may block on files or the Keychain, so the
/// transport runs each one on the blocking pool.
pub trait DeviceStore: Send + Sync + 'static {
    /// The paired server, or `None` when this device is not paired.
    fn load(&self) -> Result<Option<Paired>, ClientError>;
    /// Keep a pairing, replacing the old one.
    fn save(&self, paired: &Paired) -> Result<(), ClientError>;
    /// Drop the pairing. Safe when there is none.
    fn forget(&self) -> Result<(), ClientError>;
}

pub(crate) fn store_error(what: &str) -> ClientError {
    ClientError::new(ErrorKind::Server, what)
}

/// Write `bytes` to `path` with mode 0600 through a temporary file in the same directory, then a
/// rename. The directory is made with mode 0700 when it is missing.
pub(crate) fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<(), ClientError> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    let fail = |what: &str| store_error(&format!("cannot {what} the device file"));
    let dir = path.parent().ok_or_else(|| fail("place"))?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|_| fail("make the directory of"))?;
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|_| fail("write"))?;
    file.write_all(bytes).map_err(|_| fail("write"))?;
    file.sync_all().map_err(|_| fail("sync"))?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
        .map_err(|_| fail("protect"))?;
    std::fs::rename(&tmp, path).map_err(|_| fail("replace"))
}

/// One JSON file of mode 0600.
#[derive(Clone, Debug)]
pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    pub fn new(path: PathBuf) -> FileStore {
        FileStore { path }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl DeviceStore for FileStore {
    fn load(&self) -> Result<Option<Paired>, ClientError> {
        let text = match std::fs::read(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(store_error("the device file cannot be read")),
        };
        // The parse error of serde_json can quote the text, so it is dropped.
        let paired: Paired = serde_json::from_slice(&text)
            .map_err(|_| store_error("the device file is not valid"))?;
        if !paired.is_valid() {
            return Err(store_error("the device file is not valid"));
        }
        Ok(Some(paired))
    }

    fn save(&self, paired: &Paired) -> Result<(), ClientError> {
        let bytes =
            serde_json::to_vec(paired).map_err(|_| store_error("the device cannot be written"))?;
        write_private(&self.path, &bytes)
    }

    fn forget(&self) -> Result<(), ClientError> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(store_error("the device file cannot be removed")),
        }
    }
}

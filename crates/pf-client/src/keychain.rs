//! The macOS Keychain store (`r1.s2.w4`, grill A5).
//!
//! The token is a generic password in the login Keychain: service `pulse-factory.device-token`,
//! account = the server address. The address, the pin and the device record are in a JSON file in
//! the app's data directory. That file holds no secret. `forget` deletes both.
//!
//! This module is compiled on macOS only. The Linux build never sees it, and its tests run in the
//! `macos-app` job.

use std::path::PathBuf;

use pf_api::types::DeviceRecord;
use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};
use serde::{Deserialize, Serialize};

use crate::error::ClientError;
use crate::store::{DeviceStore, Paired, store_error, write_private};

/// The Keychain service of the device token.
pub const KEYCHAIN_SERVICE: &str = "pulse-factory.device-token";

/// The file in the data directory that holds the pairing without its token.
pub const PAIRING_FILE: &str = "pairing.json";

/// `errSecItemNotFound`.
const ITEM_NOT_FOUND: i32 = -25300;

/// The part of a pairing that is no secret.
#[derive(Serialize, Deserialize)]
struct Public {
    address: String,
    pin: String,
    device: DeviceRecord,
}

#[derive(Clone, Debug)]
pub struct KeychainStore {
    file: PathBuf,
    service: String,
}

impl KeychainStore {
    /// The store of the app: the real service name, and `<data_dir>/pairing.json`.
    pub fn new(data_dir: PathBuf) -> KeychainStore {
        KeychainStore::with_service(data_dir, KEYCHAIN_SERVICE)
    }

    /// A store under another service name, for the tests, which delete their item again.
    pub fn with_service(data_dir: PathBuf, service: &str) -> KeychainStore {
        KeychainStore {
            file: data_dir.join(PAIRING_FILE),
            service: service.to_string(),
        }
    }

    fn read_public(&self) -> Result<Option<Public>, ClientError> {
        let text = match std::fs::read(&self.file) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(store_error("the pairing file cannot be read")),
        };
        serde_json::from_slice(&text)
            .map(Some)
            .map_err(|_| store_error("the pairing file is not valid"))
    }

    fn delete_token(&self, account: &str) -> Result<(), ClientError> {
        match delete_generic_password(&self.service, account) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == ITEM_NOT_FOUND => Ok(()),
            Err(_) => Err(store_error("the Keychain item cannot be deleted")),
        }
    }
}

impl DeviceStore for KeychainStore {
    fn load(&self) -> Result<Option<Paired>, ClientError> {
        let Some(public) = self.read_public()? else {
            return Ok(None);
        };
        let token = match get_generic_password(&self.service, &public.address) {
            Ok(bytes) => String::from_utf8(bytes)
                .map_err(|_| store_error("the Keychain item is not valid"))?,
            // A pairing file with no token behind it is no pairing.
            Err(e) if e.code() == ITEM_NOT_FOUND => return Ok(None),
            Err(_) => return Err(store_error("the Keychain cannot be read")),
        };
        let paired = Paired {
            address: public.address,
            pin: public.pin,
            token,
            device: public.device,
        };
        if !paired.is_valid() {
            return Err(store_error("the pairing is not valid"));
        }
        Ok(Some(paired))
    }

    fn save(&self, paired: &Paired) -> Result<(), ClientError> {
        // A pairing with another server leaves no token behind for the old one.
        if let Some(old) = self.read_public()?
            && old.address != paired.address
        {
            self.delete_token(&old.address)?;
        }
        set_generic_password(&self.service, &paired.address, paired.token.as_bytes())
            .map_err(|_| store_error("the Keychain cannot be written"))?;
        let public = Public {
            address: paired.address.clone(),
            pin: paired.pin.clone(),
            device: paired.device.clone(),
        };
        let bytes = serde_json::to_vec(&public)
            .map_err(|_| store_error("the pairing cannot be written"))?;
        write_private(&self.file, &bytes)
    }

    fn forget(&self) -> Result<(), ClientError> {
        if let Some(public) = self.read_public()? {
            self.delete_token(&public.address)?;
        }
        match std::fs::remove_file(&self.file) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(store_error("the pairing file cannot be removed")),
        }
    }
}

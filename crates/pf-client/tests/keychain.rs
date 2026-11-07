//! AC-1 of `r1.s2.w4`, on macOS only (the `macos-app` job): the Keychain store keeps a token under a
//! test service name, reads it back, and `forget` deletes it again.
#![cfg(target_os = "macos")]

use pf_client::keychain::KeychainStore;
use pf_client::store::{DeviceStore, Paired};

fn paired(address: &str) -> Paired {
    Paired {
        address: address.into(),
        pin: "ab".repeat(32),
        token: "cd".repeat(32),
        device: serde_json::from_value(serde_json::json!({
            "id": "D-1", "name": "macbook", "paired_at": "2026-11-08T00:00:00Z",
            "last_seen_at": null, "last_peer": null, "revoked_at": null
        }))
        .unwrap(),
    }
}

fn service() -> String {
    format!("pulse-factory.device-token.test-{}", std::process::id())
}

#[test]
fn a_token_goes_to_the_keychain_and_comes_back_and_forget_deletes_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = KeychainStore::with_service(dir.path().to_path_buf(), &service());
    store.forget().unwrap();
    assert!(store.load().unwrap().is_none());

    store.save(&paired("127.0.0.1:7443")).unwrap();
    assert_eq!(store.load().unwrap(), Some(paired("127.0.0.1:7443")));
    // The file holds no token.
    let file = std::fs::read_to_string(dir.path().join("pairing.json")).unwrap();
    assert!(!file.contains(&"cd".repeat(32)), "{file}");

    store.forget().unwrap();
    assert!(store.load().unwrap().is_none());
    assert!(!dir.path().join("pairing.json").exists());
    // The item is gone from the Keychain too: a second store over the same service finds none.
    let again = KeychainStore::with_service(dir.path().to_path_buf(), &service());
    assert!(again.load().unwrap().is_none());
}

#[test]
fn pairing_with_another_server_deletes_the_old_token() {
    let dir = tempfile::tempdir().unwrap();
    let svc = format!("{}-b", service());
    let store = KeychainStore::with_service(dir.path().to_path_buf(), &svc);
    store.save(&paired("127.0.0.1:1")).unwrap();
    store.save(&paired("127.0.0.1:2")).unwrap();
    assert_eq!(store.load().unwrap().unwrap().address, "127.0.0.1:2");
    store.forget().unwrap();
    assert!(store.load().unwrap().is_none());
}

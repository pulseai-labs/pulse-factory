//! AC-1 of `r1.s2.w4`: `FileStore` writes mode 0600 by a rename, and `forget` removes the file.

use std::os::unix::fs::PermissionsExt;

use pf_client::store::{DeviceStore, FileStore, Paired};

fn paired() -> Paired {
    Paired {
        address: "127.0.0.1:7443".into(),
        pin: "ab".repeat(32),
        token: "cd".repeat(32),
        device: serde_json::from_value(serde_json::json!({
            "id": "D-1", "name": "macbook", "paired_at": "2026-11-08T00:00:00Z",
            "last_seen_at": null, "last_peer": null, "revoked_at": null
        }))
        .unwrap(),
    }
}

#[test]
fn an_empty_store_holds_no_device() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileStore::new(dir.path().join("device.json"));
    assert!(store.load().unwrap().is_none());
}

#[test]
fn save_writes_mode_0600_and_load_reads_it_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state").join("device.json");
    let store = FileStore::new(path.clone());
    store.save(&paired()).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert_eq!(store.load().unwrap(), Some(paired()));
    // No temporary file stays behind.
    let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["device.json".to_string()]);
}

#[test]
fn save_replaces_the_file_by_a_rename() {
    use std::os::unix::fs::MetadataExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("device.json");
    let store = FileStore::new(path.clone());
    store.save(&paired()).unwrap();
    let first = std::fs::metadata(&path).unwrap().ino();
    let mut next = paired();
    next.address = "127.0.0.1:9".into();
    store.save(&next).unwrap();
    let second = std::fs::metadata(&path).unwrap().ino();
    assert_ne!(first, second, "a rename puts a new file in place");
    assert_eq!(store.load().unwrap().unwrap().address, "127.0.0.1:9");
}

#[test]
fn save_tightens_a_loose_old_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("device.json");
    std::fs::write(&path, "{}").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    FileStore::new(path.clone()).save(&paired()).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn forget_removes_the_file_and_is_safe_twice() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("device.json");
    let store = FileStore::new(path.clone());
    store.save(&paired()).unwrap();
    store.forget().unwrap();
    assert!(!path.exists());
    assert!(store.load().unwrap().is_none());
    store.forget().unwrap();
}

#[test]
fn a_broken_file_is_a_server_error_that_names_no_secret() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("device.json");
    std::fs::write(&path, format!("{{\"token\":\"{}\"", "ef".repeat(32))).unwrap();
    let e = FileStore::new(path).load().unwrap_err();
    assert_eq!(e.kind, pf_client::error::ErrorKind::Server);
    assert!(!e.message.contains(&"ef".repeat(32)));
}

#[test]
fn debug_of_a_paired_device_hides_the_token() {
    let text = format!("{:?}", paired());
    assert!(!text.contains(&"cd".repeat(32)), "{text}");
}

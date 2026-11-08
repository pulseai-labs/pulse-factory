//! Correction of `r1.s2.w4` (rule: blocking file work stays off the async control path).
//!
//! The device store is synchronous and does file work (the Keychain store adds Keychain calls).
//! `Transport` must call it on the blocking pool, never on the thread that polls the async task.
//! The runtime here is `current_thread`, so the thread that runs the test is the one that polls
//! the transport. A store call on that thread is a store call on the async path.

use std::sync::{Arc, Mutex};
use std::thread::ThreadId;
use std::time::Duration;

use pf_client::error::{ClientError, ErrorKind};
use pf_client::http::Timeouts;
use pf_client::store::{DeviceStore, FileStore, Paired};
use pf_client::transport::Transport;

/// A store that records the thread of each call, and does the real work in a `FileStore`.
struct Recording {
    inner: FileStore,
    calls: Mutex<Vec<(&'static str, ThreadId)>>,
}

impl Recording {
    fn new(dir: &std::path::Path) -> Arc<Recording> {
        Arc::new(Recording {
            inner: FileStore::new(dir.join("device.json")),
            calls: Mutex::new(Vec::new()),
        })
    }

    fn note(&self, what: &'static str) {
        self.calls
            .lock()
            .unwrap()
            .push((what, std::thread::current().id()));
    }

    fn calls(&self) -> Vec<(&'static str, ThreadId)> {
        self.calls.lock().unwrap().clone()
    }
}

impl DeviceStore for Recording {
    fn load(&self) -> Result<Option<Paired>, ClientError> {
        self.note("load");
        self.inner.load()
    }
    fn save(&self, paired: &Paired) -> Result<(), ClientError> {
        self.note("save");
        self.inner.save(paired)
    }
    fn forget(&self) -> Result<(), ClientError> {
        self.note("forget");
        self.inner.forget()
    }
}

fn paired() -> Paired {
    Paired {
        // Nothing listens here, so a call that gets past the load fails at the connect.
        address: "127.0.0.1:1".into(),
        pin: "ab".repeat(32),
        token: "cd".repeat(32),
        device: serde_json::from_value(serde_json::json!({
            "id": "D-1", "name": "macbook", "paired_at": "2026-11-08T00:00:00Z",
            "last_seen_at": null, "last_peer": null, "revoked_at": null
        }))
        .unwrap(),
    }
}

fn transport(store: Arc<Recording>) -> Transport<Recording> {
    Transport::new(store).with_timeouts(Timeouts {
        connect: Duration::from_secs(2),
        request: Duration::from_secs(2),
    })
}

fn assert_off_this_thread(store: &Recording, expected: &[&str]) {
    let here = std::thread::current().id();
    let calls = store.calls();
    let names: Vec<&str> = calls.iter().map(|(n, _)| *n).collect();
    assert_eq!(names, expected, "the store calls that the transport made");
    for (name, thread) in calls {
        assert_ne!(thread, here, "{name} ran on the async thread");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn paired_reads_the_store_off_the_async_thread() {
    let dir = tempfile::tempdir().unwrap();
    let store = Recording::new(dir.path());
    store.inner.save(&paired()).unwrap();
    let t = transport(store.clone());
    let server = t.paired().await.unwrap().expect("a paired device");
    assert_eq!(server.address, "127.0.0.1:1");
    assert_off_this_thread(&store, &["load"]);
}

#[tokio::test(flavor = "current_thread")]
async fn a_token_call_reads_the_store_off_the_async_thread() {
    let dir = tempfile::tempdir().unwrap();
    let store = Recording::new(dir.path());
    store.inner.save(&paired()).unwrap();
    let t = transport(store.clone());
    // The load works; the connect to a closed port then fails.
    let err = t.status().await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unreachable, "{err}");
    assert_off_this_thread(&store, &["load"]);
}

#[tokio::test(flavor = "current_thread")]
async fn a_token_call_with_no_pairing_is_unauthorized() {
    let dir = tempfile::tempdir().unwrap();
    let store = Recording::new(dir.path());
    let t = transport(store.clone());
    let err = t.project_list().await.unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unauthorized);
    assert_off_this_thread(&store, &["load"]);
}

#[tokio::test(flavor = "current_thread")]
async fn forget_drops_the_pairing_off_the_async_thread() {
    let dir = tempfile::tempdir().unwrap();
    let store = Recording::new(dir.path());
    store.inner.save(&paired()).unwrap();
    let t = transport(store.clone());
    t.forget().await.unwrap();
    assert_off_this_thread(&store, &["forget"]);
    assert!(store.inner.load().unwrap().is_none());
}

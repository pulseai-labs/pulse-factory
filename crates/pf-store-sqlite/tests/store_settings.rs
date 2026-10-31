//! AC-14: the store opens with WAL and `synchronous=FULL`, read back through `PRAGMA`.

use pf_store_sqlite::SqliteStore;

#[test]
fn the_store_opens_with_wal_and_synchronous_full() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.db");
    let store = SqliteStore::open(&path).unwrap();

    // Read back through PRAGMA, on the writer's own connection.
    let settings = store.settings().unwrap();
    assert_eq!(settings.journal_mode, "wal");
    assert_eq!(settings.synchronous, "FULL");
    assert_eq!(store.pragma("journal_mode").unwrap(), "wal");
    assert_eq!(store.pragma("synchronous").unwrap(), "2", "2 is FULL");

    // The journal mode is a property of the file: a second connection reads it too.
    let other = rusqlite::Connection::open(&path).unwrap();
    let mode: String = other
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
}

#[test]
fn a_second_open_of_the_same_file_keeps_the_settings() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run.db");
    drop(SqliteStore::open(&path).unwrap());
    let again = SqliteStore::open(&path).unwrap();
    let settings = again.settings().unwrap();
    assert_eq!(settings.journal_mode, "wal");
    assert_eq!(settings.synchronous, "FULL");
}

#[test]
fn the_store_is_shared_by_threads_through_one_writer() {
    let dir = tempfile::tempdir().unwrap();
    let store = std::sync::Arc::new(SqliteStore::open(dir.path().join("run.db")).unwrap());
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let store = store.clone();
            std::thread::spawn(move || store.settings().unwrap().synchronous)
        })
        .collect();
    for h in handles {
        assert_eq!(h.join().unwrap(), "FULL");
    }
}

#[test]
fn a_path_that_cannot_be_opened_is_an_error_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let err = SqliteStore::open(dir.path().join("no-such-dir").join("run.db"))
        .err()
        .expect("opening in a missing directory fails");
    assert!(!err.to_string().is_empty());
}

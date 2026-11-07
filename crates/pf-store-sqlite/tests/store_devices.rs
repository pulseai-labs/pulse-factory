//! Devices on the store port, on the real SQLite store (ADR-0003, ADR-0012). A device is a
//! controller entity: pairing and revoking commit under the scope `@controller`, and the store gives
//! the device the next `D-<n>`, never reused. Last-seen is telemetry: `touch_device` writes it with
//! no admission and no ledger line.

use pf_engine::port::store::{
    AdmissionOutcome, CONTROLLER_SCOPE, Commit, Committed, Effect, LedgerEntry, NEW_ID,
    NewAdmission, NewDevice, Store,
};
use pf_store_sqlite::SqliteStore;

fn open() -> (tempfile::TempDir, SqliteStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("store.sqlite")).unwrap();
    (dir, store)
}

fn admission(kind: &str, unit: &str) -> NewAdmission {
    NewAdmission {
        kind: kind.to_string(),
        outcome: AdmissionOutcome::Admitted,
        reason: None,
        unit_id: Some(unit.to_string()),
        role: None,
        model: None,
        attempt: None,
        admitted_at: "t1".to_string(),
    }
}

fn token(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}

fn add_commit(command_id: &str, name: &str, token_sha256: &str) -> Commit {
    Commit {
        run_id: CONTROLLER_SCOPE.to_string(),
        command_id: command_id.to_string(),
        content: format!("{{\"device_name\":\"{name}\"}}"),
        admission: admission("device_pair", NEW_ID),
        effects: vec![Effect::AddDevice(NewDevice {
            name: name.to_string(),
            token_sha256: token_sha256.to_string(),
            paired_at: "t1".to_string(),
        })],
        ledger: Some(LedgerEntry {
            ledger_file: "controller.ledger".to_string(),
            body: format!("device_pair {NEW_ID} admitted name={name} peer=127.0.0.1"),
        }),
    }
}

fn revoke_commit(command_id: &str, id: &str) -> Commit {
    Commit {
        run_id: CONTROLLER_SCOPE.to_string(),
        command_id: command_id.to_string(),
        content: format!("{{\"id\":\"{id}\"}}"),
        admission: admission("device_revoke", id),
        effects: vec![Effect::RevokeDevice {
            id: id.to_string(),
            revoked_at: "t2".to_string(),
        }],
        ledger: Some(LedgerEntry {
            ledger_file: "controller.ledger".to_string(),
            body: format!("device_revoke {id} admitted command={command_id}"),
        }),
    }
}

/// The id the store gave: the commit's admission names it in `unit_id`.
fn applied_id(c: Committed) -> String {
    match c {
        Committed::Applied { admission, .. } => admission.unit_id.expect("the assigned id"),
        other => panic!("not applied: {other:?}"),
    }
}

fn rejected(c: Committed) -> String {
    match c {
        Committed::Rejected(why) => why,
        other => panic!("not rejected: {other:?}"),
    }
}

fn admission_count(store: &SqliteStore) -> usize {
    store.admissions(CONTROLLER_SCOPE).unwrap().len()
}

fn outbox_count(store: &SqliteStore) -> usize {
    store
        .unwritten_ledger_lines("controller.ledger")
        .unwrap()
        .len()
}

#[test]
fn adds_give_d1_then_d2_in_order() {
    let (_d, store) = open();
    assert_eq!(
        applied_id(store.commit(add_commit("c1", "mac", &token(1))).unwrap()),
        "D-1"
    );
    assert_eq!(
        applied_id(store.commit(add_commit("c2", "pad", &token(2))).unwrap()),
        "D-2"
    );
    let all = store.devices().unwrap();
    let ids: Vec<&str> = all.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(ids, ["D-1", "D-2"]);
    assert_eq!(all[0].name, "mac");
    assert_eq!(all[0].paired_at, "t1");
    assert_eq!(all[0].last_seen_at, None);
    assert_eq!(all[0].last_peer, None);
    assert_eq!(all[0].revoked_at, None);
}

#[test]
fn the_admission_and_the_ledger_line_name_the_assigned_id() {
    let (_d, store) = open();
    let Committed::Applied { admission, ledger } =
        store.commit(add_commit("c1", "mac", &token(1))).unwrap()
    else {
        panic!("not applied")
    };
    assert_eq!(admission.unit_id.as_deref(), Some("D-1"));
    assert_eq!(admission.run_id, CONTROLLER_SCOPE);
    assert_eq!(admission.kind, "device_pair");
    let line = ledger.expect("a ledger line");
    assert_eq!(
        line.body,
        "device_pair D-1 admitted name=mac peer=127.0.0.1"
    );
    assert!(!line.body.contains(NEW_ID));
    // The token hash is in the device row only: no admission field and no ledger line holds it.
    assert!(!admission.content.contains(&token(1)));
    assert!(!line.body.contains(&token(1)));
}

#[test]
fn an_id_is_never_reused_after_a_revoke() {
    let (_d, store) = open();
    store.commit(add_commit("c1", "mac", &token(1))).unwrap();
    store.commit(add_commit("c2", "pad", &token(2))).unwrap();
    store.commit(revoke_commit("c3", "D-2")).unwrap();
    // The last row is revoked, and the next add is still D-3.
    assert_eq!(
        applied_id(store.commit(add_commit("c4", "pad", &token(3))).unwrap()),
        "D-3"
    );
}

#[test]
fn a_revoke_sets_revoked_at_and_keeps_the_row() {
    let (_d, store) = open();
    store.commit(add_commit("c1", "mac", &token(1))).unwrap();
    let c = store.commit(revoke_commit("c2", "D-1")).unwrap();
    assert!(matches!(c, Committed::Applied { .. }), "{c:?}");
    let all = store.devices().unwrap();
    assert_eq!(all.len(), 1, "the row stays");
    assert_eq!(all[0].revoked_at.as_deref(), Some("t2"));
    assert_eq!(all[0].name, "mac");
}

#[test]
fn a_second_revoke_and_an_unknown_id_are_refusals_that_write_nothing() {
    let (_d, store) = open();
    store.commit(add_commit("c1", "mac", &token(1))).unwrap();
    store.commit(revoke_commit("c2", "D-1")).unwrap();
    let before = (admission_count(&store), outbox_count(&store));

    let why = rejected(store.commit(revoke_commit("c3", "D-1")).unwrap());
    assert!(why.contains("D-1") && why.contains("revoked"), "{why}");
    let why = rejected(store.commit(revoke_commit("c4", "D-9")).unwrap());
    assert!(why.contains("D-9"), "{why}");

    assert_eq!(
        (admission_count(&store), outbox_count(&store)),
        before,
        "a refused commit writes no admission and no outbox line"
    );
    assert_eq!(
        store.devices().unwrap()[0].revoked_at.as_deref(),
        Some("t2")
    );
}

#[test]
fn a_token_hash_that_is_taken_is_a_refusal_even_for_a_revoked_device() {
    let (_d, store) = open();
    store.commit(add_commit("c1", "mac", &token(1))).unwrap();
    let why = rejected(store.commit(add_commit("c2", "pad", &token(1))).unwrap());
    assert!(why.contains("token"), "{why}");
    store.commit(revoke_commit("c3", "D-1")).unwrap();
    rejected(store.commit(add_commit("c4", "pad", &token(1))).unwrap());
    assert_eq!(store.devices().unwrap().len(), 1);
}

#[test]
fn device_by_token_returns_a_live_device_and_nothing_for_a_revoked_or_unknown_one() {
    let (_d, store) = open();
    store.commit(add_commit("c1", "mac", &token(1))).unwrap();
    store.commit(add_commit("c2", "pad", &token(2))).unwrap();

    let live = store.device_by_token(&token(2)).unwrap().expect("D-2");
    assert_eq!(live.id, "D-2");
    assert_eq!(live.name, "pad");
    assert!(store.device_by_token(&token(9)).unwrap().is_none());
    assert!(store.device_by_token("").unwrap().is_none());

    store.commit(revoke_commit("c3", "D-2")).unwrap();
    assert!(store.device_by_token(&token(2)).unwrap().is_none());
    // The other device is untouched.
    assert_eq!(store.device_by_token(&token(1)).unwrap().unwrap().id, "D-1");
}

#[test]
fn devices_lists_live_rows_first_and_each_group_in_pairing_order() {
    let (_d, store) = open();
    for (i, name) in ["a", "b", "c", "d"].iter().enumerate() {
        let n = u8::try_from(i + 1).unwrap();
        store
            .commit(add_commit(&format!("c{n}"), name, &token(n)))
            .unwrap();
    }
    store.commit(revoke_commit("r1", "D-1")).unwrap();
    store.commit(revoke_commit("r3", "D-3")).unwrap();
    let ids: Vec<String> = store.devices().unwrap().into_iter().map(|d| d.id).collect();
    assert_eq!(ids, ["D-2", "D-4", "D-1", "D-3"]);
}

#[test]
fn touch_device_writes_last_seen_and_peer_with_no_admission_and_no_ledger_line() {
    let (_d, store) = open();
    store.commit(add_commit("c1", "mac", &token(1))).unwrap();
    let before = (admission_count(&store), outbox_count(&store));

    let wrote = store
        .touch_device("D-1", "2026-11-07T10:00:00.000Z", "100.64.0.7")
        .unwrap();
    assert!(wrote);
    let d = store.devices().unwrap().remove(0);
    assert_eq!(d.last_seen_at.as_deref(), Some("2026-11-07T10:00:00.000Z"));
    assert_eq!(d.last_peer.as_deref(), Some("100.64.0.7"));
    assert_eq!(
        (admission_count(&store), outbox_count(&store)),
        before,
        "a touch is no admission and no ledger line"
    );
    // The row the lookup by token returns carries the telemetry too.
    let found = store.device_by_token(&token(1)).unwrap().unwrap();
    assert_eq!(found.last_peer.as_deref(), Some("100.64.0.7"));
}

#[test]
fn touch_device_writes_at_most_once_a_minute_per_device() {
    let (_d, store) = open();
    store.commit(add_commit("c1", "mac", &token(1))).unwrap();
    store.commit(add_commit("c2", "pad", &token(2))).unwrap();

    assert!(
        store
            .touch_device("D-1", "2026-11-07T10:00:00.000Z", "10.0.0.1")
            .unwrap()
    );
    // Inside the minute: no write, so neither the time nor the peer moves.
    assert!(
        !store
            .touch_device("D-1", "2026-11-07T10:00:59.000Z", "10.0.0.2")
            .unwrap()
    );
    let d = store.devices().unwrap().remove(0);
    assert_eq!(d.last_seen_at.as_deref(), Some("2026-11-07T10:00:00.000Z"));
    assert_eq!(d.last_peer.as_deref(), Some("10.0.0.1"));

    // Another device has its own minute.
    assert!(
        store
            .touch_device("D-2", "2026-11-07T10:00:30.000Z", "10.0.0.3")
            .unwrap()
    );

    // At the minute, the write goes through.
    assert!(
        store
            .touch_device("D-1", "2026-11-07T10:01:00.000Z", "10.0.0.2")
            .unwrap()
    );
    let d = store.devices().unwrap().remove(0);
    assert_eq!(d.last_seen_at.as_deref(), Some("2026-11-07T10:01:00.000Z"));
    assert_eq!(d.last_peer.as_deref(), Some("10.0.0.2"));
}

#[test]
fn touch_device_writes_nothing_for_a_revoked_or_unknown_device() {
    let (_d, store) = open();
    store.commit(add_commit("c1", "mac", &token(1))).unwrap();
    store.commit(revoke_commit("c2", "D-1")).unwrap();
    assert!(
        !store
            .touch_device("D-1", "2026-11-07T10:00:00.000Z", "10.0.0.1")
            .unwrap()
    );
    assert!(
        !store
            .touch_device("D-7", "2026-11-07T10:00:00.000Z", "10.0.0.1")
            .unwrap()
    );
    assert_eq!(store.devices().unwrap()[0].last_seen_at, None);
}

//! Launch records and the host registry on the store port, on the real SQLite store (ADR-0003's
//! amendment of 2026-11-16; `r2.s2.w2`): `Effect::LaunchRecordWrite`, `Effect::HostStarted` and
//! `Effect::HostEnded` under `@controller`, and the four reads.

mod common;

use common::*;
use pf_engine::port::store::{
    AdmissionOutcome, Commit, Committed, Effect, Store, canonical_json, record_sha256,
};
use pf_store_sqlite::SqliteStore;

fn body() -> String {
    record_body(&hashed_secret())
}

/// Write revision `revision` of a chat's incarnation, under its own command ID.
fn write(store: &SqliteStore, session: &str, incarnation: u32, revision: u32) -> Committed {
    write_as(
        store,
        &format!("lr-{session}-{incarnation}-{revision}"),
        session,
        incarnation,
        revision,
    )
}

/// The same, under a command ID the caller names: a retry of a refused write needs a new ID, since
/// a command ID that was admitted is never admitted twice.
fn write_as(
    store: &SqliteStore,
    command: &str,
    session: &str,
    incarnation: u32,
    revision: u32,
) -> Committed {
    store
        .commit(record_commit(
            command,
            new_record("chat", session, incarnation, revision, &body()),
        ))
        .unwrap()
}

#[test]
fn a_record_writes_and_reads_back_by_session_incarnation_and_revision() {
    let (_d, store) = open();
    let (admission, ledger) = applied(write(&store, "S-1", 1, 1));
    assert_eq!(admission.kind, "launch_record_write");
    assert!(ledger.is_some());

    let row = store.launch_record("chat", "S-1", 1, 1).unwrap().unwrap();
    assert_eq!(row.session_kind, "chat");
    assert_eq!(row.session_id, "S-1");
    assert_eq!((row.incarnation, row.revision), (1, 1));
    assert_eq!(row.launcher, "detached");
    assert_eq!(row.host_unit, "pf-host-srv-S-1-1");
    assert_eq!(row.command_id, "lr-S-1-1-1");
    // The store keeps the canonical body, and its hash is the hash of that text.
    let value: serde_json::Value = serde_json::from_str(&row.record).unwrap();
    assert_eq!(row.record, canonical_json(&value));
    assert_eq!(row.record_sha256, record_sha256(&row.record));
    assert_eq!(value["resume"], "fresh");
    assert!(value.get("resume_key").is_none(), "no runtime session ID");

    assert_eq!(store.launch_record("chat", "S-1", 1, 2).unwrap(), None);
    assert_eq!(store.launch_record("chat", "S-2", 1, 1).unwrap(), None);
    assert_eq!(store.launch_record("seat", "S-1", 1, 1).unwrap(), None);
}

#[test]
fn the_newest_revision_wins() {
    let (_d, store) = open();
    assert_eq!(store.newest_launch_record("chat", "S-1", 1).unwrap(), None);
    applied(write(&store, "S-1", 1, 1));
    assert_eq!(
        store
            .newest_launch_record("chat", "S-1", 1)
            .unwrap()
            .unwrap()
            .revision,
        1
    );
    // A live change writes revision 2, with a body that differs (the hash differs too).
    let changed = record_body(&hashed_secret()).replace("\"fresh\"", "\"--resume\"");
    applied(
        store
            .commit(record_commit(
                "lr-live-change",
                new_record("chat", "S-1", 1, 2, &changed),
            ))
            .unwrap(),
    );
    let newest = store
        .newest_launch_record("chat", "S-1", 1)
        .unwrap()
        .unwrap();
    assert_eq!(newest.revision, 2);
    let first = store.launch_record("chat", "S-1", 1, 1).unwrap().unwrap();
    assert_ne!(newest.record_sha256, first.record_sha256);
    // Revision 1 is still there, as it was written.
    assert_eq!(
        first.record_sha256,
        new_record("chat", "S-1", 1, 1, &body()).record_sha256
    );
    // Another incarnation has its own newest revision.
    assert_eq!(store.newest_launch_record("chat", "S-1", 2).unwrap(), None);
}

#[test]
fn a_revision_and_an_incarnation_must_be_the_next_ones() {
    let (_d, store) = open();
    // Nothing is written yet: only incarnation 1, revision 1 is next.
    for (incarnation, revision) in [(1, 2), (2, 1), (3, 1), (2, 2)] {
        let why = rejected(write(&store, "S-1", incarnation, revision));
        assert!(
            why.contains("next"),
            "({incarnation}, {revision}) must name what is next: {why}"
        );
    }
    applied(write(&store, "S-1", 1, 1));
    // A second revision 1, a gap, and a revision that skips.
    for (incarnation, revision) in [(1, 1), (1, 3), (1, 4)] {
        let command = format!("again-{incarnation}-{revision}");
        let why = rejected(write_as(&store, &command, "S-1", incarnation, revision));
        assert!(why.contains("revision"), "{why}");
    }
    applied(write(&store, "S-1", 1, 2));
    // Revision 1 of incarnation 3 needs incarnation 2 first.
    let why = rejected(write(&store, "S-1", 3, 1));
    assert!(why.contains("incarnation"), "{why}");
    applied(write(&store, "S-1", 2, 1));
    applied(write(&store, "S-1", 3, 1));
    // Another session starts at incarnation 1 again.
    applied(write(&store, "S-2", 1, 1));
    // Nothing a refusal did stayed: the rows are exactly those that were applied.
    assert!(store.launch_record("chat", "S-1", 2, 2).unwrap().is_none());
    assert_eq!(
        store
            .newest_launch_record("chat", "S-1", 1)
            .unwrap()
            .unwrap()
            .revision,
        2
    );
}

#[test]
fn a_secret_name_whose_value_is_plain_text_is_refused() {
    let (_d, store) = open();
    for plain in [
        "hunter2",
        "sha256:abc",
        &format!("sha256:{}", "EF".repeat(32)),
        "ref:",
        "",
    ] {
        let why = rejected(
            store
                .commit(record_commit(
                    &format!("lr-bad-{plain}"),
                    new_record("chat", "S-1", 1, 1, &record_body(plain)),
                ))
                .unwrap(),
        );
        assert!(why.contains("TOKEN"), "{plain:?}: {why}");
        assert!(
            !why.contains("hunter2"),
            "a refusal never repeats the value: {why}"
        );
    }
    assert_eq!(store.launch_record("chat", "S-1", 1, 1).unwrap(), None);
    // `sha256:` and `ref:` values are accepted.
    applied(
        store
            .commit(record_commit(
                "lr-hash",
                new_record("chat", "S-1", 1, 1, &record_body(&hashed_secret())),
            ))
            .unwrap(),
    );
    applied(
        store
            .commit(record_commit(
                "lr-ref",
                new_record("chat", "S-2", 1, 1, &record_body("ref:seat-token")),
            ))
            .unwrap(),
    );
    // A name that is not a secret may hold plain text.
    let row = store.launch_record("chat", "S-1", 1, 1).unwrap().unwrap();
    assert!(row.record.contains("/home/dev"));
}

#[test]
fn a_record_sha256_that_does_not_match_the_body_is_refused() {
    let (_d, store) = open();
    let mut record = new_record("chat", "S-1", 1, 1, &body());
    record.record_sha256 = "0".repeat(64);
    let why = rejected(store.commit(record_commit("lr-1", record)).unwrap());
    assert!(why.contains("record_sha256"), "{why}");
    // The hash of the carried text, when the text is not canonical, is not the canonical hash.
    let mut record = new_record("chat", "S-1", 1, 1, &body());
    record.record = format!(" {}", record.record);
    // The same value with other white space has the same canonical body, so it is accepted.
    applied(store.commit(record_commit("lr-2", record)).unwrap());
    // A body with other content and the old hash is refused.
    let mut record = new_record("chat", "S-2", 1, 1, &body());
    record.record = record.record.replace("claude-code", "other-runtime");
    let why = rejected(store.commit(record_commit("lr-3", record)).unwrap());
    assert!(why.contains("record_sha256"), "{why}");
    // A body that is not JSON, and one that is not an object, are refused.
    for text in ["not json", "[1,2]"] {
        let mut record = new_record("chat", "S-3", 1, 1, &body());
        record.record = text.to_string();
        rejected(store.commit(record_commit("lr-4", record)).unwrap());
    }
}

#[test]
fn a_host_row_names_its_launched_revision_and_hash_and_a_later_revision_leaves_them() {
    let (_d, store) = open();
    applied(write(&store, "S-1", 1, 1));
    let first = store.launch_record("chat", "S-1", 1, 1).unwrap().unwrap();
    let unit = "pf-host-srv-S-1-1";
    applied(
        store
            .commit(host_commit(
                "h-1",
                new_host(unit, "chat", "S-1", 1, 1, &first.record_sha256),
            ))
            .unwrap(),
    );
    // A live change writes revision 2 after the host started.
    let changed = record_body(&hashed_secret()).replace("\"fresh\"", "\"--resume\"");
    applied(
        store
            .commit(record_commit(
                "lr-live-change",
                new_record("chat", "S-1", 1, 2, &changed),
            ))
            .unwrap(),
    );
    let host = store.host(unit).unwrap().unwrap();
    assert_eq!(host.launched_revision, 1);
    assert_eq!(host.launched_record_sha256, first.record_sha256);
    assert_eq!(host.command_id, "h-1");
    assert_eq!((host.protocol, host.invocation_id.as_str()), (1, "inv-1"));
    assert_eq!(host.ended_at, None);
    assert_eq!(host.exit, None);
    // The newest revision is not the host's launched one.
    let newest = store
        .newest_launch_record("chat", "S-1", 1)
        .unwrap()
        .unwrap();
    assert_eq!(newest.revision, 2);
    assert_ne!(newest.record_sha256, host.launched_record_sha256);
}

#[test]
fn a_launched_revision_that_does_not_exist_or_a_hash_that_is_not_its_own_is_refused() {
    let (_d, store) = open();
    applied(write(&store, "S-1", 1, 1));
    let good = store
        .launch_record("chat", "S-1", 1, 1)
        .unwrap()
        .unwrap()
        .record_sha256;
    // A revision that does not exist, an incarnation that has no record, a session that has none.
    for (session, incarnation, revision) in [("S-1", 1, 2), ("S-1", 2, 1), ("S-9", 1, 1)] {
        let why = rejected(
            store
                .commit(host_commit(
                    "h-1",
                    new_host("u", "chat", session, incarnation, revision, &good),
                ))
                .unwrap(),
        );
        assert!(why.contains("revision"), "{why}");
    }
    // The hash of another body.
    let other = record_sha256("{}");
    let why = rejected(
        store
            .commit(host_commit(
                "h-2",
                new_host("u", "chat", "S-1", 1, 1, &other),
            ))
            .unwrap(),
    );
    assert!(why.contains("hash"), "{why}");
    // A hash that is not 64 hex digits.
    rejected(
        store
            .commit(host_commit(
                "h-3",
                new_host("u", "chat", "S-1", 1, 1, "abc"),
            ))
            .unwrap(),
    );
    assert!(store.live_hosts().unwrap().is_empty());
    // The same unit twice, and a second host for one incarnation.
    applied(
        store
            .commit(host_commit(
                "h-4",
                new_host("u", "chat", "S-1", 1, 1, &good),
            ))
            .unwrap(),
    );
    rejected(
        store
            .commit(host_commit(
                "h-5",
                new_host("u", "chat", "S-1", 1, 1, &good),
            ))
            .unwrap(),
    );
    rejected(
        store
            .commit(host_commit(
                "h-6",
                new_host("v", "chat", "S-1", 1, 1, &good),
            ))
            .unwrap(),
    );
    assert_eq!(store.live_hosts().unwrap().len(), 1);
}

#[test]
fn a_terminal_and_a_detached_launcher_are_accepted_and_an_unknown_kind_or_launcher_is_refused() {
    let (_d, store) = open();
    let mut terminal = new_record("terminal", "T-1", 1, 1, &body());
    terminal.launcher = "detached".to_string();
    applied(store.commit(record_commit("lr-t", terminal)).unwrap());
    let mut systemd = new_record("seat", "u1", 1, 1, &body());
    systemd.launcher = "systemd".to_string();
    applied(store.commit(record_commit("lr-s", systemd)).unwrap());

    let why = rejected(
        store
            .commit(record_commit(
                "lr-k",
                new_record("daemon", "D-1", 1, 1, &body()),
            ))
            .unwrap(),
    );
    assert!(why.contains("kind"), "{why}");
    let mut launchd = new_record("chat", "S-5", 1, 1, &body());
    launchd.launcher = "launchd".to_string();
    let why = rejected(store.commit(record_commit("lr-l", launchd)).unwrap());
    assert!(why.contains("launcher"), "{why}");
    // The host row says the same of its kind and its launcher.
    let hash = store
        .launch_record("terminal", "T-1", 1, 1)
        .unwrap()
        .unwrap()
        .record_sha256;
    applied(
        store
            .commit(host_commit(
                "h-t",
                new_host("pf-host-srv-T-1-1", "terminal", "T-1", 1, 1, &hash),
            ))
            .unwrap(),
    );
    let mut bad = new_host("w", "terminal", "T-1", 1, 1, &hash);
    bad.launcher = "launchd".to_string();
    rejected(store.commit(host_commit("h-l", bad)).unwrap());
    rejected(
        store
            .commit(host_commit(
                "h-k",
                new_host("x", "daemon", "T-1", 1, 1, &hash),
            ))
            .unwrap(),
    );
}

#[test]
fn live_hosts_list_only_the_rows_with_no_end() {
    let (_d, store) = open();
    for session in ["S-1", "S-2", "S-3"] {
        applied(write(&store, session, 1, 1));
        let hash = store
            .launch_record("chat", session, 1, 1)
            .unwrap()
            .unwrap()
            .record_sha256;
        applied(
            store
                .commit(host_commit(
                    &format!("h-{session}"),
                    new_host(&format!("u-{session}"), "chat", session, 1, 1, &hash),
                ))
                .unwrap(),
        );
    }
    let units = |store: &SqliteStore| -> Vec<String> {
        store
            .live_hosts()
            .unwrap()
            .into_iter()
            .map(|h| h.unit)
            .collect()
    };
    assert_eq!(units(&store), ["u-S-1", "u-S-2", "u-S-3"]);
    applied(
        store
            .commit(host_end_commit("e-2", "u-S-2", "{\"code\":0}"))
            .unwrap(),
    );
    assert_eq!(units(&store), ["u-S-1", "u-S-3"]);
    let ended = store.host("u-S-2").unwrap().unwrap();
    assert_eq!(ended.ended_at.as_deref(), Some("t3"));
    assert_eq!(ended.exit.as_deref(), Some("{\"code\":0}"));
    // A second end, an unknown unit and an exit that is not JSON are refused.
    rejected(
        store
            .commit(host_end_commit("e-2b", "u-S-2", "{\"code\":1}"))
            .unwrap(),
    );
    rejected(
        store
            .commit(host_end_commit("e-9", "u-none", "{\"code\":0}"))
            .unwrap(),
    );
    rejected(
        store
            .commit(host_end_commit("e-3", "u-S-3", "not json"))
            .unwrap(),
    );
    assert_eq!(units(&store), ["u-S-1", "u-S-3"]);
    assert_eq!(store.host("u-none").unwrap(), None);
}

#[test]
fn the_three_effects_are_written_only_in_an_admission_under_the_controller() {
    let (_d, store) = open();
    let scoped = |mut c: Commit, scope: &str| {
        c.run_id = scope.to_string();
        c
    };
    let record = new_record("chat", "S-1", 1, 1, &body());
    let why = rejected(
        store
            .commit(scoped(record_commit("c-1", record.clone()), "run-1"))
            .unwrap(),
    );
    assert!(why.contains("@controller"), "{why}");
    assert_eq!(store.launch_record("chat", "S-1", 1, 1).unwrap(), None);
    applied(store.commit(record_commit("c-2", record.clone())).unwrap());
    let host = new_host("u", "chat", "S-1", 1, 1, &record.record_sha256);
    rejected(
        store
            .commit(scoped(host_commit("c-3", host.clone()), "run-1"))
            .unwrap(),
    );
    applied(store.commit(host_commit("c-4", host)).unwrap());
    rejected(
        store
            .commit(scoped(host_end_commit("c-5", "u", "{}"), "run-1"))
            .unwrap(),
    );
    // A refused admission under another scope may still be recorded as a refusal of that command.
    let refusal = commit(
        "launch_record_write",
        "c-6",
        "{}",
        AdmissionOutcome::Refused,
        Some("refused"),
        None,
        Vec::<Effect>::new(),
        None,
    );
    applied(store.commit(refusal).unwrap());
}

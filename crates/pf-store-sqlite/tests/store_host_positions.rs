//! Host positions on the store port, on the real SQLite store (ADR-0003's amendment of 2026-11-16,
//! ADR-0011's kept-and-resent event test; `r2.s2.w4`): `Effect::HostPositionWrite` under
//! `@controller` and `Store::host_position`.

mod common;

use common::*;
use pf_engine::port::store::{Commit, Committed, Store};
use rusqlite::Connection;

const POSITION: &str = r#"{"offset":42,"pending":[]}"#;

#[test]
fn a_save_for_a_live_host_is_stored_and_read_back_and_the_next_seq_replaces_it() {
    let (_d, store) = open();
    live_host(&store, "u-1");
    assert_eq!(
        store.host_position("u-1").unwrap(),
        None,
        "nothing saved yet"
    );

    let (admission, ledger) = applied(
        store
            .commit(position_commit("hp-u-1-1", "u-1", 1, POSITION))
            .unwrap(),
    );
    assert_eq!(admission.kind, "host_position");
    assert_eq!(admission.run_id, "@controller");
    assert!(ledger.is_none(), "a save writes no ledger line");
    let row = store.host_position("u-1").unwrap().unwrap();
    assert_eq!(row.unit, "u-1");
    assert_eq!(row.seq, 1);
    assert_eq!(row.command_id, "hp-u-1-1");
    assert_eq!(row.updated_at, "t5");
    let value: serde_json::Value = serde_json::from_str(&row.position).unwrap();
    assert_eq!(value["offset"], 42);

    let second = r#"{"offset":99,"pending":[]}"#;
    applied(
        store
            .commit(position_commit("hp-u-1-2", "u-1", 2, second))
            .unwrap(),
    );
    let row = store.host_position("u-1").unwrap().unwrap();
    assert_eq!(row.seq, 2);
    assert_eq!(row.command_id, "hp-u-1-2");
    let value: serde_json::Value = serde_json::from_str(&row.position).unwrap();
    assert_eq!(
        value["offset"], 99,
        "the read is the last accepted position"
    );
}

#[test]
fn a_skipped_or_repeated_seq_is_rejected_and_writes_nothing() {
    let (_d, store) = open();
    live_host(&store, "u-1");
    // The first save is seq 1, whatever else is asked.
    for (command, seq) in [("c-0", 0), ("c-2", 2), ("c-3", 3)] {
        let why = rejected(
            store
                .commit(position_commit(command, "u-1", seq, POSITION))
                .unwrap(),
        );
        assert!(why.contains("seq"), "seq {seq}: {why}");
    }
    assert_eq!(store.host_position("u-1").unwrap(), None);
    applied(
        store
            .commit(position_commit("c-a", "u-1", 1, POSITION))
            .unwrap(),
    );
    // A repeat of seq 1 under another command ID, and a skip to seq 3.
    for (command, seq) in [("c-b", 1), ("c-c", 3)] {
        let why = rejected(
            store
                .commit(position_commit(command, "u-1", seq, POSITION))
                .unwrap(),
        );
        assert!(why.contains("seq 2 is next"), "seq {seq}: {why}");
    }
    assert_eq!(store.host_position("u-1").unwrap().unwrap().seq, 1);
    // A refused commit leaves no admission: its command ID is free for a later, valid save.
    assert_eq!(store.admission("@controller", "c-b").unwrap(), None);
    applied(
        store
            .commit(position_commit("c-b", "u-1", 2, POSITION))
            .unwrap(),
    );
}

#[test]
fn a_replay_by_command_id_changes_nothing_and_other_content_conflicts() {
    let (_d, store) = open();
    live_host(&store, "u-1");
    applied(
        store
            .commit(position_commit("hp-u-1-1", "u-1", 1, POSITION))
            .unwrap(),
    );
    match store
        .commit(position_commit("hp-u-1-1", "u-1", 1, POSITION))
        .unwrap()
    {
        Committed::Replayed(row) => assert_eq!(row.command_id, "hp-u-1-1"),
        other => panic!("not a replay: {other:?}"),
    }
    match store
        .commit(position_commit(
            "hp-u-1-1",
            "u-1",
            1,
            r#"{"offset":7,"pending":[]}"#,
        ))
        .unwrap()
    {
        Committed::Conflict(_) => {}
        other => panic!("not a conflict: {other:?}"),
    }
    let row = store.host_position("u-1").unwrap().unwrap();
    assert_eq!((row.seq, row.position.contains("42")), (1, true));
}

#[test]
fn a_unit_with_no_host_row_is_rejected_and_an_ended_host_takes_a_write() {
    let (_d, store) = open();
    let why = rejected(
        store
            .commit(position_commit("hp-x-1", "u-none", 1, POSITION))
            .unwrap(),
    );
    assert!(why.contains("no host"), "{why}");
    assert_eq!(store.host_position("u-none").unwrap(), None);

    live_host(&store, "u-1");
    applied(
        store
            .commit(host_end_commit("e-1", "u-1", "{\"code\":0}"))
            .unwrap(),
    );
    // The adapter saves its last position after the host's end: the row is the host's, ended or not.
    applied(
        store
            .commit(position_commit("hp-u-1-1", "u-1", 1, POSITION))
            .unwrap(),
    );
    assert_eq!(store.host_position("u-1").unwrap().unwrap().seq, 1);
}

#[test]
fn a_position_must_be_json_and_is_written_only_under_the_controller() {
    let (_d, store) = open();
    live_host(&store, "u-1");
    let why = rejected(
        store
            .commit(position_commit("hp-bad", "u-1", 1, "not json"))
            .unwrap(),
    );
    assert!(why.contains("JSON"), "{why}");

    let scoped = |mut c: Commit, scope: &str| {
        c.run_id = scope.to_string();
        c
    };
    let why = rejected(
        store
            .commit(scoped(
                position_commit("hp-u-1-1", "u-1", 1, POSITION),
                "run-1",
            ))
            .unwrap(),
    );
    assert!(why.contains("@controller"), "{why}");
    assert_eq!(store.host_position("u-1").unwrap(), None);
    // Under the controller the same save goes through.
    applied(
        store
            .commit(position_commit("hp-u-1-1", "u-1", 1, POSITION))
            .unwrap(),
    );
}

#[test]
fn the_triggers_refuse_a_lower_seq_a_delete_and_an_insert_or_replace_on_a_raw_connection() {
    let (dir, store) = open();
    live_host(&store, "u-1");
    applied(
        store
            .commit(position_commit("hp-u-1-1", "u-1", 1, POSITION))
            .unwrap(),
    );
    applied(
        store
            .commit(position_commit("hp-u-1-2", "u-1", 2, POSITION))
            .unwrap(),
    );
    let conn = Connection::open(dir.path().join("store.sqlite")).unwrap();
    let refusal = |sql: &str| -> String {
        conn.execute(sql, [])
            .expect_err(&format!("{sql} must be refused"))
            .to_string()
    };
    for sql in [
        "UPDATE host_positions SET seq = 1",
        "UPDATE host_positions SET seq = 2",
        "UPDATE host_positions SET seq = 4",
        "UPDATE host_positions SET unit = 'other', seq = 3",
    ] {
        let why = refusal(sql);
        assert!(why.contains("a host position"), "{sql}: {why}");
    }
    let why = refusal("DELETE FROM host_positions");
    assert!(why.contains("never deleted"), "{why}");
    let why = refusal(
        "INSERT OR REPLACE INTO host_positions (unit, seq, position, command_id, updated_at) \
         VALUES ('u-1', 1, '{}', 'raw', 't9')",
    );
    assert!(why.contains("never replaced"), "{why}");
    let (seq, position): (i64, String) = conn
        .query_row("SELECT seq, position FROM host_positions", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!((seq, position.as_str()), (2, POSITION));
    // The one change the table allows: the next seq of the same unit.
    conn.execute(
        "UPDATE host_positions SET seq = 3, position = '{\"offset\":1}'",
        [],
    )
    .unwrap();
}

//! Notices on the store port, on the real SQLite store (ADR-0003's amendment of 2026-11-21;
//! `r2.s11.w3`): `Effect::NoticeAdd`, `NoticeSupersede`, `NoticeSend` and `NoticeAck` under
//! `@controller`, and the reads `notice`, `notices_after`, `newest_notice`, `notice_deliveries`,
//! `waiting_deliveries`, `notice_targets` and `notice_sends`.

mod common;

use common::*;
use pf_engine::port::store::{Committed, DeliveryState, Store};
use rusqlite::Connection;

#[test]
fn a_notice_gets_the_next_position_and_its_deliveries_and_its_ledger_line_names_it() {
    let (_d, store) = open();
    assert_eq!(store.newest_notice().unwrap(), 0, "no notice yet");
    assert_eq!(store.notice(1).unwrap(), None);

    let (admission, ledger) = applied(
        store
            .commit(notice_commit(
                "notice:R-1:1:opened",
                "R-1",
                1,
                "opened",
                vec![
                    waiting("app", "A stop waits."),
                    superseded("orchestrator", "A stop waits.", "no_project"),
                ],
            ))
            .unwrap(),
    );
    assert_eq!(admission.kind, "notice");
    assert_eq!(admission.run_id, "@controller");
    assert_eq!(admission.unit_id.as_deref(), Some("1"), "the position");
    let ledger = ledger.expect("a notice writes its ledger line");
    assert_eq!(
        ledger.body,
        "notice 1 request=R-1@1 event=opened command=notice:R-1:1:opened"
    );

    let second = add_notice(&store, "R-2", 1, "opened");
    assert_eq!(second, 2, "one sequence for the store");
    assert_eq!(store.newest_notice().unwrap(), 2);

    let row = store.notice(1).unwrap().unwrap();
    assert_eq!(row.position, 1);
    assert_eq!(row.request_id, "R-1");
    assert_eq!(row.revision, 1);
    assert_eq!(row.event, "opened");
    assert_eq!(row.run_id, "run-1");
    assert_eq!(row.project_id.as_deref(), Some("P-1"));
    assert_eq!(row.created_at, "t6");
    assert_eq!(row.command_id, "notice:R-1:1:opened");

    let deliveries = store.notice_deliveries(1).unwrap();
    assert_eq!(deliveries.len(), 2);
    let app = deliveries.iter().find(|d| d.channel == "app").unwrap();
    assert_eq!(
        (app.state, app.why.as_deref(), app.text.as_str()),
        (DeliveryState::Waiting, None, "A stop waits.")
    );
    let chat = deliveries
        .iter()
        .find(|d| d.channel == "orchestrator")
        .unwrap();
    assert_eq!(
        (chat.state, chat.why.as_deref()),
        (DeliveryState::Superseded, Some("no_project"))
    );
    assert_eq!(app.updated_at, "t6");

    let after = store.notices_after(1, 10).unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].position, 2);
    assert_eq!(
        store.notices_after(0, 1).unwrap().len(),
        1,
        "the limit holds"
    );
    assert_eq!(store.notices_after(2, 10).unwrap().len(), 0);
}

#[test]
fn a_second_notice_of_one_event_is_rejected_and_writes_nothing() {
    let (_d, store) = open();
    add_notice(&store, "R-1", 1, "opened");
    let why = rejected(
        store
            .commit(notice_commit(
                "other-command",
                "R-1",
                1,
                "opened",
                vec![waiting("app", "again")],
            ))
            .unwrap(),
    );
    assert!(why.contains("R-1") && why.contains("opened"), "{why}");
    assert_eq!(store.newest_notice().unwrap(), 1, "no position was used");
    assert_eq!(store.notice_deliveries(1).unwrap().len(), 2);
    // A refused commit leaves no admission: its command ID is free.
    assert_eq!(
        store.admission("@controller", "other-command").unwrap(),
        None
    );
    // Another event of the same revision, and another revision, are other notices.
    assert_eq!(add_notice(&store, "R-1", 1, "closed"), 2);
    assert_eq!(add_notice(&store, "R-1", 2, "revised"), 3);
}

#[test]
fn a_notice_with_a_bad_event_revision_or_delivery_is_rejected() {
    let (_d, store) = open();
    for (event, revision, deliveries, word) in [
        ("pinged", 1, vec![waiting("app", "x")], "event"),
        ("opened", 0, vec![waiting("app", "x")], "revision"),
        ("opened", 1, vec![], "deliver"),
        ("opened", 1, vec![waiting("", "x")], "channel"),
        (
            "opened",
            1,
            vec![waiting("app", "x"), waiting("app", "y")],
            "twice",
        ),
    ] {
        let why = rejected(
            store
                .commit(notice_commit("bad", "R-1", revision, event, deliveries))
                .unwrap(),
        );
        assert!(why.contains(word), "{event}/{revision}: {why}");
    }
    assert_eq!(store.newest_notice().unwrap(), 0);
}

#[test]
fn every_notice_effect_is_rejected_outside_the_controllers_scope() {
    let (_d, store) = open();
    let position = add_notice(&store, "R-1", 1, "opened");
    let mut commits = vec![
        notice_commit("n-x", "R-2", 1, "opened", vec![waiting("app", "x")]),
        send_commit(position, "device:D-1", 1),
        ack_commit("a-x", "device:D-1", "app", position, "shown"),
        supersede_commit("s-x", position, "app", "request_closed"),
    ];
    for c in &mut commits {
        c.run_id = "run-1".to_string();
        let why = rejected(store.commit(c.clone()).unwrap());
        assert!(why.contains("@controller"), "{}: {why}", c.admission.kind);
    }
    assert_eq!(store.newest_notice().unwrap(), 1);
    assert!(store.notice_targets().unwrap().is_empty());
    assert!(store.notice_sends(position).unwrap().is_empty());
    assert_eq!(store.waiting_deliveries("app").unwrap().len(), 1);
}

#[test]
fn a_delivery_is_superseded_once_and_the_waiting_read_leaves_it_out() {
    let (_d, store) = open();
    let p1 = add_notice(&store, "R-1", 1, "opened");
    let p2 = add_notice(&store, "R-2", 1, "opened");
    let waiting_app: Vec<u64> = store
        .waiting_deliveries("app")
        .unwrap()
        .iter()
        .map(|d| d.position)
        .collect();
    assert_eq!(waiting_app, vec![p1, p2], "oldest first");

    let (_, ledger) = applied(
        store
            .commit(supersede_commit("sup-1", p1, "app", "request_closed"))
            .unwrap(),
    );
    assert_eq!(
        ledger.unwrap().body,
        "notice_superseded 1 channel=app why=request_closed"
    );
    let rest: Vec<u64> = store
        .waiting_deliveries("app")
        .unwrap()
        .iter()
        .map(|d| d.position)
        .collect();
    assert_eq!(rest, vec![p2]);
    let row = store
        .notice_deliveries(p1)
        .unwrap()
        .into_iter()
        .find(|d| d.channel == "app")
        .unwrap();
    assert_eq!(
        (row.state, row.why.as_deref(), row.updated_at.as_str()),
        (DeliveryState::Superseded, Some("request_closed"), "t30")
    );
    // The other channel of the same notice still waits.
    assert_eq!(store.waiting_deliveries("orchestrator").unwrap().len(), 2);

    // A second supersede, an unknown delivery and an empty reason are rejected.
    let again = rejected(
        store
            .commit(supersede_commit("sup-2", p1, "app", "request_closed"))
            .unwrap(),
    );
    assert!(again.contains("superseded"), "{again}");
    let unknown = rejected(
        store
            .commit(supersede_commit("sup-3", 9, "app", "request_closed"))
            .unwrap(),
    );
    assert!(unknown.contains("no delivery"), "{unknown}");
    let no_reason = rejected(
        store
            .commit(supersede_commit("sup-4", p2, "app", ""))
            .unwrap(),
    );
    assert!(no_reason.contains("reason"), "{no_reason}");
}

#[test]
fn a_send_count_is_the_stored_one_plus_one() {
    let (_d, store) = open();
    let position = add_notice(&store, "R-1", 1, "opened");
    assert!(store.notice_sends(position).unwrap().is_empty());

    let (_, ledger) = applied(
        store
            .commit(send_commit(position, "device:D-1", 1))
            .unwrap(),
    );
    assert_eq!(
        ledger.unwrap().body,
        format!("notice_send {position} target=device:D-1 attempt=1")
    );
    // A skip, and a repeat under another command ID, are rejected and leave no admission.
    for (n, word) in [(3, "2 is next"), (1, "2 is next"), (0, "2 is next")] {
        let mut c = send_commit(position, "device:D-1", n);
        c.command_id = format!("x-{n}");
        let why = rejected(store.commit(c).unwrap());
        assert!(why.contains(word), "sends {n}: {why}");
        assert_eq!(
            store.admission("@controller", &format!("x-{n}")).unwrap(),
            None
        );
    }
    applied(
        store
            .commit(send_commit(position, "device:D-1", 2))
            .unwrap(),
    );
    // A first send starts at 1, whatever number it names.
    for n in [0, 2] {
        let why = rejected(
            store
                .commit(send_commit(position, "device:D-9", n))
                .unwrap(),
        );
        assert!(why.contains("starts at 1"), "first send {n}: {why}");
    }
    // Another target counts from 1.
    applied(
        store
            .commit(send_commit(position, "agent:P-1:orchestrator", 1))
            .unwrap(),
    );

    let mut sends = store.notice_sends(position).unwrap();
    sends.sort_by(|a, b| a.target.cmp(&b.target));
    assert_eq!(sends.len(), 2);
    assert_eq!(sends[0].target, "agent:P-1:orchestrator");
    assert_eq!((sends[0].sends, sends[0].last_sent_at.as_str()), (1, "t11"));
    assert_eq!(sends[1].target, "device:D-1");
    assert_eq!((sends[1].sends, sends[1].last_sent_at.as_str()), (2, "t12"));
    assert_eq!(sends[1].position, position);

    // A notice that is not there takes no send.
    let why = rejected(store.commit(send_commit(9, "device:D-1", 1)).unwrap());
    assert!(why.contains("no notice"), "{why}");
}

#[test]
fn an_acknowledgement_makes_the_target_and_only_raises_it() {
    let (_d, store) = open();
    // With no notice, a join at position 0 makes the row.
    applied(
        store
            .commit(ack_commit("ack-0", "device:D-1", "app", 0, "joined"))
            .unwrap(),
    );
    let targets = store.notice_targets().unwrap();
    assert_eq!(targets.len(), 1);
    assert_eq!(
        (
            targets[0].target.as_str(),
            targets[0].channel.as_str(),
            targets[0].acked,
            targets[0].outcome.as_str()
        ),
        ("device:D-1", "app", 0, "joined")
    );

    let p1 = add_notice(&store, "R-1", 1, "opened");
    let p2 = add_notice(&store, "R-2", 1, "opened");
    let (admission, ledger) = applied(
        store
            .commit(ack_commit("ack-1", "device:D-1", "app", p1, "shown"))
            .unwrap(),
    );
    assert_eq!(admission.kind, "notice_ack");
    assert_eq!(
        ledger.unwrap().body,
        format!("notice_ack device:D-1 position={p1} outcome=shown")
    );
    let row = &store.notice_targets().unwrap()[0];
    assert_eq!(
        (row.acked, row.outcome.as_str(), row.updated_at.as_str()),
        (p1, "shown", "t20")
    );

    // At or below the stored position: rejected, nothing written, the ID stays free.
    for (id, position) in [("ack-same", p1), ("ack-low", 0)] {
        let why = rejected(
            store
                .commit(ack_commit(id, "device:D-1", "app", position, "shown"))
                .unwrap(),
        );
        assert!(why.contains("does not rise"), "{id}: {why}");
        assert_eq!(store.admission("@controller", id).unwrap(), None);
    }
    // Above the newest notice: rejected.
    let above = rejected(
        store
            .commit(ack_commit("ack-high", "device:D-1", "app", p2 + 1, "shown"))
            .unwrap(),
    );
    assert!(above.contains("newest"), "{above}");
    // Another channel for the same target is rejected.
    let channel = rejected(
        store
            .commit(ack_commit(
                "ack-chan",
                "device:D-1",
                "orchestrator",
                p2,
                "shown",
            ))
            .unwrap(),
    );
    assert!(channel.contains("channel"), "{channel}");
    // A blocked acknowledgement raises the position and keeps its outcome.
    applied(
        store
            .commit(ack_commit("ack-2", "device:D-1", "app", p2, "blocked"))
            .unwrap(),
    );
    let row = &store.notice_targets().unwrap()[0];
    assert_eq!((row.acked, row.outcome.as_str()), (p2, "blocked"));
    // A new row above the newest notice is rejected too, and an empty target is no target.
    let new_high = rejected(
        store
            .commit(ack_commit(
                "ack-new-high",
                "device:D-2",
                "app",
                p2 + 5,
                "joined",
            ))
            .unwrap(),
    );
    assert!(new_high.contains("newest"), "{new_high}");
    let empty = rejected(
        store
            .commit(ack_commit("ack-empty", "", "app", p2, "joined"))
            .unwrap(),
    );
    assert!(empty.contains("target"), "{empty}");
}

#[test]
fn the_same_command_replays_and_another_content_conflicts() {
    let (_d, store) = open();
    let position = add_notice(&store, "R-1", 1, "opened");
    match store
        .commit(notice_commit(
            "notice:R-1:1:opened",
            "R-1",
            1,
            "opened",
            vec![
                waiting("app", "App text."),
                waiting("orchestrator", "Chat text."),
            ],
        ))
        .unwrap()
    {
        Committed::Replayed(row) => {
            assert_eq!(row.unit_id.as_deref(), Some(position.to_string().as_str()))
        }
        other => panic!("not a replay: {other:?}"),
    }
    match store
        .commit(notice_commit(
            "notice:R-1:1:opened",
            "R-1",
            2,
            "revised",
            vec![waiting("app", "x")],
        ))
        .unwrap()
    {
        Committed::Conflict(_) => {}
        other => panic!("not a conflict: {other:?}"),
    }
    assert_eq!(store.newest_notice().unwrap(), 1);
}

#[test]
fn the_rows_are_bounded_by_the_events_the_targets_and_the_sends() {
    // One notice per request event, one target per device or agent, one send row per notice and
    // target: repeated commits add no row.
    let (dir, store) = open();
    let p = add_notice(&store, "R-1", 1, "opened");
    for n in 1..=5 {
        applied(store.commit(send_commit(p, "device:D-1", n)).unwrap());
    }
    applied(
        store
            .commit(ack_commit("a-1", "device:D-1", "app", p, "shown"))
            .unwrap(),
    );
    let conn = Connection::open(dir.path().join("store.sqlite")).unwrap();
    for (table, rows) in [
        ("notices", 1),
        ("notice_deliveries", 2),
        ("notice_targets", 1),
        ("notice_sends", 1),
    ] {
        let n: i64 = conn
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, rows, "{table}");
    }
    assert_eq!(store.notice_sends(p).unwrap()[0].sends, 5);
}

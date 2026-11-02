//! AC-10: ledger lines appear whole, in admission order, with rising sequence numbers. Earlier
//! lines are byte-identical after later appends.

mod common;

use common::*;
use pf_engine::ledger::{format_line, parse_seq};
use pf_engine::port::forge::PrRef;
use pf_engine::unit::Signal;
use pf_engine::{Command, CommandBody};

fn drive(env: &Env, ids: &[&str]) {
    let records = ids
        .iter()
        .enumerate()
        .map(|(i, id)| unit(id, "g1", u32::try_from(i).unwrap() + 1))
        .collect();
    env.engine.ingest(&mut FakeUnitSource { records }).unwrap();
    for id in ids {
        env.engine.start_stage(id).unwrap();
        env.engine
            .signal(
                id,
                "build",
                Signal::PrState {
                    state: "open".to_string(),
                },
                Some(PrRef {
                    repo: "synthetic/repo".to_string(),
                    number: 1,
                }),
            )
            .unwrap();
    }
}

#[test]
fn lines_are_whole_in_admission_order_with_rising_sequence_numbers() {
    let env = env(workflow(&["build"], false));
    drive(&env, &["u1", "u2", "u3"]);

    let text = env.ledger_text();
    assert!(text.ends_with('\n'), "the file ends with a newline");
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines.len() >= 7, "{text}");

    // Whole lines: one record per line, each with the sequence number and the run.
    for line in &lines {
        assert!(!line.is_empty());
        assert!(line.starts_with("- #"), "{line}");
        assert!(line.contains(" run-1 "), "{line}");
        assert!(line.contains(" | "), "{line}");
    }
    // Rising and without a gap: the store assigns 1, 2, 3, ...
    let seqs: Vec<u64> = lines.iter().map(|l| parse_seq(l).unwrap()).collect();
    let expected: Vec<u64> = (1..=u64::try_from(lines.len()).unwrap()).collect();
    assert_eq!(seqs, expected);

    // The file holds the outbox rows in commit order, and nothing else.
    let outbox = env.store.outbox();
    assert_eq!(outbox.len(), lines.len());
    let rebuilt: String = outbox.iter().map(|l| format_line(l.seq, &l.body)).collect();
    assert_eq!(rebuilt, text);
    assert!(
        outbox.iter().all(|l| l.written),
        "every line is marked written"
    );

    // Admission order: the timestamp of each line (a counting clock) rises with the sequence.
    let stamps: Vec<&str> = lines.iter().map(|l| l.split(' ').nth(2).unwrap()).collect();
    let mut sorted = stamps.clone();
    sorted.sort_unstable();
    assert_eq!(stamps, sorted);
    // And the order of the first lines is the order of the commands.
    assert!(lines[0].contains(" open_run "), "{}", lines[0]);
    assert!(lines[1].contains(" launch u1 "), "{}", lines[1]);
}

#[test]
fn earlier_lines_are_byte_identical_after_later_appends() {
    let env = env(workflow(&["build"], false));
    drive(&env, &["u1"]);
    let before = std::fs::read(env.ledger_path()).unwrap();
    assert!(!before.is_empty());

    drive_more(&env);
    let after = std::fs::read(env.ledger_path()).unwrap();
    assert!(after.len() > before.len());
    assert_eq!(
        &after[..before.len()],
        &before[..],
        "an earlier line changed"
    );

    // And again, to be sure that the prefix holds after every append.
    let middle = after.clone();
    env.engine.end_run("work is finished").unwrap();
    let last = std::fs::read(env.ledger_path()).unwrap();
    assert!(last.len() > middle.len());
    assert_eq!(&last[..middle.len()], &middle[..]);
}

fn drive_more(env: &Env) {
    env.engine
        .ingest(&mut FakeUnitSource {
            records: vec![unit("u9", "g1", 9)],
        })
        .unwrap();
    env.engine.start_stage("u9").unwrap();
    env.engine.park("u9", "stalled").unwrap();
}

#[test]
fn lines_that_other_writers_left_in_the_file_stay_and_the_numbers_continue_after_the_engine_own() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.md");
    let foreign = "- 2026-10-31T15:49:54.508Z unit-x seat | Q: a question | A: an answer\n\
                   # a heading line that is not a record\n";
    std::fs::write(&path, foreign).unwrap();

    let store = FakeStore::new();
    let engine = pf_engine::Engine::new(
        config(dir.path(), workflow(&["build"], false)),
        store.clone(),
        FakeAgent::new(),
        FakeForge::new(),
        counting_clock(),
        Box::new(pf_engine::ledger::FsLedger::new(&path)),
    )
    .unwrap();
    engine.start_up().unwrap();
    engine.halt("kill switch").unwrap();

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with(foreign), "foreign lines changed:\n{text}");
    let own: Vec<&str> = text[foreign.len()..].lines().collect();
    assert_eq!(own.len(), 2);
    assert_eq!(parse_seq(own[0]), Some(1));
    assert_eq!(parse_seq(own[1]), Some(2));
}

#[test]
fn a_line_is_cut_to_one_line_and_a_foreign_line_has_no_sequence_number() {
    assert_eq!(format_line(7, "one\ntwo\r\nthree"), "- #7 one two  three\n");
    assert_eq!(parse_seq("- #12 body"), Some(12));
    assert_eq!(parse_seq("- #12"), None);
    assert_eq!(parse_seq("- #x body"), None);
    assert_eq!(parse_seq("- 2026-10-31 something"), None);
    assert_eq!(parse_seq(""), None);
}

#[test]
fn a_command_that_is_replayed_or_rejected_writes_no_line() {
    // With a merge rule the unit stays `running` after its stage, so it can be parked.
    let env = env(workflow(&["build"], true));
    drive(&env, &["u1"]);
    let before = env.ledger_text();
    let park = Command {
        id: "p".to_string(),
        body: CommandBody::Park {
            unit_id: "u1".to_string(),
            reason: "stalled".to_string(),
        },
    };
    env.engine.submit(park.clone()).unwrap();
    let after_first = env.ledger_text();
    assert_eq!(after_first.lines().count(), before.lines().count() + 1);
    env.engine.submit(park).unwrap(); // replay
    assert!(
        env.engine
            .submit(Command {
                id: "p2".to_string(),
                body: CommandBody::Park {
                    unit_id: "u1".to_string(),
                    reason: "again".to_string(),
                },
            })
            .is_err()
    ); // the unit is parked now: rejected
    assert_eq!(env.ledger_text(), after_first);
}

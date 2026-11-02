//! AC-3: the queue unit source. A seat publishes a record by atomic rename. The queue reads it once.
//! A bad record halts the run through `halt_for`, and the halt names the file.

#[path = "../../pf-engine/tests/common/mod.rs"]
mod common;

use std::os::unix::fs::symlink;
use std::path::Path;

use common::{PrScript, env, item_workflow};
use pf_engine::launch::LaunchStep;
use pf_engine::merge::MergeStep;
use pf_engine::port::forge::PrRef;
use pf_engine::port::unit_source::UnitSource;
use pf_engine::unit::Signal;
use pf_workflow::queue::{MAX_RECORD_BYTES, QueuePoll, QueueSource, ingest_queue};

fn record(id: &str, group: &str, order: u32) -> String {
    format!(
        r#"{{"version":1,"id":"{id}","group":"{group}","order":{order},"vars":{{"brief":"do {id}"}}}}"#
    )
}

/// Publish the way a seat does: write a temp name, then rename.
fn publish(dir: &Path, name: &str, text: &str) {
    let tmp = dir.join(format!("{name}.tmp"));
    std::fs::write(&tmp, text).unwrap();
    std::fs::rename(&tmp, dir.join(name)).unwrap();
}

fn ids(records: &[pf_engine::port::unit_source::UnitRecord]) -> Vec<String> {
    records.iter().map(|r| r.id.clone()).collect()
}

#[test]
fn a_record_published_by_rename_is_read_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut q = QueueSource::new(dir.path(), "item");
    assert_eq!(q.poll().unwrap(), vec![]);
    publish(dir.path(), "u1.json", &record("u1", "g1", 1));
    assert_eq!(ids(&q.poll().unwrap()), ["u1"]);
    assert_eq!(
        q.poll().unwrap(),
        vec![],
        "the second poll reads nothing new"
    );
    publish(dir.path(), "u2.json", &record("u2", "g1", 2));
    publish(dir.path(), "a0.json", &record("a0", "g1", 3));
    assert_eq!(
        ids(&q.poll().unwrap()),
        ["a0", "u2"],
        "records come in name order"
    );
    assert_eq!(q.track(), Some("item"));
    assert_eq!(q.file_of("u2"), Some("u2.json"));
}

#[test]
fn a_missing_queue_directory_is_an_empty_queue() {
    let dir = tempfile::tempdir().unwrap();
    let mut q = QueueSource::new(dir.path().join("not-yet"), "item");
    assert_eq!(q.poll().unwrap(), vec![]);
}

#[test]
fn a_temp_name_a_non_json_file_and_a_symlink_are_not_admitted() {
    let dir = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    std::fs::write(elsewhere.path().join("real.json"), record("linked", "g", 1)).unwrap();
    // A temp name: the seat is still writing.
    std::fs::write(dir.path().join("u1.json.tmp"), record("u1", "g", 1)).unwrap();
    std::fs::write(dir.path().join(".u2.partial"), record("u2", "g", 2)).unwrap();
    // Not JSON by name.
    std::fs::write(dir.path().join("notes.txt"), record("u3", "g", 3)).unwrap();
    // A symlink to a valid record.
    symlink(
        elsewhere.path().join("real.json"),
        dir.path().join("link.json"),
    )
    .unwrap();
    // A directory with a record name.
    std::fs::create_dir(dir.path().join("dir.json")).unwrap();

    let mut q = QueueSource::new(dir.path(), "item");
    assert_eq!(q.poll().unwrap(), vec![]);
    let skipped: Vec<&str> = q.skipped().iter().map(|s| s.file.as_str()).collect();
    assert_eq!(skipped, ["dir.json", "link.json"]);
    assert!(q.skipped().iter().any(|s| s.why.contains("symlink")));

    // Once the seat finishes the rename, the record is read.
    std::fs::rename(dir.path().join("u1.json.tmp"), dir.path().join("u1.json")).unwrap();
    assert_eq!(ids(&q.poll().unwrap()), ["u1"]);
}

#[test]
fn the_same_content_twice_is_a_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let mut q = QueueSource::new(dir.path(), "item");
    publish(dir.path(), "u1.json", &record("u1", "g1", 1));
    // The same ID and content under another file name.
    publish(dir.path(), "u1-again.json", &record("u1", "g1", 1));
    assert_eq!(ids(&q.poll().unwrap()), ["u1"]);
    assert_eq!(q.poll().unwrap(), vec![]);

    // Through the engine: a second pass, and a fresh source after a restart, replay the unit.
    let e = env(item_workflow(&["build"], false));
    let first = ingest_queue(&e.engine, &mut QueueSource::new(dir.path(), "item")).unwrap();
    let QueuePoll::Ingested(report) = first else {
        panic!("{first:?}")
    };
    assert_eq!(report.admitted, ["u1"]);
    let again = ingest_queue(&e.engine, &mut QueueSource::new(dir.path(), "item")).unwrap();
    let QueuePoll::Ingested(report) = again else {
        panic!("{again:?}")
    };
    assert_eq!(report.replayed, ["u1"]);
    assert!(report.admitted.is_empty());
    assert!(
        e.engine
            .view()
            .unwrap()
            .run
            .unwrap()
            .halted_reason
            .is_none()
    );
}

#[test]
fn changed_content_a_malformed_file_and_an_oversize_file_each_name_the_file() {
    // Changed content in the same file.
    let dir = tempfile::tempdir().unwrap();
    let mut q = QueueSource::new(dir.path(), "item");
    publish(dir.path(), "u1.json", &record("u1", "g1", 1));
    q.poll().unwrap();
    publish(dir.path(), "u1.json", &record("u1", "g1", 2));
    let e = q.poll().unwrap_err().to_string();
    assert!(e.contains("u1.json") && e.contains("changed"), "{e}");

    // The same ID with other content in another file.
    let dir = tempfile::tempdir().unwrap();
    let mut q = QueueSource::new(dir.path(), "item");
    publish(dir.path(), "a.json", &record("u1", "g1", 1));
    publish(dir.path(), "b.json", &record("u1", "g1", 9));
    let e = q.poll().unwrap_err().to_string();
    assert!(
        e.contains("b.json") && e.contains("a.json") && e.contains("u1"),
        "{e}"
    );

    // Malformed: not JSON, an unknown field, a wrong version, a bad ID, not UTF-8.
    for (name, bytes) in [
        ("bad.json", b"{not json".to_vec()),
        (
            "bad.json",
            br#"{"version":1,"id":"u","group":"g","order":1,"vars":{},"extra":1}"#.to_vec(),
        ),
        (
            "bad.json",
            br#"{"version":2,"id":"u","group":"g","order":1,"vars":{}}"#.to_vec(),
        ),
        (
            "bad.json",
            br#"{"version":1,"id":"a b","group":"g","order":1,"vars":{}}"#.to_vec(),
        ),
        ("bad.json", vec![0xff, 0xfe, 0x00]),
    ] {
        let dir = tempfile::tempdir().unwrap();
        publish(dir.path(), name, "");
        std::fs::write(dir.path().join(name), &bytes).unwrap();
        let mut q = QueueSource::new(dir.path(), "item");
        let e = q.poll().unwrap_err().to_string();
        assert!(e.contains(name), "{e}");
    }

    // A file over 64 KiB. Exactly 64 KiB is allowed.
    let dir = tempfile::tempdir().unwrap();
    let base = record("big", "g1", 1);
    let pad = |len: u64| format!("{base}{}", " ".repeat((len as usize) - base.len()));
    publish(dir.path(), "big.json", &pad(MAX_RECORD_BYTES));
    let mut q = QueueSource::new(dir.path(), "item");
    assert_eq!(ids(&q.poll().unwrap()), ["big"]);
    let dir = tempfile::tempdir().unwrap();
    publish(dir.path(), "big.json", &pad(MAX_RECORD_BYTES + 1));
    let mut q = QueueSource::new(dir.path(), "item");
    let e = q.poll().unwrap_err().to_string();
    assert!(e.contains("big.json") && e.contains("65536"), "{e}");
}

#[test]
fn a_bad_file_leaves_the_good_ones_for_the_next_poll() {
    let dir = tempfile::tempdir().unwrap();
    let mut q = QueueSource::new(dir.path(), "item");
    publish(dir.path(), "a.json", &record("a", "g", 1));
    publish(dir.path(), "z.json", "{broken");
    assert!(q.poll().is_err());
    std::fs::remove_file(dir.path().join("z.json")).unwrap();
    assert_eq!(
        ids(&q.poll().unwrap()),
        ["a"],
        "the good record is not lost"
    );
}

fn ingest_ok(e: &common::Env, dir: &Path) -> pf_engine::IngestReport {
    match ingest_queue(&e.engine, &mut QueueSource::new(dir, "item")).unwrap() {
        QueuePoll::Ingested(r) => r,
        QueuePoll::Halted(why) => panic!("halted: {why}"),
    }
}

#[test]
fn after_a_bad_record_no_launch_and_no_merge_is_admitted_but_signals_are_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let e = env(item_workflow(&["build"], true));
    publish(dir.path(), "u1.json", &record("u1", "g1", 1));
    publish(dir.path(), "u2.json", &record("u2", "g1", 2));
    publish(dir.path(), "u3.json", &record("u3", "g1", 3));
    ingest_ok(&e, dir.path());
    // u1 has finished its stage and has a green PR. u2 is running a stage. u3 has not launched.
    assert!(matches!(
        e.engine.start_stage("u1").unwrap(),
        LaunchStep::Launched { .. }
    ));
    assert!(matches!(
        e.engine.start_stage("u2").unwrap(),
        LaunchStep::Launched { .. }
    ));
    e.forge.script(1, PrScript::green("work/u1", "sha1"));
    let pr = PrRef {
        repo: "owner/repo".into(),
        number: 1,
    };
    e.engine
        .signal(
            "u1",
            "build",
            Signal::DoneFile {
                path: "done".into(),
            },
            Some(pr),
        )
        .unwrap();

    // A seat rewrites a published record.
    let mut q = QueueSource::new(dir.path(), "item");
    q.poll().unwrap();
    publish(dir.path(), "u3.json", &record("u3", "g1", 7));
    let polled = ingest_queue(&e.engine, &mut q).unwrap();
    let QueuePoll::Halted(reason) = polled else {
        panic!("{polled:?}")
    };
    assert!(reason.contains("u3.json"), "{reason}");
    assert!(reason.contains("records never change"), "{reason}");

    let halted = e.engine.view().unwrap().run.unwrap().halted_reason;
    assert!(halted.expect("the run is halted").contains("u3.json"));

    // No launch and no merge.
    match e.engine.start_stage("u3").unwrap() {
        LaunchStep::Refused { reason } => assert_eq!(reason, "run halted"),
        other => panic!("{other:?}"),
    }
    match e.engine.merge_next().unwrap() {
        MergeStep::Held { unit, reason } => {
            assert_eq!(unit, "u1");
            assert_eq!(reason, "run halted");
        }
        other => panic!("the merge must be held: {other:?}"),
    }
    assert!(e.forge.merged().is_empty());
    assert_eq!(e.agent.launch_count(), 2);

    // A running unit's signal is still recorded.
    e.engine
        .signal(
            "u2",
            "build",
            Signal::DoneFile {
                path: "done".into(),
            },
            None,
        )
        .unwrap();
    let view = e.engine.view().unwrap();
    let stage = view.stages_of("u2")[0];
    assert_eq!(stage.status, pf_engine::Status::Done);

    // A second bad record does not fail the pass: the halt stays.
    publish(dir.path(), "bad.json", "{broken");
    let again = ingest_queue(&e.engine, &mut QueueSource::new(dir.path(), "item")).unwrap();
    assert!(matches!(again, QueuePoll::Halted(_)));
}

#[test]
fn a_record_that_changed_across_a_restart_halts_the_run_and_names_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let e = env(item_workflow(&["build"], false));
    publish(dir.path(), "u1.json", &record("u1", "g1", 1));
    ingest_ok(&e, dir.path());
    // The controller restarts, and the file has changed in between.
    publish(dir.path(), "u1.json", &record("u1", "g1", 5));
    let polled = ingest_queue(&e.engine, &mut QueueSource::new(dir.path(), "item")).unwrap();
    let QueuePoll::Halted(reason) = polled else {
        panic!("{polled:?}")
    };
    assert!(reason.contains("u1.json"), "{reason}");
    assert!(
        e.engine
            .view()
            .unwrap()
            .run
            .unwrap()
            .halted_reason
            .is_some()
    );
}

#[test]
fn a_hostile_value_in_vars_is_stored_as_text_and_never_run() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("pwned");
    let queue = dir.path().join("queue");
    std::fs::create_dir(&queue).unwrap();
    let hostile = [
        format!("$(touch {})", marker.display()),
        format!("`touch {}`", marker.display()),
        format!("x; touch {}", marker.display()),
        "--flag-looking value".to_string(),
        "line one\nline two".to_string(),
    ];
    let vars: Vec<String> = hostile
        .iter()
        .enumerate()
        .map(|(i, v)| {
            format!(
                "{:?}:{}",
                format!("k{i}"),
                serde_json::to_string(v).unwrap()
            )
        })
        .collect();
    let text = format!(
        r#"{{"version":1,"id":"u1","group":"g1","order":1,"vars":{{{}}}}}"#,
        vars.join(",")
    );
    publish(&queue, "u1.json", &text);

    let e = env(item_workflow(&["build"], false));
    ingest_ok(&e, &queue);
    let view = e.engine.view().unwrap();
    let unit = view.node("u1").unwrap();
    let detail = view.unit_detail(unit).unwrap();
    for (i, v) in hostile.iter().enumerate() {
        assert_eq!(&detail.record.vars[&format!("k{i}")], v);
    }
    assert!(!marker.exists(), "the queue ran a record value");
}

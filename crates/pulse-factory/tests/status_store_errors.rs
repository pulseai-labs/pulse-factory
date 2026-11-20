//! AC-11 of `r2.s2.w3` (the `status.rs` debt line; audit 10): `status` never answers an empty list for a
//! store read that failed. Each read it makes (`run`, `nodes`, `sessions`, `admissions`, `requests`) is
//! checked, and a failed one answers `cannot read run <id> from the store: <cause>`: the CLI exits
//! non-zero and prints no unit list. The test makes a read fail by renaming a table through its own
//! connection. With the store whole, `status` answers as before.

mod common;

use common::ctl::Live;
use common::{code, stderr, stdout};
use serde_json::Value;

fn rename(live: &Live, from: &str, to: &str) {
    let db = rusqlite::Connection::open(live.sb.state.join("store.sqlite")).unwrap();
    db.execute_batch(&format!("ALTER TABLE {from} RENAME TO {to}"))
        .unwrap();
}

#[test]
fn a_failed_store_read_is_an_error_and_never_an_empty_list() {
    let live = Live::start();
    let run = live.arm();
    live.queue_item("u1", 1);
    live.controller.tick_all();

    // The store whole: `status` answers as before, with the unit.
    let out = live.cli(&["status", &run, "--json"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let whole: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert!(
        whole["units"]
            .as_array()
            .unwrap()
            .iter()
            .any(|u| u["id"] == "u1"),
        "{whole}"
    );

    for table in ["runs", "nodes", "sessions", "admissions", "requests"] {
        let hidden = format!("{table}_hidden");
        rename(&live, table, &hidden);
        for args in [vec!["status", &run, "--json"], vec!["status", &run]] {
            let out = live.cli(&args);
            assert_ne!(
                code(&out),
                0,
                "{table}: status answered with a failed read: {}",
                stdout(&out)
            );
            let err = stderr(&out);
            assert!(
                err.contains(&format!("cannot read run {run} from the store")),
                "{table}: {err}"
            );
            assert!(
                !stdout(&out).contains("u1"),
                "{table}: a unit list came with the error: {}",
                stdout(&out)
            );
            assert!(
                !stdout(&out).contains("\"units\""),
                "{table}: {}",
                stdout(&out)
            );
        }
        rename(&live, &hidden, table);
        // The table is back: the same call answers again.
        let out = live.cli(&["status", &run, "--json"]);
        assert_eq!(code(&out), 0, "{table}: {}", stderr(&out));
    }
}

//! AC-3 (r1.s1.w2): a URL whose whole user info is the credential leaves no trace in the ledger or
//! in a stored admission. The value is not `ghp_`-shaped, so only the URL rule can catch it.

mod common;

use common::{Env, FakeUnitSource, env, unit, workflow};
use pf_engine::admission::PushOutcome;

const SECRET: &str = "s3cr3tvalue";
const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn run() -> Env {
    let e = env(workflow(&["build"], true));
    e.engine
        .ingest(&mut FakeUnitSource {
            records: vec![unit("u1", "g1", 1)],
        })
        .unwrap();
    e.engine.start_stage("u1").unwrap();
    e
}

fn stored(e: &Env) -> String {
    e.engine
        .view()
        .unwrap()
        .admissions
        .iter()
        .map(|a| a.content.clone())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_push_repo_url_with_a_bare_token_is_absent_from_the_ledger_and_the_admission() {
    let e = run();
    e.engine
        .record_push(
            "u1",
            "ev-1",
            &format!("https://{SECRET}@example.com/owner/repo"),
            "work/u1",
            SHA_A,
            PushOutcome::Pushed,
        )
        .unwrap();
    let ledger = e.ledger_text();
    assert!(!ledger.contains(SECRET), "{ledger}");
    assert!(
        ledger.contains("https://[redacted]@example.com/owner/repo"),
        "{ledger}"
    );
    let admissions = stored(&e);
    assert!(!admissions.contains(SECRET), "{admissions}");
}

#[test]
fn a_park_reason_with_a_bare_token_url_is_absent_from_the_ledger_and_the_admission() {
    let e = run();
    e.engine
        .park(
            "u1",
            &format!("fatal: unable to access 'https://{SECRET}@github.com/o/r': 403"),
        )
        .unwrap();
    let ledger = e.ledger_text();
    assert!(!ledger.contains(SECRET), "{ledger}");
    assert!(
        ledger.contains("https://[redacted]@github.com/o/r"),
        "{ledger}"
    );
    let admissions = stored(&e);
    assert!(!admissions.contains(SECRET), "{admissions}");
}

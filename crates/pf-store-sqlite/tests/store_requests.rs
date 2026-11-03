//! The store port for requests and claims, on real SQL: the `R-<n>` sequence, `PutRequest` on a
//! missing ID, and the atomicity of a commit whose second effect fails (a park and its request
//! commit together or not at all). Claims fence in order.

use pf_engine::port::store::*;
use pf_engine::status::Status;
use pf_store_sqlite::SqliteStore;

fn store() -> (tempfile::TempDir, SqliteStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(dir.path().join("store.sqlite")).unwrap();
    (dir, store)
}

fn admission(kind: &str) -> NewAdmission {
    NewAdmission {
        kind: kind.to_string(),
        outcome: AdmissionOutcome::Admitted,
        reason: None,
        unit_id: None,
        role: None,
        model: None,
        attempt: None,
        admitted_at: "2026-01-01T00:00:00.000Z".to_string(),
    }
}

fn commit(store: &SqliteStore, id: &str, effects: Vec<Effect>) -> Result<Committed, StoreError> {
    store.commit(Commit {
        run_id: "r1".to_string(),
        command_id: id.to_string(),
        content: format!("{{\"id\":\"{id}\"}}"),
        admission: admission("test"),
        effects,
        ledger: None,
    })
}

fn new_request(unit: Option<&str>, kind: RequestKind) -> NewRequest {
    NewRequest {
        run_id: "r1".to_string(),
        unit_id: unit.map(str::to_string),
        stage: "implement".to_string(),
        kind,
        reason: "why".to_string(),
        body: "{}".to_string(),
        opened_at: "2026-01-01T00:00:00.000Z".to_string(),
    }
}

fn unit_node(id: &str, status: Status) -> NodeRow {
    NodeRow {
        run_id: "r1".to_string(),
        id: id.to_string(),
        parent_id: None,
        kind: NodeKind::Unit,
        status,
        reason: None,
        role: None,
        model: None,
        attempt: 0,
        detail: "{}".to_string(),
        created_at: "2026-01-01T00:00:00.000Z".to_string(),
        updated_at: "2026-01-01T00:00:00.000Z".to_string(),
    }
}

fn claim(generation: u64, fenced: Option<&str>) -> ClaimRow {
    ClaimRow {
        generation,
        token_hash: format!("{generation:064x}"),
        holder: "ann@host".to_string(),
        claimed_at: "2026-01-01T00:00:00.000Z".to_string(),
        fenced_at: fenced.map(str::to_string),
    }
}

#[test]
fn request_ids_are_one_sequence_for_the_whole_store() {
    let (_dir, store) = store();
    commit(
        &store,
        "c1",
        vec![
            Effect::OpenRequest(new_request(Some("u1"), RequestKind::Park)),
            Effect::OpenRequest(new_request(Some("u2"), RequestKind::Stop)),
        ],
    )
    .unwrap();
    // Another run shares the sequence.
    let mut other = new_request(None, RequestKind::MergeAck);
    other.run_id = "r2".to_string();
    store
        .commit(Commit {
            run_id: "r2".to_string(),
            command_id: "c1".to_string(),
            content: "{}".to_string(),
            admission: admission("test"),
            effects: vec![Effect::OpenRequest(other)],
            ledger: None,
        })
        .unwrap();
    let ids: Vec<String> = store
        .requests("r1")
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(ids, vec!["R-1", "R-2"]);
    assert_eq!(store.requests("r2").unwrap()[0].id, "R-3");
    let first = store.request("R-1").unwrap().unwrap();
    assert_eq!(first.revision, 1);
    assert_eq!(first.status, RequestStatus::Open);
    assert_eq!(first.unit_id.as_deref(), Some("u1"));
    assert!(store.request("R-99").unwrap().is_none());
}

#[test]
fn put_request_on_a_missing_id_rolls_the_commit_back() {
    let (_dir, store) = store();
    commit(
        &store,
        "c1",
        vec![Effect::PutNode(unit_node("u1", Status::Running))],
    )
    .unwrap();
    let ghost = RequestRow {
        id: "R-7".to_string(),
        revision: 2,
        run_id: "r1".to_string(),
        unit_id: Some("u1".to_string()),
        stage: String::new(),
        kind: RequestKind::Park,
        reason: "why".to_string(),
        body: "{}".to_string(),
        status: RequestStatus::Open,
        opened_at: "2026-01-01T00:00:00.000Z".to_string(),
        answered_at: None,
        answer: None,
        answer_generation: None,
    };
    let result = commit(
        &store,
        "c2",
        vec![
            Effect::PutNode(unit_node("u1", Status::Parked)),
            Effect::PutRequest(ghost),
        ],
    );
    assert!(result.is_err(), "{result:?}");
    // The first effect did not stick, and the command left no admission.
    let nodes = store.nodes("r1").unwrap();
    assert_eq!(nodes[0].status, Status::Running);
    assert!(store.requests("r1").unwrap().is_empty());
    assert!(
        store
            .admissions("r1")
            .unwrap()
            .iter()
            .all(|a| a.command_id != "c2"),
        "a command that failed is not recorded as admitted"
    );
    // The same command ID can be sent again.
    commit(
        &store,
        "c2",
        vec![Effect::PutNode(unit_node("u1", Status::Parked))],
    )
    .unwrap();
}

#[test]
fn a_unique_violation_in_the_second_effect_rolls_the_first_back() {
    let (_dir, store) = store();
    commit(
        &store,
        "c1",
        vec![Effect::PutNode(unit_node("u1", Status::Running))],
    )
    .unwrap();
    // A park and its request commit together.
    commit(
        &store,
        "park-0",
        vec![
            Effect::PutNode(unit_node("u1", Status::Parked)),
            Effect::OpenRequest(new_request(Some("u1"), RequestKind::Park)),
        ],
    )
    .unwrap();
    assert_eq!(store.nodes("r1").unwrap()[0].status, Status::Parked);
    assert_eq!(store.requests("r1").unwrap().len(), 1);

    // A second open request for the same unit breaks the unique index. The effect before it, a
    // node write, must not stay, and neither may the admission row.
    let mut changed = unit_node("u1", Status::Failed);
    changed.reason = Some("must not stick".to_string());
    let result = commit(
        &store,
        "park-1",
        vec![
            Effect::PutNode(changed),
            Effect::OpenRequest(new_request(Some("u1"), RequestKind::Stop)),
        ],
    );
    assert!(result.is_err(), "{result:?}");
    let node = &store.nodes("r1").unwrap()[0];
    assert_eq!(node.status, Status::Parked, "the first effect rolled back");
    assert_eq!(node.reason, None);
    assert_eq!(store.requests("r1").unwrap().len(), 1, "no second request");
    assert!(
        store
            .admissions("r1")
            .unwrap()
            .iter()
            .all(|a| a.command_id != "park-1")
    );
}

#[test]
fn a_run_level_request_is_not_held_to_one_per_unit() {
    let (_dir, store) = store();
    commit(
        &store,
        "c1",
        vec![
            Effect::OpenRequest(new_request(None, RequestKind::MergeAck)),
            Effect::OpenRequest(new_request(None, RequestKind::MergeAck)),
        ],
    )
    .unwrap();
    assert_eq!(store.requests("r1").unwrap().len(), 2);
}

#[test]
fn put_request_replaces_the_row_and_keeps_the_id() {
    let (_dir, store) = store();
    commit(
        &store,
        "c1",
        vec![Effect::OpenRequest(new_request(
            Some("u1"),
            RequestKind::Park,
        ))],
    )
    .unwrap();
    let mut row = store.request("R-1").unwrap().unwrap();
    row.status = RequestStatus::Answered;
    row.answer = Some("{\"verb\":\"clear\"}".to_string());
    row.answer_generation = Some(3);
    row.answered_at = Some("2026-01-02T00:00:00.000Z".to_string());
    commit(&store, "c2", vec![Effect::PutRequest(row)]).unwrap();
    let back = store.request("R-1").unwrap().unwrap();
    assert_eq!(back.status, RequestStatus::Answered);
    assert_eq!(back.answer_generation, Some(3));
    assert_eq!(back.answer.as_deref(), Some("{\"verb\":\"clear\"}"));
    // The next request is R-2, and an answered unit can open another one.
    commit(
        &store,
        "c3",
        vec![Effect::OpenRequest(new_request(
            Some("u1"),
            RequestKind::Park,
        ))],
    )
    .unwrap();
    assert_eq!(store.requests("r1").unwrap()[1].id, "R-2");
}

#[test]
fn claims_fence_in_order() {
    let (_dir, store) = store();
    commit(&store, "claim:1", vec![Effect::PutClaim(claim(1, None))]).unwrap();

    // The new claim before the fence breaks the one-current index: it rolls back as a whole.
    let result = commit(
        &store,
        "claim:2",
        vec![
            Effect::PutClaim(claim(2, None)),
            Effect::PutClaim(claim(1, Some("2026-01-02T00:00:00.000Z"))),
        ],
    );
    assert!(result.is_err(), "{result:?}");
    let claims = store.claims().unwrap();
    assert_eq!(claims.len(), 1);
    assert!(claims[0].fenced_at.is_none());

    // Fence first, then insert: both land, and only the new claim is current.
    commit(
        &store,
        "claim:2",
        vec![
            Effect::PutClaim(claim(1, Some("2026-01-02T00:00:00.000Z"))),
            Effect::PutClaim(claim(2, None)),
        ],
    )
    .unwrap();
    let claims = store.claims().unwrap();
    assert_eq!(claims.len(), 2);
    assert_eq!(
        claims[0].fenced_at.as_deref(),
        Some("2026-01-02T00:00:00.000Z")
    );
    assert!(claims[1].fenced_at.is_none());
    assert_eq!(claims[1].token_hash, format!("{:064x}", 2));
    // The fence keeps the first claim's token hash and holder: only `fenced_at` changes.
    assert_eq!(claims[0].token_hash, format!("{:064x}", 1));
    assert_eq!(claims[0].holder, "ann@host");
}

#[test]
fn a_claim_with_a_short_hash_is_refused_by_the_table() {
    let (_dir, store) = store();
    let mut bad = claim(1, None);
    bad.token_hash = "abc".to_string();
    assert!(commit(&store, "claim:1", vec![Effect::PutClaim(bad)]).is_err());
    assert!(store.claims().unwrap().is_empty());
}

//! AC-12: a third model session waits while two hold slots, and takes a slot when one is
//! released. A second cold build waits while one holds its slot, and takes it when released. A
//! setting of 3 sessions admits a third.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use pf_engine::capacity::{Capacity, Settings};
use tokio::time::{sleep, timeout};

const SHORT: Duration = Duration::from_millis(60);
const LONG: Duration = Duration::from_secs(5);

#[test]
fn the_defaults_are_two_model_sessions_and_one_cold_build() {
    let s = Settings::default();
    assert_eq!((s.model_sessions, s.cold_builds), (2, 1));
    let cap = Capacity::new(&s).unwrap();
    assert_eq!(cap.free_model_sessions(), 2);
    assert_eq!(cap.free_cold_builds(), 1);
}

#[test]
fn a_setting_of_zero_is_refused() {
    for (m, c) in [(0, 1), (1, 0)] {
        let err = Capacity::new(&Settings {
            model_sessions: m,
            cold_builds: c,
        })
        .unwrap_err();
        assert!(err.to_string().contains("at least 1"), "{err}");
    }
}

#[tokio::test]
async fn a_third_model_session_waits_while_two_hold_slots_and_takes_one_when_released() {
    let cap = Capacity::new(&Settings::default()).unwrap();
    let first = cap.acquire_model_session().await;
    let _second = cap.acquire_model_session().await;
    assert_eq!(cap.free_model_sessions(), 0);
    assert!(cap.try_acquire_model_session().is_none());

    // The third waits while two hold slots.
    assert!(
        timeout(SHORT, cap.acquire_model_session()).await.is_err(),
        "a third session must wait"
    );

    let waiter = {
        let cap = cap.clone();
        tokio::spawn(async move { cap.acquire_model_session().await })
    };
    sleep(SHORT).await;
    assert!(!waiter.is_finished());

    // A released slot is taken by the waiting holder.
    drop(first);
    let third = timeout(LONG, waiter)
        .await
        .expect("the waiter takes the slot")
        .unwrap();
    assert_eq!(cap.free_model_sessions(), 0);
    drop(third);
    assert_eq!(cap.free_model_sessions(), 1);
}

#[tokio::test]
async fn a_released_slot_goes_to_the_waiter_that_came_first() {
    let cap = Capacity::new(&Settings::default()).unwrap();
    let held = [
        cap.acquire_model_session().await,
        cap.acquire_model_session().await,
    ];
    let order = Arc::new(Mutex::new(Vec::new()));
    let mut waiters = Vec::new();
    for name in ["first", "second"] {
        let cap = cap.clone();
        let order = order.clone();
        waiters.push(tokio::spawn(async move {
            let slot = cap.acquire_model_session().await;
            order.lock().unwrap().push(name);
            slot
        }));
        sleep(SHORT).await; // let this waiter queue before the next one starts
    }
    let [a, b] = held;
    drop(a);
    let first_slot = timeout(LONG, waiters.remove(0)).await.unwrap().unwrap();
    assert_eq!(*order.lock().unwrap(), vec!["first"]);
    assert!(!waiters[0].is_finished(), "the second waiter keeps waiting");
    drop(b);
    let second_slot = timeout(LONG, waiters.remove(0)).await.unwrap().unwrap();
    assert_eq!(*order.lock().unwrap(), vec!["first", "second"]);
    drop((first_slot, second_slot));
}

#[tokio::test]
async fn a_second_cold_build_waits_while_one_holds_its_slot_and_takes_it_when_released() {
    let cap = Capacity::new(&Settings::default()).unwrap();
    let build = cap.acquire_cold_build().await;
    assert!(cap.try_acquire_cold_build().is_none());
    assert!(timeout(SHORT, cap.acquire_cold_build()).await.is_err());

    let waiter = {
        let cap = cap.clone();
        tokio::spawn(async move { cap.acquire_cold_build().await })
    };
    sleep(SHORT).await;
    assert!(!waiter.is_finished());
    drop(build);
    let second = timeout(LONG, waiter)
        .await
        .expect("the waiter takes the slot")
        .unwrap();
    assert_eq!(cap.free_cold_builds(), 0);
    drop(second);
    assert_eq!(cap.free_cold_builds(), 1);
}

#[tokio::test]
async fn model_sessions_and_cold_builds_have_separate_slots() {
    let cap = Capacity::new(&Settings::default()).unwrap();
    let _a = cap.acquire_model_session().await;
    let _b = cap.acquire_model_session().await;
    // Two model sessions do not use the cold build slot.
    assert!(timeout(SHORT, cap.acquire_cold_build()).await.is_ok());
}

#[tokio::test]
async fn a_setting_of_three_sessions_admits_a_third() {
    let cap = Capacity::new(&Settings {
        model_sessions: 3,
        cold_builds: 1,
    })
    .unwrap();
    let a = cap.try_acquire_model_session();
    let b = cap.try_acquire_model_session();
    let c = timeout(SHORT, cap.acquire_model_session()).await;
    assert!(
        a.is_some() && b.is_some() && c.is_ok(),
        "three slots admit three"
    );
    assert!(cap.try_acquire_model_session().is_none(), "a fourth waits");
}

#[tokio::test]
async fn the_engine_takes_its_slots_from_the_workflow_settings() {
    let mut wf = workflow(&["build"], false);
    wf.settings = Settings {
        model_sessions: 3,
        cold_builds: 2,
    };
    let env = env(wf);
    assert_eq!(env.engine.capacity().free_model_sessions(), 3);
    assert_eq!(env.engine.capacity().free_cold_builds(), 2);

    let mut bad = workflow(&["build"], false);
    bad.settings.model_sessions = 0;
    let dir = tempfile::tempdir().unwrap();
    let refused = pf_engine::Engine::new(
        config(dir.path(), bad),
        FakeStore::new(),
        FakeAgent::new(),
        FakeForge::new(),
        counting_clock(),
        Box::new(pf_engine::ledger::FsLedger::new(dir.path().join("l.md"))),
    );
    assert!(refused.is_err());
}

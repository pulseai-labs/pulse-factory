//! AC-12 of `r1.s3.w3`: `device pair` makes no code while the API is `failed`. The controller runs
//! in the test process with the API on a runtime of the test's own. Nothing here reaches beyond
//! `127.0.0.1`.

mod common;

use std::net::TcpListener;
use std::time::{Duration, Instant};

use common::ctl::Live;
use common::*;
use pf_api::{ApiHost, Refusal};
use pulse_factory::controller::api::{self, ApiBridge};
use serde_json::{Value, json};

/// A live controller with its API on `listen`, served by a runtime of its own.
struct Served {
    live: Live,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Served {
    fn drop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn serve(listen: String) -> Served {
    let live = Live::start_config(BIN.into(), move |cfg| cfg.api_listen = Some(listen));
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let controller = live.controller.clone();
    let thread = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let _api = api::spawn_api(&controller);
            let _ = rx.await;
        });
    });
    Served {
        live,
        stop: Some(tx),
        thread: Some(thread),
    }
}

impl Served {
    fn state(&self) -> Value {
        pf_ipc::call(&self.live.sb.state, "api_status", json!({}))
            .unwrap()
            .result
    }

    fn wait_state(&self, want: &str) {
        let end = Instant::now() + Duration::from_secs(10);
        while self.state()["state"] != want {
            assert!(
                Instant::now() < end,
                "the API never reached {want}: {}",
                self.state()
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

#[test]
fn a_failed_api_makes_no_code_and_says_why() {
    // Another listener holds the port.
    let holder = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = holder.local_addr().unwrap().to_string();
    let served = serve(addr.clone());
    served.wait_state("failed");

    let out = served.live.cli(&["api", "status"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(
        stdout(&out).lines().next().unwrap_or("").contains("failed"),
        "{}",
        stdout(&out)
    );

    let out = served.live.cli(&["device", "pair"]);
    assert_eq!(code(&out), 1, "stdout: {}", stdout(&out));
    assert!(
        stderr(&out).contains("the API cannot listen:"),
        "{}",
        stderr(&out)
    );
    assert!(stderr(&out).contains("fix it and restart the controller"));
    assert!(
        stdout(&out).is_empty(),
        "no code was printed: {}",
        stdout(&out)
    );

    // The JSON form refuses the same way.
    let out = served.live.cli(&["device", "pair", "--json"]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("the API cannot listen:"));

    // No code is live: a redeem of any secret is refused.
    let host = ApiBridge::new(served.live.controller.clone());
    let any_secret = "ab".repeat(32);
    let refused = host.redeem(&any_secret, "macbook", "127.0.0.1".parse().unwrap());
    assert!(matches!(refused, Err(Refusal::Code)), "{refused:?}");
    // And nothing was written for a pairing: the controller ledger holds no device line.
    let ledger =
        std::fs::read_to_string(served.live.sb.state.join("controller.ledger")).unwrap_or_default();
    assert!(!ledger.contains("device_pair"), "{ledger}");
    drop(holder);
}

#[test]
fn a_waiting_api_still_makes_a_code() {
    // A tailnet address that is not on this host: the bind waits for it.
    let served = serve("100.100.1.1:48765".to_string());
    served.wait_state("waiting");
    let out = served.live.cli(&["device", "pair", "--json"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let v: Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert!(
        v["code"].as_str().is_some_and(|c| c.starts_with("pf1-")),
        "{v}"
    );
}

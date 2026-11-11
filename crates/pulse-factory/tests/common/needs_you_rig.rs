//! The spine rig with the API on `127.0.0.1:0`, shared by `run_list.rs` and `client_requests.rs`
//! (`r1.s4.w3`). It is included with `#[path]` and is no module of `common`, so a file here never
//! gets a test crate of its own. Nothing here starts a seat process, spends or reaches the network.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use pf_adapter_fake::{Scenario, Step};
use pf_engine::port::store::{RequestKind, RequestStatus};
use pf_forge_bare::PrChecks;
use pulse_factory::controller::api;
use serde_json::json;

use crate::common::chat::lines;
use crate::common::spine::*;

/// The spine rig with the API listening, on a runtime of its own.
pub struct Api {
    pub s: Spine,
    pub addr: SocketAddr,
    /// The SHA-256 of the server's key, as `api_status` shows it.
    pub spki: String,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Api {
    fn drop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub fn api() -> Api {
    let s = Spine::start_with(|cfg| cfg.api_listen = Some("127.0.0.1:0".to_string()));
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let controller = s.live.controller.clone();
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
    let status = || {
        pf_ipc::call(&s.live.sb.state, "api_status", json!({}))
            .unwrap()
            .result
    };
    let end = Instant::now() + Duration::from_secs(10);
    while status()["state"] != "listening" {
        assert!(Instant::now() < end, "the API never listened: {}", status());
        std::thread::sleep(Duration::from_millis(25));
    }
    let st = status();
    let addr: SocketAddr = st["listen"].as_str().unwrap().parse().unwrap();
    let spki = st["key_spki_sha256"].as_str().unwrap().to_string();
    Api {
        s,
        addr,
        spki,
        stop: Some(tx),
        thread: Some(thread),
    }
}

impl Api {
    /// A one-use pairing code, through the socket as the CLI does.
    pub fn pair_code(&self) -> String {
        let made = pf_ipc::call(&self.s.live.sb.state, "device_pair", json!({})).unwrap();
        assert!(made.ok, "{:?}", made.error);
        made.result["code"].as_str().unwrap().to_string()
    }

    pub fn api_ledger(&self) -> Vec<String> {
        lines(&self.s.live.sb.state.join("api.ledger"))
    }

    /// An implement seat writes its stop file and ends its turn: a stop request opens. Returns its ID.
    pub fn open_stop(&self, question: &str) -> String {
        let s = &self.s;
        s.item("u1", 1, 1);
        s.tick();
        std::fs::create_dir_all(s.out("u1", "")).unwrap();
        std::fs::write(s.out("u1", "stop"), format!("{question}\n")).unwrap();
        s.live.ports.fake.finish(&s.session_of("u1"));
        s.tick();
        let open: Vec<_> = s
            .requests(RequestKind::Stop)
            .into_iter()
            .filter(|r| r.status == RequestStatus::Open)
            .collect();
        assert_eq!(open.len(), 1, "{open:?}");
        open[0].id.clone()
    }

    /// Drive the run, as `spine_close.rs` does, to an open `merge_ack` on the spine PR (#9,
    /// `spine/s1` into `main`). The two items have merged and the lane's first unit has ended.
    /// Returns the request ID and the head SHA.
    pub fn to_merge_ack(&self) -> (String, String) {
        let s = &self.s;
        let fake = &s.live.ports.fake;
        fake.script("lane", Scenario::Script(vec![cost(1_000), Step::TurnEnd]));
        fake.script("lane.2", Scenario::Script(vec![cost(2_000), Step::TurnEnd]));
        fake.script("spine", Scenario::Script(vec![cost(7_000), Step::TurnEnd]));
        s.tick();
        s.item("u1", 1, 1);
        s.item("u2", 2, 2);
        s.tick();
        s.finish_item("u1", 1);
        s.finish_item("u2", 2);
        s.pass("lane", &s.out("lane", "lane.done"));
        s.tick();
        assert!(s.has_unit("lane.2"));
        s.touch(&s.run_dir().join("rounds.done"));
        s.tick();
        assert!(s.has_unit("spine"));
        s.tick();
        s.forge
            .write_pr(9, SPINE_BRANCH, "main", PrChecks::Green)
            .unwrap();
        let head = s.head(SPINE_BRANCH);
        std::fs::create_dir_all(s.out("spine", "")).unwrap();
        std::fs::write(
            s.out("spine", "pr.url"),
            format!("file://{}#9\n", s.forge.repo()),
        )
        .unwrap();
        s.live.ports.fake.finish(&s.session_of("spine"));
        s.tick();
        let open = s.requests(RequestKind::MergeAck);
        assert_eq!(open.len(), 1, "{open:?}");
        (open[0].id.clone(), head)
    }

    /// After the merge: the lane's second unit ends and a tick notes the `end_when` waiting text.
    pub fn wait_for_the_close(&self) {
        let s = &self.s;
        s.pass("lane.2", &s.out("lane.2", "lane.done"));
        s.tick();
    }

    /// The operator's close: the `end_when` file, then a tick.
    pub fn close(&self) {
        let s = &self.s;
        s.touch(&s.run_dir().join("closed.flag"));
        s.tick();
    }
}

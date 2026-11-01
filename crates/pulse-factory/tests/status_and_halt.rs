//! AC-8: `status` and `halt` through the socket, and what a restart does. The controller runs in
//! the test process on a `FakeAdapter`, and the CLI runs as a child process against its socket.

mod common;

use common::ctl::*;
use common::rig::*;
use common::*;
use pf_adapter_fake::Step;
use pf_engine::port::agent::{SessionRef, Usage};
use pf_forge_bare::PrChecks;
use serde_json::{Value, json};

fn status(sb: &Sandbox, extra: &[&str]) -> Value {
    let mut args: Vec<String> = vec!["status".into()];
    args.extend(extra.iter().map(|s| (*s).to_string()));
    args.push("--json".into());
    let out = sb.run(&args);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    serde_json::from_str(&stdout(&out)).unwrap()
}

fn usage(i: u64, o: u64, cr: u64, cw: u64, cost: u64) -> Step {
    Step::Usage(Usage {
        input_tokens: i,
        output_tokens: o,
        cache_read_tokens: cr,
        cache_write_tokens: cw,
        cost_micros: cost,
    })
}

fn queue_item(live: &Live, id: &str, order: u32) {
    let wt = live.sb.spine_dir.join(format!("wt-{id}"));
    std::fs::create_dir_all(&wt).unwrap();
    let rec = pf_engine::port::unit_source::UnitRecord::new(
        id,
        "g1",
        order,
        [
            ("worktree_path".to_string(), wt.display().to_string()),
            ("branch".to_string(), format!("work/{id}")),
        ]
        .into(),
    );
    std::fs::write(
        live.run_dir().join(format!("queue/{id}.json")),
        rec.to_json(),
    )
    .unwrap();
}

fn refs(live: &Live) -> Vec<SessionRef> {
    live.handle()
        .engine
        .view()
        .unwrap()
        .sessions
        .iter()
        .filter_map(|s| s.process_ref.clone().map(SessionRef))
        .collect()
}

#[test]
fn status_json_holds_every_field_and_the_total_is_the_fixed_number() {
    let live = Live::start();
    live.arm();
    live.ports.fake.script(
        "lane",
        pf_adapter_fake::Scenario::Script(vec![usage(100, 50, 10, 5, 1_234_567)]),
    );
    live.ports.fake.script(
        "u1",
        pf_adapter_fake::Scenario::Script(vec![usage(7, 3, 0, 0, 7_654_321)]),
    );
    queue_item(&live, "u1", 1);
    live.controller.tick_all();
    for r in refs(&live) {
        live.ports.fake.finish(&r);
    }
    live.controller.tick_all();
    let v = status(&live.sb, &[]);
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["run"], "s1-1");
    assert_eq!(v["env"], "live");
    assert_eq!(v["state"], "active");
    for key in [
        "halted_reason",
        "degraded",
        "notes",
        "units",
        "sessions",
        "total",
        "end_reason",
    ] {
        assert!(v.get(key).is_some(), "status --json lacks {key}: {v}");
    }
    let sessions = v["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    let s = sessions.iter().find(|s| s["unit"] == "u1").unwrap();
    assert_eq!(s["stage"], "implement");
    assert_eq!(s["role"], "implementer");
    assert_eq!(s["model"], "model-b");
    assert_eq!(s["runtime"], "omp");
    assert_eq!(s["status"], "running");
    assert_eq!(
        s["tokens"],
        json!({"input": 7, "output": 3, "cache_read": 0, "cache_write": 0})
    );
    assert_eq!(s["cost_micros"], 7_654_321);
    assert_eq!(s["cost"], "$7.654321");
    let unit = v["units"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["id"] == "u1")
        .unwrap()
        .clone();
    assert_eq!(unit["stage"], "implement");
    assert_eq!(unit["status"], "running");
    assert_eq!(v["total"]["cost_micros"], 8_888_888);
    assert_eq!(v["total"]["cost"], "$8.888888");
    assert_eq!(
        v["total"]["tokens"],
        json!({"input": 107, "output": 53, "cache_read": 10, "cache_write": 5})
    );

    // The text form shows the same facts.
    let out = live.sb.run(&["status".into()]);
    let text = stdout(&out);
    for want in [
        "run: s1-1",
        "env: live",
        "state: active",
        "unit u1: stage implement, running",
        "role implementer, model model-b, runtime omp",
        "cost $7.654321",
        "total cost: $8.888888",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }
}

#[test]
fn status_shows_a_tick_note_and_a_degraded_reason() {
    let live = Live::start();
    live.arm();
    std::fs::write(live.run_dir().join("queue/bad.json"), "not json").unwrap();
    live.controller.tick_all();
    let v = status(&live.sb, &[]);
    assert!(
        v["notes"].to_string().contains("queue halted"),
        "{}",
        v["notes"]
    );
    assert!(
        v["halted_reason"].as_str().unwrap().contains("bad.json"),
        "{v}"
    );
    // A ledger that cannot be written puts the engine on its degraded path.
    let ledger = live.sb.ledger.clone();
    std::fs::remove_file(&ledger).unwrap();
    std::fs::create_dir(&ledger).unwrap();
    let out = live.sb.run(&["halt".into(), "--reason".into(), "x".into()]);
    let _ = out;
    let v = status(&live.sb, &[]);
    assert!(
        v["degraded"].is_string(),
        "the degraded reason is shown: {v}"
    );
    let text = stdout(&live.sb.run(&["status".into()]));
    assert!(text.contains("degraded:"), "{text}");
}

#[test]
fn status_on_an_unknown_run_or_with_no_controller_exits_non_zero() {
    let live = Live::start();
    live.arm();
    let out = live.sb.run(&["status".into(), "nonesuch-9".into()]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("nonesuch-9"), "{}", stderr(&out));
    drop(live);
    let sb = Sandbox::new();
    let out = sb.run(&["status".into()]);
    assert_eq!(code(&out), 1);
    assert!(
        stderr(&out).contains("systemctl --user status pulse-factory"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn halt_stops_the_next_launch_repeats_safely_and_keeps_the_first_reason() {
    let live = Live::start();
    live.arm();
    live.ports.fake.script(
        "lane",
        pf_adapter_fake::Scenario::Script(vec![Step::TurnEnd]),
    );
    live.controller.tick_all();
    assert_eq!(live.ports.fake.launch_count(), 1, "the lane launched");
    let out = live
        .sb
        .run(&["halt".into(), "--reason".into(), "first reason".into()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    // The first halt says it halted the run. It does not claim an earlier halt.
    assert!(
        stdout(&out).contains("run s1-1 halted"),
        "the first halt prints the halt line: {}",
        stdout(&out)
    );
    assert!(
        !stdout(&out).contains("already"),
        "the first halt is not a repeat: {}",
        stdout(&out)
    );
    let again = live
        .sb
        .run(&["halt".into(), "--reason".into(), "second reason".into()]);
    assert_eq!(code(&again), 0, "a repeat is safe: {}", stderr(&again));
    assert!(
        stdout(&again).contains("first reason"),
        "{}",
        stdout(&again)
    );
    assert!(
        stdout(&again).contains("halted already"),
        "a repeat says so: {}",
        stdout(&again)
    );
    assert!(
        !stdout(&again).contains("second reason"),
        "{}",
        stdout(&again)
    );
    assert_eq!(status(&live.sb, &[])["halted_reason"], "first reason");

    queue_item(&live, "u1", 1);
    live.controller.tick_all();
    assert_eq!(live.ports.fake.launch_count(), 1, "nothing new launches");

    // A running seat's signal is still recorded.
    std::fs::create_dir_all(live.run_dir().join("out/lane")).unwrap();
    std::fs::write(live.run_dir().join("out/lane/lane.done"), "x").unwrap();
    for r in refs(&live) {
        live.ports.fake.finish(&r);
    }
    live.controller.tick_all();
    let v = status(&live.sb, &[]);
    let lane = v["units"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["id"] == "lane")
        .unwrap()
        .clone();
    assert_eq!(lane["status"], "done", "{v}");
}

#[test]
fn halt_json_carries_already_halted_false_on_the_first_halt_and_true_on_a_repeat() {
    let live = Live::start();
    live.arm();
    let halt = |reason: &str| -> Value {
        let out = live.sb.run(&[
            "halt".into(),
            "--reason".into(),
            reason.into(),
            "--json".into(),
        ]);
        assert_eq!(code(&out), 0, "{}", stderr(&out));
        serde_json::from_str(&stdout(&out)).unwrap()
    };
    let first = halt("first reason");
    assert_eq!(first["schema_version"], 1, "{first}");
    assert_eq!(first["run"], "s1-1", "{first}");
    assert_eq!(first["halted"], true, "{first}");
    assert_eq!(first["already_halted"], false, "{first}");
    let again = halt("second reason");
    assert_eq!(again["halted"], true, "{again}");
    assert_eq!(again["already_halted"], true, "{again}");
    assert_eq!(again["reason"], "first reason", "{again}");
}

#[test]
fn halt_with_a_seat_token_is_refused() {
    let live = Live::start();
    live.arm();
    let out = live
        .sb
        .cmd(&["halt", "--reason", "x"])
        .env("PF_SEAT_TOKEN", "secret")
        .output()
        .unwrap();
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("PF_SEAT_TOKEN"), "{}", stderr(&out));
    assert!(!stderr(&out).contains("secret"));
    assert!(status(&live.sb, &[])["halted_reason"].is_null());
}

#[test]
fn a_halt_stops_the_next_merge() {
    let r = Rig::new();
    r.tick();
    r.item("u1", 1, 1, PrChecks::Green);
    r.reach_close("u1");
    r.pr_url("u1", r.forge.repo(), 1);
    r.engine.halt("operator stop").unwrap();
    r.fake.finish(&r.session_ref("u1").unwrap());
    r.tick();
    r.tick();
    assert_eq!(
        r.unit("u1").0,
        pf_engine::status::Status::Running,
        "{}",
        r.ledger_text()
    );
    assert!(
        !r.ledger_text().contains("merge of unit"),
        "{}",
        r.ledger_text()
    );
}

#[test]
fn a_restart_parks_a_running_unit_relaunches_nothing_and_ends_a_stale_session() {
    let live = Live::start();
    let run = live.arm();
    live.controller.tick_all();
    // The lane runs. A second unit is parked by hand, with its session left open.
    queue_item(&live, "u1", 1);
    live.controller.tick_all();
    live.handle().engine.park("u1", "parked by hand").unwrap();
    let open = |c: &pulse_factory::controller::Controller| {
        c.store()
            .sessions(&run)
            .map(|s| s.iter().filter(|x| !x.ended).count())
    };
    use pf_engine::port::store::Store;
    assert_eq!(open(&live.controller).unwrap(), 2);

    // A restart: a new controller and a new adapter that knows no reference.
    let fake = std::sync::Arc::new(pf_adapter_fake::FakeAdapter::new());
    fake.add_plugin(
        "methodkit",
        "1.26.1",
        live.sb.plugin_dir("methodkit", "1.26.1").to_str().unwrap(),
    );
    let ports = std::sync::Arc::new(TestPorts {
        fake: fake.clone(),
        plans: Default::default(),
        gate: Default::default(),
        block_run_json: Default::default(),
    });
    let second = pulse_factory::controller::Controller::open(
        pulse_factory::controller::Config {
            state_dir: live.sb.state.clone(),
            tick_ms: 3_600_000,
            binary: BIN.into(),
        },
        ports,
    )
    .unwrap();
    let view = second.handles()[0].engine.view().unwrap();
    assert_eq!(
        view.node("lane").unwrap().status,
        pf_engine::status::Status::Parked,
        "a running unit whose seat is gone parks"
    );
    assert_eq!(
        view.node("lane").unwrap().reason.as_deref(),
        Some("seat lost across a controller restart")
    );
    assert_eq!(
        open(&second).unwrap(),
        0,
        "no session stays open under a parked unit"
    );
    second.tick_all();
    assert_eq!(fake.launch_count(), 0, "nothing relaunches");
    // The stale session does not make arm refuse: the run is replaced.
    let mut plan = live.plan();
    plan.replace = Some(run.clone());
    let replaced = second.arm(plan).unwrap();
    assert_eq!(replaced, "s1-2");
}

#[test]
fn a_second_controller_refuses_and_a_stale_socket_does_not_stop_one() {
    let sb = Sandbox::new();
    let first = Ctl::start(&sb, 3_600_000);
    let second = sb.cmd(&["controller"]).output().unwrap();
    assert_ne!(code(&second), 0, "a second controller must refuse");
    assert!(
        stderr(&second).contains("controller"),
        "{}",
        stderr(&second)
    );
    // The first is killed hard: its socket file stays behind, and nothing answers on it.
    let pid = first.pid();
    std::mem::forget(first);
    let _ = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status();
    for _ in 0..100 {
        if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        sb.state.join("controller.sock").exists(),
        "a stale socket is left"
    );
    let third = Ctl::start(&sb, 3_600_000);
    assert!(pf_ipc::call(&sb.state, "runs", json!({})).is_ok());
    drop(third);
}

//! AC-7: `SeatPort` adds the run's facts to every launch, and the seat adapters end their
//! processes. Nothing here launches a real seat: the stub `omp` is a shell script.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;

use common::ctl::*;
use common::*;
use pf_adapter_fake::FakeAdapter;
use pf_engine::port::agent::*;
use pf_workflow::rules::RoleRules;
use pulse_factory::controller::ports::{LivePorts, Ports, RunSpec, omp_config};
use pulse_factory::controller::seat::{SeatFacts, SeatPort, SeatTable};
use serde_json::json;

fn rules_of(role: &str, allow: &[&str]) -> RoleRules {
    let list: String = allow.iter().map(|a| format!("  - {a}\n")).collect();
    RoleRules::parse(
        &format!("version: 1\nrole: {role}\nallow:\n{list}"),
        &format!("{role}.yaml"),
    )
    .unwrap()
}

fn facts(dir: &std::path::Path) -> SeatFacts {
    let mut rules = BTreeMap::new();
    rules.insert(
        "implementer".to_string(),
        (
            dir.join("package/rules/implementer.yaml"),
            rules_of("implementer", &["delete_own_scratch"]),
        ),
    );
    rules.insert(
        "close".to_string(),
        (
            dir.join("package/rules/close.yaml"),
            rules_of("close", &["push_own_branch"]),
        ),
    );
    SeatFacts {
        repo: "acme/widgets".into(),
        remote: "origin".into(),
        default_branch: "main".into(),
        forge_api_hosts: "api.github.com".into(),
        run_dir: dir.to_path_buf(),
        state_dir: dir.join("state"),
        target_dir: String::new(),
        plugins: vec![dir.join("plugins/methodkit")],
        rules,
        binary: PathBuf::from("/bin/pf"),
    }
}

fn plan(role: &str, settings: &[(&str, &str)]) -> LaunchPlan {
    LaunchPlan {
        run_id: "r1".into(),
        unit_id: "u1".into(),
        stage: "s".into(),
        role: role.into(),
        model: "m".into(),
        runtime: "omp".into(),
        attempt: 1,
        settings: settings
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect(),
    }
}

/// A plan and a `SeatPort` over a recording fake.
fn port(
    dir: &std::path::Path,
    f: SeatFacts,
) -> (
    SeatPort,
    Arc<FakeAdapter>,
    Arc<std::sync::Mutex<Vec<LaunchPlan>>>,
) {
    let fake = Arc::new(FakeAdapter::new());
    let plans = Arc::new(std::sync::Mutex::new(Vec::new()));
    let rec = Arc::new(Recording {
        inner: fake.clone(),
        plans: plans.clone(),
        gate: Arc::new(Gate::default()),
    });
    let _ = dir;
    (
        SeatPort::new(rec, f, Arc::new(SeatTable::default())),
        fake,
        plans,
    )
}

#[test]
fn seat_port_adds_every_setting_with_its_value() {
    let dir = tempfile::tempdir().unwrap();
    let f = facts(dir.path());
    let (p, fake, plans) = port(dir.path(), f);
    let report = dir.path().join("spine/work-u1").display().to_string();
    let wt = dir.path().join("wt").display().to_string();
    let input = plan(
        "implementer",
        &[
            ("report_dir", &report),
            ("worktree", &wt),
            ("own_branch", "work/u1"),
            ("cwd", &wt),
        ],
    );
    p.launch(&input).unwrap();
    p.launch(&input).unwrap();
    let plans = plans.lock().unwrap();
    let got = &plans[0].settings;
    let run = dir.path().display().to_string();
    let expect = [
        ("plugins", format!("{run}/plugins/methodkit")),
        ("repo", "acme/widgets".to_string()),
        ("remote", "origin".to_string()),
        ("default_branch", "main".to_string()),
        ("forge_api_hosts", "api.github.com".to_string()),
        (
            "rules_file",
            format!("{run}/package/rules/implementer.yaml"),
        ),
        ("scratch_dir", format!("{run}/out/u1/scratch")),
        ("run_dir", run.clone()),
        ("target_dir", String::new()),
        ("report_dir", report.clone()),
        ("worktree", wt.clone()),
        ("own_branch", "work/u1".to_string()),
        ("own_tag", String::new()),
        ("env.PF_HOOK_CMD", "'/bin/pf' hook".to_string()),
        ("env.PF_STATE_DIR", format!("{run}/state")),
    ];
    for (k, v) in expect {
        assert_eq!(got.get(k), Some(&v), "setting {k}");
    }
    assert!(
        std::path::Path::new(&got["scratch_dir"]).is_dir(),
        "the scratch directory is made"
    );
    let (k1, k2) = (
        &plans[0].settings["env.PF_SESSION_KEY"],
        &plans[1].settings["env.PF_SESSION_KEY"],
    );
    assert!(!k1.is_empty() && k1 != k2, "a fresh key for each launch");
    assert_eq!(fake.launch_count(), 2);
}

#[test]
fn a_lane_has_an_empty_worktree_and_an_item_has_its_own() {
    let live = Live::start();
    live.arm();
    let spine = live.sb.spine_dir.display().to_string();
    // The lane seed launches in the first tick.
    live.controller.tick_all();
    let lane = live.last_plan();
    assert_eq!(lane.role, "lane");
    assert_eq!(lane.settings["worktree"], "");
    assert_eq!(lane.settings["report_dir"], spine);
    assert_eq!(lane.settings["forge_api_hosts"], "forge.invalid");
    // An item has its own worktree and report directory.
    let wt = live.sb.spine_dir.join("wt-u1");
    std::fs::create_dir_all(&wt).unwrap();
    let rec = pf_engine::port::unit_source::UnitRecord::new(
        "u1",
        "g1",
        1,
        [
            ("worktree_path".to_string(), wt.display().to_string()),
            ("branch".to_string(), "work/u1".to_string()),
        ]
        .into(),
    );
    std::fs::write(live.run_dir().join("queue/u1.json"), rec.to_json()).unwrap();
    live.controller.tick_all();
    let item = live.last_plan();
    assert_eq!(item.role, "implementer");
    assert_eq!(item.settings["worktree"], wt.display().to_string());
    assert_eq!(item.settings["own_branch"], "work/u1");
    assert_eq!(item.settings["report_dir"], format!("{spine}/work-u1"));
    assert_ne!(
        lane.settings["env.PF_SESSION_KEY"],
        item.settings["env.PF_SESSION_KEY"]
    );
}

#[test]
fn every_refusal_is_refused_with_no_process_started() {
    let dir = tempfile::tempdir().unwrap();
    let report = dir.path().join("rep").display().to_string();
    let good = |role| {
        plan(
            role,
            &[
                ("report_dir", &report),
                ("push_dir", "/x"),
                ("push_branch", "b"),
            ],
        )
    };
    type Edit = fn(&mut SeatFacts);
    let edits: [(&str, Edit); 6] = [
        ("repo", |f| f.repo.clear()),
        ("remote", |f| f.remote.clear()),
        ("default_branch", |f| f.default_branch.clear()),
        ("forge_api_hosts", |f| f.forge_api_hosts.clear()),
        ("run_dir", |f| f.run_dir = PathBuf::new()),
        ("rules_file", |f| {
            f.rules.get_mut("implementer").unwrap().0 = PathBuf::new();
        }),
    ];
    for (name, edit) in edits {
        let mut f = facts(dir.path());
        edit(&mut f);
        let (p, fake, _) = port(dir.path(), f);
        let err = p.launch(&good("implementer")).unwrap_err();
        assert!(matches!(err, AgentError::Refused(_)), "{name}: {err:?}");
        assert!(err.to_string().contains(name), "{name}: {err}");
        assert_eq!(fake.launch_count(), 0, "{name}: a process started");
    }
    // report_dir missing or empty.
    let (p, fake, _) = port(dir.path(), facts(dir.path()));
    for settings in [vec![], vec![("report_dir", "")]] {
        let err = p.launch(&plan("implementer", &settings)).unwrap_err();
        assert!(
            matches!(err, AgentError::Refused(_)) && err.to_string().contains("report_dir"),
            "{err}"
        );
    }
    // A role that may push its own branch needs a push_dir and a push_branch.
    for settings in [
        vec![("report_dir", report.as_str())],
        vec![("report_dir", report.as_str()), ("push_dir", "/x")],
    ] {
        let err = p.launch(&plan("close", &settings)).unwrap_err();
        assert!(
            matches!(err, AgentError::Refused(_)) && err.to_string().contains("push_dir"),
            "{err}"
        );
    }
    assert_eq!(fake.launch_count(), 0);
    // An empty own_tag, worktree and target_dir are accepted.
    p.launch(&plan(
        "implementer",
        &[("report_dir", &report), ("own_tag", "")],
    ))
    .unwrap();
    assert_eq!(fake.launch_count(), 1);
}

#[test]
fn the_key_never_reaches_status_or_a_park_reason() {
    let live = Live::start();
    live.arm();
    let settings = live.role_settings("implementer", "u1");
    let plan = live.launch("implementer", "u1", &settings).unwrap();
    let key = plan.settings["env.PF_SESSION_KEY"].clone();
    live.controller.tick_all();
    // A launch the adapter refuses parks the unit with a reason.
    live.ports.fake.script(
        "u2",
        pf_adapter_fake::Scenario::RefusedLaunch("no capacity here".into()),
    );
    let rec = pf_engine::port::unit_source::UnitRecord::new(
        "u2",
        "g1",
        2,
        [
            (
                "worktree_path".to_string(),
                live.sb.spine_dir.display().to_string(),
            ),
            ("branch".to_string(), "work/u2".to_string()),
        ]
        .into(),
    );
    std::fs::write(live.run_dir().join("queue/u2.json"), rec.to_json()).unwrap();
    live.controller.tick_all();
    let status = pf_ipc::call(&live.sb.state, "status", json!({})).unwrap();
    let text = serde_json::to_string(&status.result).unwrap();
    assert!(
        text.contains("no capacity here"),
        "the park reason is in status: {text}"
    );
    assert!(!text.contains(&key), "the key must not reach status");
    assert!(!live.sb.read_ledger().contains(&key));
}

#[test]
fn the_contract_file_names_every_key_the_code_adds() {
    let dir = tempfile::tempdir().unwrap();
    let (p, _, plans) = port(dir.path(), facts(dir.path()));
    let report = dir.path().join("rep").display().to_string();
    let input = plan("implementer", &[("report_dir", &report)]);
    p.launch(&input).unwrap();
    let added: BTreeSet<String> = plans.lock().unwrap()[0]
        .settings
        .keys()
        .filter(|k| !input.settings.contains_key(*k))
        .cloned()
        .collect();
    assert!(added.len() >= 14, "{added:?}");
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/contracts/launch-settings.md"
    ))
    .unwrap();
    for key in added.iter().chain(
        [
            "worktree",
            "own_branch",
            "decisions_file",
            "report_dir",
            "env.PI_EDIT_VARIANT",
        ]
        .iter()
        .map(|s| &**s)
        .map(str::to_string)
        .collect::<Vec<_>>()
        .iter(),
    ) {
        assert!(
            text.contains(&format!("`{key}`")),
            "launch-settings.md does not name {key}"
        );
    }
}

#[test]
fn stop_on_an_unknown_or_untagged_reference_is_ok() {
    let router = AgentRouter::new();
    assert!(router.stop(&SessionRef("nope".into())).is_ok(), "untagged");
    assert!(
        router.stop(&SessionRef("omp:nope".into())).is_ok(),
        "unknown runtime"
    );
    let fake = FakeAdapter::new();
    assert!(fake.stop(&SessionRef("fake-9".into())).is_ok());
    let dir = tempfile::tempdir().unwrap();
    let omp = pf_adapter_omp::OmpAdapter::new(pf_adapter_omp::OmpConfig::new(dir.path()));
    assert!(omp.stop(&SessionRef("omp-9".into())).is_ok());
    assert_eq!(omp.is_alive(&SessionRef("omp-9".into())), Ok(false));
    assert_eq!(fake.is_alive(&SessionRef("fake-9".into())), Ok(false));
}

#[test]
fn the_omp_adapter_gets_its_roots_and_the_live_forge_has_hosts() {
    let spec = RunSpec {
        run_id: "r1".into(),
        run_dir: PathBuf::from("/runs/r1"),
        runtimes: vec!["omp".into()],
        plugin_roots: vec![PathBuf::from("/plugins")],
        paper: None,
        forge_repo: "acme/widgets".into(),
    };
    let cfg = omp_config(&spec);
    assert_eq!(cfg.plugin_roots, vec![PathBuf::from("/plugins")]);
    assert_eq!(cfg.brief_root, Some(PathBuf::from("/runs/r1/package")));
    assert_eq!(cfg.repo, "acme/widgets");
    let built = LivePorts.build(&spec).unwrap();
    assert_eq!(built.forge_api_hosts, "api.github.com");
}

const STUB_BODY: &str = r#"env > "$DIR/env"
(trap '' HUP; exec sleep 600) &
echo $! > "$DIR/child.pid"
echo '{"ev":"session_start","t":1}' >> "$PF_EVENT_FILE"
echo '{"ev":"agent_start","t":2}' >> "$PF_EVENT_FILE"
wait"#;

fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .map(|s| !s.contains(") Z "))
        .unwrap_or(false)
}

#[test]
fn omp_sets_the_edit_variant_and_stop_leaves_no_child_alive() {
    let dir = tempfile::tempdir().unwrap();
    let omp = dir.path().join("omp");
    std::fs::write(
        &omp,
        format!("#!/bin/sh\nDIR='{}'\nif [ \"$1\" = \"--version\" ]; then echo \"omp/18.11.3\"; exit 0; fi\n{STUB_BODY}\n", dir.path().display()),
    )
    .unwrap();
    std::fs::set_permissions(&omp, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut cfg = pf_adapter_omp::OmpConfig::new(dir.path().join("state"));
    cfg.omp_bin = omp;
    cfg.repo = "acme/widgets".into();
    let adapter = pf_adapter_omp::OmpAdapter::new(cfg);
    let cwd = dir.path().join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    let p = plan(
        "implementer",
        &[
            ("cwd", cwd.to_str().unwrap()),
            ("message", "go"),
            ("first_turn_seconds", "20"),
            ("stall_minutes", "10"),
        ],
    );
    let s = adapter.launch(&p).unwrap();
    let env = std::fs::read_to_string(dir.path().join("env")).unwrap();
    assert!(env.lines().any(|l| l == "PI_EDIT_VARIANT=replace"), "{env}");
    let child: u32 = std::fs::read_to_string(dir.path().join("child.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(alive(child), "the stub's child runs");
    adapter.stop(&s).unwrap();
    adapter.stop(&s).unwrap(); // safe to repeat
    for _ in 0..50 {
        if !alive(child) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(!alive(child), "stop must leave no child of the stub alive");
    assert_eq!(adapter.is_alive(&s), Ok(false));
}

#[test]
fn the_paper_forge_has_api_hosts_too() {
    let dir = tempfile::tempdir().unwrap();
    let bare = dir.path().join("forge.git");
    std::fs::create_dir_all(&bare).unwrap();
    git(
        &bare,
        &["init", "--quiet", "--bare", "--initial-branch=main"],
    );
    let spec = RunSpec {
        run_id: "r1".into(),
        run_dir: dir.path().join("run"),
        runtimes: vec!["omp".into(), "claude".into()],
        plugin_roots: vec![],
        paper: Some(dir.path().to_path_buf()),
        forge_repo: bare.display().to_string(),
    };
    let built = LivePorts.build(&spec).unwrap();
    assert_eq!(built.forge_api_hosts, "forge.invalid");
    assert!(!built.forge_api_hosts.is_empty());
}

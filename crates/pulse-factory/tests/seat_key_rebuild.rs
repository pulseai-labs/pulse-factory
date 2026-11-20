//! AC-6 of `r2.s2.w3`: the seat key table is rebuilt from the launch records (G3, Approach 8). The
//! table keys each seat by the SHA-256 of its session key, never the key text, and a controller that
//! starts again enters each surviving seat under the hash its launch record holds. So a surviving
//! seat's hook call is answered by the seat's rules, not "unknown to this controller". The first
//! tests run the real controller binary and `SIGKILL` it; the last ones drive `SeatPort` directly.

mod common;

use std::collections::BTreeMap;
use std::io::Write;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use common::restart::Restart;
use pf_adapter_fake::FakeAdapter;
use pf_engine::port::agent::*;
use pf_engine::port::host::{HostError, HostHandle, HostLaunch, Rebind, SessionHosts};
use pf_workflow::rules::RoleRules;
use pulse_factory::controller::seat::{SeatFacts, SeatPort, SeatTable};
use serde_json::{Value, json};

fn call(tool: &str, input: Value) -> Value {
    json!({ "role": "x", "tool": tool, "input": input })
}

fn allowed() -> Value {
    call("bash", json!({ "command": "ls -la" }))
}

fn denied() -> Value {
    call("bash", json!({ "command": "git push origin main" }))
}

#[test]
fn a_surviving_seats_hook_call_is_answered_by_its_rules_after_a_controller_restart() {
    let mut rig = Restart::new();
    rig.start_controller();
    rig.arm();
    rig.queue("u1", 1);
    rig.wait_seat("u1");
    rig.wait_attached("u1");
    // Before the restart: the rules answer.
    assert_eq!(rig.hook("u1", &allowed())["decision"], "allow");
    assert_eq!(rig.hook("u1", &denied())["decision"], "deny");

    rig.kill9();
    rig.start_controller();

    // After it: the same seat, the same key, and the same answers, from the rules.
    let allow = rig.hook("u1", &allowed());
    assert_eq!(allow["decision"], "allow", "{allow}");
    let deny = rig.hook("u1", &denied());
    assert_eq!(deny["decision"], "deny", "{deny}");
    let reason = deny["reason"].as_str().unwrap_or_default();
    assert!(!reason.contains("unknown to this controller"), "{deny}");
    assert!(!reason.is_empty(), "{deny}");
    // The audit line of the seat holds the call, and not the key.
    let audit = std::fs::read_to_string(rig.out("u1").join("audit.log")).unwrap();
    assert!(audit.contains("git push origin main"), "{audit}");
    assert!(!audit.contains("pfk_"), "{audit}");
}

#[test]
fn an_unknown_key_is_still_denied_after_a_restart() {
    let mut rig = Restart::new();
    rig.start_controller();
    rig.arm();
    rig.queue("u1", 1);
    rig.wait_seat("u1");
    rig.wait_attached("u1");
    rig.kill9();
    rig.start_controller();

    let mut child = rig
        .cli_cmd(&["hook"])
        .env(
            "PF_SESSION_KEY",
            "pfk_00000000000000000000000000000000000000000000000f",
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(allowed().to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(answer["decision"], "deny", "{answer}");
    assert!(
        answer["reason"]
            .as_str()
            .unwrap()
            .contains("unknown to this controller"),
        "{answer}"
    );
}

// ------------------------------------------------------------------ SeatPort and the table

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
    SeatFacts {
        repo: "acme/widgets".into(),
        remote: "origin".into(),
        default_branch: "main".into(),
        forge_api_hosts: "api.github.com".into(),
        run_dir: dir.to_path_buf(),
        state_dir: dir.join("state"),
        target_dir: String::new(),
        plugins: Vec::new(),
        rules,
        repeat_seeds: Vec::new(),
        binary: "/bin/pf".into(),
        random_device: pulse_factory::controller::seat::RANDOM_DEVICE.into(),
    }
}

fn plan(dir: &std::path::Path) -> LaunchPlan {
    let wt = dir.join("wt").display().to_string();
    let report = dir.join("spine/work-u1").display().to_string();
    LaunchPlan {
        run_id: "r1".into(),
        unit_id: "u1".into(),
        stage: "s".into(),
        role: "implementer".into(),
        model: "m".into(),
        runtime: "paper".into(),
        attempt: 1,
        settings: [
            ("report_dir", report),
            ("worktree", wt.clone()),
            ("own_branch", "work/u1".to_string()),
            ("cwd", wt),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect(),
    }
}

/// An adapter that records its launches and answers `rebind` with `Survived`, as the paper adapter
/// does for a host that answered.
struct Survives {
    inner: FakeAdapter,
    plans: Mutex<Vec<LaunchPlan>>,
}

impl AgentPort for Survives {
    fn launch(&self, p: &LaunchPlan) -> Result<SessionRef, AgentError> {
        self.plans.lock().unwrap().push(p.clone());
        self.inner.launch(p)
    }
    fn status(&self, s: &SessionRef) -> Result<SessionStatus, AgentError> {
        self.inner.status(s)
    }
    fn deliver(&self, s: &SessionRef, m: &str) -> Result<(), AgentError> {
        self.inner.deliver(s, m)
    }
    fn interrupt(&self, s: &SessionRef) -> Result<(), AgentError> {
        self.inner.interrupt(s)
    }
    fn usage(&self, s: &SessionRef) -> Result<Usage, AgentError> {
        self.inner.usage(s)
    }
    fn answer_dialog(&self, s: &SessionRef, d: &Dialog) -> Result<DialogAnswer, AgentError> {
        self.inner.answer_dialog(s, d)
    }
    fn apply_rules(&self, p: &mut LaunchPlan, r: &RuleSet) -> Result<(), AgentError> {
        self.inner.apply_rules(p, r)
    }
    fn remove_ask_tool(&self, p: &mut LaunchPlan) -> Result<(), AgentError> {
        self.inner.remove_ask_tool(p)
    }
    fn hand_off(&self, s: &SessionRef, p: &LaunchPlan) -> Result<SessionRef, AgentError> {
        self.inner.hand_off(s, p)
    }
    fn is_alive(&self, s: &SessionRef) -> Result<bool, AgentError> {
        self.inner.is_alive(s)
    }
    fn rebind(&self, _: &SessionRef) -> Result<RebindOutcome, AgentError> {
        Ok(RebindOutcome::Survived)
    }
    fn process_id(&self, s: &SessionRef) -> Option<u32> {
        self.inner.process_id(s)
    }
    fn stop(&self, s: &SessionRef) -> Result<(), AgentError> {
        self.inner.stop(s)
    }
    fn resolve_plugin(&self, n: &str, v: &str) -> Result<ResolvedPlugin, AgentError> {
        self.inner.resolve_plugin(n, v)
    }
    fn drain_events(&self, s: &SessionRef) -> Result<Vec<SessionEvent>, AgentError> {
        self.inner.drain_events(s)
    }
}

/// A host service that holds one launch record, as the registry would give it.
struct Records(Mutex<BTreeMap<String, Value>>);

impl SessionHosts for Records {
    fn launch(&self, _: HostLaunch) -> Result<HostHandle, HostError> {
        Err(HostError("this test launches nothing".into()))
    }
    fn rebind(&self, _: &str) -> Rebind {
        Rebind::Lost {
            exit: json!("this test rebinds nothing"),
        }
    }
    fn launched_record(&self, unit: &str) -> Result<Option<Value>, HostError> {
        Ok(self.0.lock().unwrap().get(unit).cloned())
    }
}

fn writes(table: &SeatTable, key: &str, command: &str) -> String {
    table.answer(&json!({ "key": key, "tool": "bash", "command": command }))["decision"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn the_table_holds_the_hash_of_each_key_and_never_the_key() {
    let dir = tempfile::tempdir().unwrap();
    let inner = Arc::new(Survives {
        inner: FakeAdapter::new(),
        plans: Mutex::new(Vec::new()),
    });
    let table = Arc::new(SeatTable::default());
    let port = SeatPort::new(inner.clone(), facts(dir.path()), table.clone());
    port.launch(&plan(dir.path())).unwrap();
    let key = inner.plans.lock().unwrap()[0].settings["env.PF_SESSION_KEY"].clone();
    assert!(key.starts_with("pfk_"), "{key}");

    let held = table.keys();
    assert_eq!(held.len(), 1);
    let hash = &held[0];
    assert_eq!(hash.len(), 64, "{hash}");
    assert!(
        hash.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "{hash}"
    );
    assert_eq!(*hash, pf_host::sha256::sha256_hex(key.as_bytes()));
    assert!(!hash.contains(&key));
    // The key text opens the seat's entry. The hash is not a key.
    assert_eq!(writes(&table, &key, "ls -la"), "allow");
    let by_hash = table.answer(&json!({ "key": hash, "tool": "bash", "command": "ls -la" }));
    assert_eq!(by_hash["decision"], "deny");
    assert!(
        by_hash["reason"]
            .as_str()
            .unwrap()
            .contains("unknown to this controller")
    );
}

#[test]
fn a_survived_seat_is_entered_again_under_the_hash_its_record_holds() {
    let dir = tempfile::tempdir().unwrap();
    // The first controller launches the seat.
    let first = Arc::new(Survives {
        inner: FakeAdapter::new(),
        plans: Mutex::new(Vec::new()),
    });
    let port = SeatPort::new(
        first.clone(),
        facts(dir.path()),
        Arc::new(SeatTable::default()),
    );
    port.launch(&plan(dir.path())).unwrap();
    let launched = first.plans.lock().unwrap()[0].clone();
    let key = launched.settings["env.PF_SESSION_KEY"].clone();

    // What the host service wrote: the plan's settings with the key replaced by its stand-in.
    let stand_in = format!("sha256:{}", pf_host::sha256::sha256_hex(key.as_bytes()));
    let mut settings: BTreeMap<String, String> = launched.settings.clone();
    settings.insert("env.PF_SESSION_KEY".into(), stand_in.clone());
    let record = json!({
        "env": { "PF_SESSION_KEY": stand_in },
        "settings": settings,
        "runtime": { "id": "paper" },
        "run_binding": {
            "run_id": "r1", "unit_id": "u1", "stage": "s", "role": "implementer",
            "model": "m", "attempt": 1
        },
    });
    let hosts = Arc::new(Records(Mutex::new(
        [("pf-host-abc-s1-1".to_string(), record)].into(),
    )));

    // A second controller: an empty table, and the router's tagged reference of the unit.
    let table = Arc::new(SeatTable::default());
    let second = Arc::new(Survives {
        inner: FakeAdapter::new(),
        plans: Mutex::new(Vec::new()),
    });
    let port = SeatPort::new(second, facts(dir.path()), table.clone()).with_hosts(hosts);
    assert!(table.is_empty());
    assert_eq!(
        writes(&table, &key, "ls -la"),
        "deny",
        "an empty table knows no key"
    );

    let out = port
        .rebind(&SessionRef("paper:pf-host-abc-s1-1".into()))
        .unwrap();
    assert_eq!(out, RebindOutcome::Survived);
    assert_eq!(table.len(), 1);
    assert_eq!(
        table.keys(),
        vec![pf_host::sha256::sha256_hex(key.as_bytes())]
    );
    // The rules are the seat's own: an ordinary call is allowed, a push to the default branch is not.
    assert_eq!(writes(&table, &key, "ls -la"), "allow");
    assert_eq!(writes(&table, &key, "git push origin main"), "deny");
    // An unknown key is denied.
    assert_eq!(writes(&table, "pfk_nonesuch", "ls -la"), "deny");
}

#[test]
fn a_seat_that_cannot_be_rebuilt_is_not_survived() {
    let dir = tempfile::tempdir().unwrap();
    let inner = Arc::new(Survives {
        inner: FakeAdapter::new(),
        plans: Mutex::new(Vec::new()),
    });
    let table = Arc::new(SeatTable::default());
    // The registry has no record for the unit: the seat could not be answered, so it is not survived.
    let hosts = Arc::new(Records(Mutex::new(BTreeMap::new())));
    let port = SeatPort::new(inner, facts(dir.path()), table.clone()).with_hosts(hosts);
    match port
        .rebind(&SessionRef("paper:pf-host-abc-s1-1".into()))
        .unwrap()
    {
        RebindOutcome::Unreachable { why } => assert!(why.contains("pf-host-abc-s1-1"), "{why}"),
        other => panic!("expected unreachable, got {other:?}"),
    }
    assert!(table.is_empty());
}

#[test]
fn the_module_comment_no_longer_says_a_surviving_seat_is_denied_everything() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/controller/seat.rs"),
    )
    .unwrap();
    assert!(
        !src.contains("denied everything"),
        "the module comment still says so"
    );
}

//! A live controller in the test process: the real `Controller` and its socket, on a
//! `FakeAdapter` that records every launch plan and can be made to block.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};

use pf_adapter_fake::FakeAdapter;
use pf_engine::port::agent::*;
use pulse_factory::arm::ArmPlan;
use pulse_factory::controller::ports::{Ports, RunPorts, RunSpec};
use pulse_factory::controller::{Config, Controller, RunHandle};

use super::{BIN, Sandbox, git};

/// A gate a test closes to hold a launch inside the adapter.
#[derive(Default)]
pub struct Gate {
    state: Mutex<(bool, bool)>, // (closed, someone is waiting)
    cv: Condvar,
}

impl Gate {
    pub fn close(&self) {
        self.state.lock().unwrap().0 = true;
    }

    pub fn open(&self) {
        self.state.lock().unwrap().0 = false;
        self.cv.notify_all();
    }

    fn pass(&self) {
        let mut st = self.state.lock().unwrap();
        while st.0 {
            st.1 = true;
            self.cv.notify_all();
            st = self.cv.wait(st).unwrap();
        }
    }

    /// Wait until a launch is held at the gate.
    pub fn wait_for_holder(&self) {
        let mut st = self.state.lock().unwrap();
        let mut n = 0;
        while !st.1 && n < 200 {
            let (g, _) = self
                .cv
                .wait_timeout(st, std::time::Duration::from_millis(50))
                .unwrap();
            st = g;
            n += 1;
        }
        assert!(st.1, "no launch reached the gate");
    }
}

pub struct Recording {
    pub inner: Arc<FakeAdapter>,
    pub plans: Arc<Mutex<Vec<LaunchPlan>>>,
    pub gate: Arc<Gate>,
}

impl AgentPort for Recording {
    fn launch(&self, p: &LaunchPlan) -> Result<SessionRef, AgentError> {
        self.plans.lock().unwrap().push(p.clone());
        self.gate.pass();
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
        self.plans.lock().unwrap().push(p.clone());
        self.inner.hand_off(s, p)
    }
    fn is_alive(&self, s: &SessionRef) -> Result<bool, AgentError> {
        self.inner.is_alive(s)
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

pub struct TestPorts {
    pub fake: Arc<FakeAdapter>,
    pub plans: Arc<Mutex<Vec<LaunchPlan>>>,
    pub gate: Arc<Gate>,
}

impl Ports for TestPorts {
    fn build(&self, spec: &RunSpec) -> Result<RunPorts, String> {
        let forge = pf_forge_bare::BareForge::open(&spec.forge_repo).map_err(|e| e.to_string())?;
        Ok(RunPorts {
            agent: Arc::new(Recording {
                inner: self.fake.clone(),
                plans: self.plans.clone(),
                gate: self.gate.clone(),
            }),
            forge: Arc::new(forge),
            forge_api_hosts: "forge.invalid".to_string(),
        })
    }
}

/// A controller with its socket served on a thread.
pub struct Live {
    pub sb: Sandbox,
    pub controller: Arc<Controller>,
    pub ports: Arc<TestPorts>,
    pub bare: PathBuf,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Live {
    pub fn start() -> Live {
        Live::start_with(BIN.into())
    }

    /// Start with the seats told to run this binary for the hook.
    pub fn start_with(binary: PathBuf) -> Live {
        let sb = Sandbox::new();
        let fake = Arc::new(FakeAdapter::new());
        fake.add_plugin(
            "methodkit",
            "1.26.1",
            sb.plugin_dir("methodkit", "1.26.1").to_str().unwrap(),
        );
        let ports = Arc::new(TestPorts {
            fake,
            plans: Arc::new(Mutex::new(Vec::new())),
            gate: Arc::new(Gate::default()),
        });
        let bare = sb.root.path().join("forge.git");
        std::fs::create_dir_all(&bare).unwrap();
        git(
            &bare,
            &["init", "--quiet", "--bare", "--initial-branch=main"],
        );
        pf_ipc::prepare_state_dir(&sb.state).unwrap();
        let bound = pf_ipc::bind(&sb.state).unwrap();
        let controller = Controller::open(
            Config {
                state_dir: sb.state.clone(),
                tick_ms: 3_600_000,
                binary,
            },
            ports.clone(),
        )
        .unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let served = controller.clone();
        let thread = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(pf_ipc::serve(bound, served, async {
                let _ = rx.await;
            }));
        });
        Live {
            sb,
            controller,
            ports,
            bare,
            stop: Some(tx),
            thread: Some(thread),
        }
    }

    pub fn plan(&self) -> ArmPlan {
        let mut params = self.sb.params();
        params.insert("spine".into(), "s1".into());
        params.insert("forge_repo".into(), self.bare.display().to_string());
        ArmPlan {
            spine: "s1".into(),
            workflow_dir: self.sb.package.clone(),
            ledger: self.sb.ledger.clone(),
            params,
            plugin_roots: vec![],
            paper: None,
            replace: None,
            forge_repo: self.bare.display().to_string(),
            default_branch: "main".into(),
            target_dir: String::new(),
        }
    }

    pub fn arm(&self) -> String {
        self.controller.arm(self.plan()).unwrap()
    }

    pub fn handle(&self) -> Arc<RunHandle> {
        self.controller.handles().last().cloned().expect("a run")
    }

    pub fn last_plan(&self) -> LaunchPlan {
        self.ports
            .plans
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("a launch")
    }

    pub fn run_dir(&self) -> PathBuf {
        self.sb.state.join("runs").join(&self.handle().info.run_id)
    }

    /// Launch a seat of `role` through `SeatPort`. The engine's settings are given as the engine
    /// would render them.
    pub fn launch(
        &self,
        role: &str,
        unit: &str,
        settings: &[(&str, String)],
    ) -> Result<LaunchPlan, AgentError> {
        let mut map: BTreeMap<String, String> = BTreeMap::new();
        for (k, v) in settings {
            map.insert((*k).to_string(), v.clone());
        }
        let plan = LaunchPlan {
            run_id: self.handle().info.run_id.clone(),
            unit_id: unit.into(),
            stage: "stage".into(),
            role: role.into(),
            model: "m".into(),
            runtime: "omp".into(),
            attempt: 1,
            settings: map,
        };
        self.handle().agent.launch(&plan)?;
        Ok(self.last_plan())
    }

    /// The settings of a seat for a role, as the stages of the fixture workflow give them.
    pub fn role_settings(&self, role: &str, unit: &str) -> Vec<(&'static str, String)> {
        let spine = self.sb.spine_dir.display().to_string();
        let repo = self.sb.repo.display().to_string();
        let wt = self.sb.spine_dir.join(format!("wt-{unit}"));
        std::fs::create_dir_all(&wt).unwrap();
        let wt = wt.display().to_string();
        match role {
            "lane" => vec![("cwd", repo), ("report_dir", spine)],
            "implementer" | "verifier" => vec![
                ("cwd", wt.clone()),
                ("worktree", wt),
                ("own_branch", format!("work/{unit}")),
                ("report_dir", format!("{spine}/work-{unit}")),
            ],
            _ => vec![
                ("cwd", repo),
                ("worktree", wt.clone()),
                ("own_branch", format!("work/{unit}")),
                ("push_dir", wt),
                ("push_branch", format!("work/{unit}")),
                ("report_dir", format!("{spine}/work-{unit}")),
            ],
        }
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.ports.gate.open();
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Run the hook the way the extension does: `/bin/sh -c "$PF_HOOK_CMD"`, the call on stdin, and
/// the environment of the seat (the plan's `env.*` settings). `edit_env` adjusts it.
pub fn run_hook(
    plan: &LaunchPlan,
    stdin: &[u8],
    edit_env: impl FnOnce(&mut BTreeMap<String, String>),
) -> (String, i32) {
    let mut env: BTreeMap<String, String> = plan
        .settings
        .iter()
        .filter_map(|(k, v)| k.strip_prefix("env.").map(|n| (n.to_string(), v.clone())))
        .collect();
    env.insert("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into());
    edit_env(&mut env);
    let hook = env
        .get("PF_HOOK_CMD")
        .cloned()
        .unwrap_or_else(|| "echo no-hook-cmd".into());
    let mut child = Command::new("/bin/sh")
        .args(["-c", &hook])
        .env_clear()
        .envs(&env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        let mut si = child.stdin.take().unwrap();
        let data = stdin.to_vec();
        let t = std::thread::spawn(move || {
            let _ = si.write_all(&data);
        });
        let out = child.wait_with_output().unwrap();
        let _ = t.join();
        (
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
            out.status.code().unwrap_or(-1),
        )
    }
}

pub fn call(tool: &str, input: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({ "role": "x", "tool": tool, "input": input })).unwrap()
}

pub fn decision(out: &str) -> serde_json::Value {
    serde_json::from_str(out).unwrap_or_else(|e| panic!("not JSON ({e}): {out:?}"))
}

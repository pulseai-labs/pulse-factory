//! The rig of the `settle` tests (`r2.s3.w2`): a controller on the fake adapter with a recording chat,
//! an armed run with parked units, a project with a live Orchestrator session and a live host row for
//! it, the run linked to that project, a clear grant, and an armed launch count. It is included with
//! `#[path]` and is no module of `common`, so a file here never gets a test crate of its own.
//!
//! A caller that must be below the chat is this test binary as a helper (`helper_calls_the_
//! controller`), run by a shell whose PID the test registers as the chat's process. The helper is a
//! test of this module: the shell runs it with `--exact settle_rig::helper_calls_the_controller`, and
//! run on its own it does nothing.
#![allow(dead_code)]

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use pf_engine::port::chat::ChatId;
use pf_engine::port::store::{
    AdmissionOutcome, CONTROLLER_SCOPE, Commit, Committed, Effect, NewAdmission, NewHost,
    NewLaunchRecord, Store,
};
use pulse_factory::controller::Config;
use serde_json::{Value, json};

use crate::common::chat::*;
use crate::common::ctl::Live;
use crate::common::*;

pub const MARKER: &str = "PULSE_FACTORY_CHAT";

/// The reason that tests give a settle.
pub const WHY: &str = "the screen shows nothing";

/// Which project the run belongs to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Link {
    /// The project of the Orchestrator session: the normal case.
    Own,
    /// Another project: the session's project is not the run's.
    Other,
    /// None: the run was armed from the CLI.
    NoProject,
}

pub struct SRig {
    pub live: Live,
    pub rec: Arc<Rec>,
    pub chat: ChatId,
    /// The project that owns the run and the Orchestrator session.
    pub project: String,
    pub session: String,
    pub run: String,
}

/// Commit one admission under `@controller`, with the effects, and answer whether it applied.
pub fn commit(
    live: &Live,
    kind: &str,
    id: &str,
    content: Value,
    effects: Vec<Effect>,
) -> Committed {
    live.controller
        .store()
        .commit(Commit {
            run_id: CONTROLLER_SCOPE.to_string(),
            command_id: id.to_string(),
            content: content.to_string(),
            admission: NewAdmission {
                kind: kind.to_string(),
                outcome: AdmissionOutcome::Admitted,
                reason: None,
                unit_id: None,
                role: None,
                model: None,
                attempt: None,
                admitted_at: "2026-11-24T00:00:00.000Z".to_string(),
            },
            effects,
            ledger: None,
        })
        .unwrap()
}

fn applied(c: Committed) {
    assert!(matches!(c, Committed::Applied { .. }), "{c:?}");
}

/// Link a run to a project, as a device's start card does.
pub fn link_run(live: &Live, run: &str, project: &str) {
    applied(commit(
        live,
        "run_project",
        &format!("run-project-{run}"),
        json!({ "run": run, "project": project }),
        vec![Effect::SetRunProject {
            run_id: run.to_string(),
            project_id: project.to_string(),
        }],
    ));
}

/// Store a run's armed launch count, as `arm` does.
pub fn set_bound(live: &Live, run: &str, bound: u64) {
    applied(commit(
        live,
        "run_launch_bound",
        &format!("run-launch-bound-{run}"),
        json!({ "run": run, "most_launches": bound }),
        vec![Effect::SetRunLaunchBound {
            run_id: run.to_string(),
            bound,
        }],
    ));
}

/// The host a chat session's row names: the unit, the launcher and the invocation ID. The default is
/// a made-up unit of the `detached` launcher. A live systemd test names a real transient unit.
#[derive(Clone)]
pub struct HostSeed {
    pub unit: String,
    pub launcher: String,
    pub invocation: String,
}

impl HostSeed {
    pub fn fake(session: &str) -> HostSeed {
        HostSeed {
            unit: format!("pf-host-testserver-{session}-1"),
            launcher: "detached".to_string(),
            invocation: "inv-1".to_string(),
        }
    }
}

/// A stub `systemctl` that stands in for the user manager when a test runs the `systemd` launcher
/// without one. It answers every `show -p ControlGroup` with one cgroup path, or fails as the real
/// one does with no user bus or no such unit. Point `Config::systemctl` at [`CgroupStub::program`].
pub struct CgroupStub {
    dir: tempfile::TempDir,
}

impl CgroupStub {
    /// A stub that names a cgroup for `unit` that no test process is in.
    pub fn new(unit: &str) -> CgroupStub {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("systemctl");
        std::fs::write(
            &script,
            "#!/bin/sh\nf=\"$(dirname \"$0\")/cgroup\"\n[ -s \"$f\" ] || exit 1\ncat \"$f\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let stub = CgroupStub { dir };
        stub.answer(unit);
        stub
    }

    pub fn program(&self) -> std::path::PathBuf {
        self.dir.path().join("systemctl")
    }

    /// The stub names the cgroup of `unit` again.
    pub fn answer(&self, unit: &str) {
        std::fs::write(
            self.dir.path().join("cgroup"),
            format!("/user.slice/user-1.slice/user@1.service/app.slice/{unit}.service\n"),
        )
        .unwrap();
    }

    /// The stub fails, as `systemctl --user` does with no user manager.
    pub fn fail(&self) {
        std::fs::write(self.dir.path().join("cgroup"), "").unwrap();
    }
}

/// Write the live host row of a chat session, as the host service does when it starts a chat's host.
pub fn seed_host(live: &Live, session: &str) -> String {
    seed_host_as(live, session, &HostSeed::fake(session))
}

/// [`seed_host`] with the host the caller names.
pub fn seed_host_as(live: &Live, session: &str, seed: &HostSeed) -> String {
    let body = json!({
        "argv": ["claude"],
        "cwd": "/work/alpha",
        "env": {"HOME": "/home/dev"},
        "secret_names": [],
        "transport": {"kind": "pipes"},
        "runtime": {
            "id": "claude-code",
            "version": "2.1.0",
            "binary_path": "/bin/true",
            "binary_sha256": "ab".repeat(32),
        },
        "settings": {},
        "plugins": [],
        "skills": [],
        "rules_sha256": "cd".repeat(32),
        "run_binding": null,
        "resume": "fresh",
    })
    .to_string();
    let (_, hash) = NewLaunchRecord::seal(&body).unwrap();
    let unit = seed.unit.clone();
    applied(commit(
        live,
        "launch_record_write",
        &format!("lr-{session}"),
        json!({ "session": session }),
        vec![Effect::LaunchRecordWrite(NewLaunchRecord {
            session_kind: "chat".to_string(),
            session_id: session.to_string(),
            incarnation: 1,
            revision: 1,
            launcher: seed.launcher.clone(),
            host_unit: unit.clone(),
            host_socket: format!("/tmp/{unit}/host.sock"),
            record: body,
            record_sha256: hash.clone(),
            created_at: "t1".to_string(),
        })],
    ));
    applied(commit(
        live,
        "host_started",
        &format!("hs-{session}"),
        json!({ "unit": unit }),
        vec![Effect::HostStarted(NewHost {
            unit: unit.clone(),
            session_kind: "chat".to_string(),
            session_id: session.to_string(),
            incarnation: 1,
            launched_revision: 1,
            launched_record_sha256: hash,
            launcher: seed.launcher.clone(),
            socket: format!("/tmp/{unit}/host.sock"),
            invocation_id: seed.invocation.clone(),
            host_binary: "/bin/true".to_string(),
            host_sha256: "aa".repeat(32),
            protocol: 1,
            started_at: "t2".to_string(),
        })],
    ));
    unit
}

impl SRig {
    /// A controller whose run `units` are parked on a stall, whose project has a session, a host
    /// and a live grant, and whose run holds the launch count `bound`.
    pub fn new(units: &[&str], bound: Option<u64>) -> SRig {
        SRig::with(units, bound, |_| {})
    }

    pub fn with(
        units: &[&str],
        bound: Option<u64>,
        tweak: impl FnOnce(&mut Config) + Send + 'static,
    ) -> SRig {
        SRig::linked(units, bound, Link::Own, tweak)
    }

    /// Like [`SRig::with`], with the run linked as `link` says. With `Link::Other` a second project
    /// owns the run and holds a clear grant; the session belongs to the first.
    pub fn linked(
        units: &[&str],
        bound: Option<u64>,
        link: Link,
        tweak: impl FnOnce(&mut Config) + Send + 'static,
    ) -> SRig {
        SRig::linked_on(units, bound, link, None, tweak)
    }

    /// Like [`SRig::linked`], with the session's host row naming `host` (the default is a made-up
    /// unit of the `detached` launcher).
    pub fn linked_on(
        units: &[&str],
        bound: Option<u64>,
        link: Link,
        host: Option<HostSeed>,
        tweak: impl FnOnce(&mut Config) + Send + 'static,
    ) -> SRig {
        let rec = Rec::new(strict_fake());
        let live = start_with(rec.clone(), tweak);
        live.arm_with_units(units);
        let run = live.handle().info.run_id.clone();
        let (project, session) = started(&live, "alpha");
        let chat = rec.chats.lock().unwrap()[0].clone();
        let other = (link == Link::Other).then(|| add_project(&live, &repo(&live, "beta")));
        match (&link, &other) {
            (Link::Own, _) => link_run(&live, &run, &project),
            (Link::Other, Some(other)) => link_run(&live, &run, other),
            _ => {}
        }
        let seed = host.unwrap_or_else(|| HostSeed::fake(&session));
        seed_host_as(&live, &session, &seed);
        if let Some(n) = bound {
            set_bound(&live, &run, n);
        }
        for unit in units {
            live.park_unit(unit, "stalled: the screen shows nothing");
        }
        let rig = SRig {
            live,
            rec,
            chat,
            project,
            session,
            run,
        };
        let out = rig.live.cli(&["grant", "clear", "--project", &rig.project]);
        assert_eq!(code(&out), 0, "{}", stderr(&out));
        if let Some(other) = other {
            let out = rig.live.cli(&["grant", "clear", "--project", &other]);
            assert_eq!(code(&out), 0, "{}", stderr(&out));
        }
        rig
    }

    /// The open request of a unit.
    pub fn request(&self, unit: &str) -> (String, u32) {
        let view = self.live.handle().engine.view().unwrap();
        let r = view
            .open_request_of(unit)
            .unwrap_or_else(|| panic!("no open request for {unit}"));
        (r.id.clone(), r.revision)
    }

    /// `settle` as the helper below the chat. The answer is the whole `Response` as JSON.
    pub fn settle(
        &self,
        request: &str,
        rev: u32,
        act: &str,
        to: Option<&str>,
        reason: &str,
    ) -> Value {
        self.call_as(
            true,
            &[],
            "settle",
            &settle_args(request, rev, act, to, reason),
        )
    }

    /// The admissions of the run, by kind.
    pub fn admissions(&self, kind: &str) -> usize {
        self.live
            .controller
            .store()
            .admissions(&self.run)
            .unwrap()
            .iter()
            .filter(|a| a.kind == kind)
            .count()
    }

    /// The refused admissions of `controller.ledger` for `settle`, as `(reason, id)`.
    pub fn refused_settles(&self) -> Vec<(String, String)> {
        self.live
            .controller
            .store()
            .admissions(CONTROLLER_SCOPE)
            .unwrap()
            .into_iter()
            .filter(|a| a.kind == "settle" && a.outcome == AdmissionOutcome::Refused)
            .map(|a| (a.reason.unwrap_or_default(), a.command_id))
            .collect()
    }

    /// The run's ledger lines of a kind of word, such as `settle`.
    pub fn run_ledger(&self, word: &str) -> Vec<String> {
        let path = self.live.handle().info.ledger.clone();
        lines(&path)
            .into_iter()
            .filter(|l| l.contains(&format!(" {word} ")))
            .collect()
    }

    pub fn request_json(&self, id: &str) -> Value {
        cli_json(&self.live, &["show", id])
    }

    pub fn tick(&self) {
        self.live.controller.tick_all();
    }
}

pub fn settle_args(request: &str, rev: u32, act: &str, to: Option<&str>, reason: &str) -> Value {
    let mut args = json!({
        "request_id": request,
        "revision": rev,
        "act": act,
        "reason": reason,
    });
    if let Some(to) = to {
        args["to_stage"] = json!(to);
    }
    args
}

static RUNS: AtomicUsize = AtomicUsize::new(0);

/// Not a test: the process that `call_as` starts. Run on its own it does nothing.
#[test]
fn helper_calls_the_controller() {
    let Ok(state) = std::env::var("PF_TEST_HELPER_STATE") else {
        return;
    };
    let verb = std::env::var("PF_TEST_HELPER_VERB").unwrap();
    let args: Value = serde_json::from_str(&std::env::var("PF_TEST_HELPER_ARGS").unwrap()).unwrap();
    let resp = pf_ipc::call(Path::new(&state), &verb, args).unwrap();
    println!("HELPER_RESPONSE:{}", serde_json::to_string(&resp).unwrap());
}

/// Not a test: a process that sends its request and exits at once, without reading the answer.
#[test]
fn helper_sends_the_request_and_exits() {
    use std::io::Write;
    let Ok(state) = std::env::var("PF_TEST_HELPER_STATE") else {
        return;
    };
    let verb = std::env::var("PF_TEST_HELPER_VERB").unwrap();
    let args: Value = serde_json::from_str(&std::env::var("PF_TEST_HELPER_ARGS").unwrap()).unwrap();
    let mut stream =
        std::os::unix::net::UnixStream::connect(pf_ipc::socket_path(Path::new(&state))).unwrap();
    let mut line = serde_json::to_vec(&pf_ipc::Request::new(&verb, args)).unwrap();
    line.push(b'\n');
    stream.write_all(&line).unwrap();
    stream.flush().unwrap();
    std::process::exit(0);
}

impl SRig {
    /// Call `verb` from the helper, below a shell. `below_chat` registers the shell as the live
    /// chat's process; `env` is what the helper's environment holds besides the path to the
    /// controller.
    pub fn call_as(
        &self,
        below_chat: bool,
        env: &[(&str, &str)],
        verb: &str,
        args: &Value,
    ) -> Value {
        let (child, go) =
            self.spawn_caller(below_chat, env, verb, args, "helper_calls_the_controller");
        std::fs::write(&go, "").unwrap();
        self.finish_caller(child)
    }

    /// Call `verb` from a helper that is below a tracked seat: the shell is registered as the
    /// process of the lane's seat, and not as the chat's.
    pub fn call_as_seat(&self, verb: &str, args: &Value) -> Value {
        let (child, go) = self.spawn_caller(false, &[], verb, args, "helper_calls_the_controller");
        let refs = self.live.session_refs();
        assert!(!refs.is_empty(), "a tracked seat needs a session row");
        self.live
            .ports
            .pids
            .lock()
            .unwrap()
            .insert(refs[0].0.clone(), child.id());
        std::fs::write(&go, "").unwrap();
        let out = self.finish_caller(child);
        self.live.ports.pids.lock().unwrap().clear();
        out
    }

    /// Start the helper's shell, which waits for the `go` file. The shell is registered as the chat's
    /// process when `below_chat` holds. The test writes the file to let the call go.
    pub fn spawn_caller(
        &self,
        below_chat: bool,
        env: &[(&str, &str)],
        verb: &str,
        args: &Value,
        helper: &str,
    ) -> (std::process::Child, std::path::PathBuf) {
        let n = RUNS.fetch_add(1, Ordering::SeqCst);
        let go = self.live.sb.root.path().join(format!("go-{n}"));
        let exe = std::env::current_exe().unwrap();
        let mut shell = Command::new("/bin/sh");
        shell
            .arg("-c")
            .arg(
                "while [ ! -e \"$GO\" ]; do sleep 0.02; done; \
                 ( \"$EXE\" --exact \"settle_rig::$HELPER\" --nocapture --test-threads=1 )",
            )
            .env_clear()
            .env("PATH", self.live.sb.path_env())
            .env("GO", &go)
            .env("EXE", &exe)
            .env("HELPER", helper)
            .env("PF_TEST_HELPER_STATE", &self.live.sb.state)
            .env("PF_TEST_HELPER_VERB", verb)
            .env("PF_TEST_HELPER_ARGS", args.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in env {
            shell.env(k, v);
        }
        let child = shell.spawn().unwrap();
        if below_chat {
            self.rec
                .pids
                .lock()
                .unwrap()
                .insert(self.chat.clone(), child.id());
        }
        (child, go)
    }

    /// Run the real `pulse-factory` client with `args` as a child of a shell that the chat's process
    /// is registered as, so the controller names its caller as the Orchestrator's session.
    pub fn cli_below_chat(&self, args: &[&str]) -> std::process::Output {
        let n = RUNS.fetch_add(1, Ordering::SeqCst);
        let go = self.live.sb.root.path().join(format!("go-{n}"));
        let inner = self.live.sb.cmd(args);
        let mut shell = Command::new("/bin/sh");
        shell
            .arg("-c")
            // The subshell keeps the shell from replacing itself with the client.
            .arg("while [ ! -e \"$GO\" ]; do sleep 0.02; done; ( \"$0\" \"$@\" )")
            .arg(inner.get_program())
            .args(inner.get_args())
            .env_clear()
            .current_dir(self.live.sb.root.path())
            .env("GO", &go)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in inner.get_envs() {
            if let Some(v) = v {
                shell.env(k, v);
            }
        }
        let child = shell.spawn().unwrap();
        self.rec
            .pids
            .lock()
            .unwrap()
            .insert(self.chat.clone(), child.id());
        std::fs::write(&go, "").unwrap();
        let out = child.wait_with_output().unwrap();
        self.rec.pids.lock().unwrap().clear();
        out
    }

    /// Wait for the helper and read its answer.
    pub fn finish_caller(&self, child: std::process::Child) -> Value {
        let out = child.wait_with_output().unwrap();
        self.rec.pids.lock().unwrap().clear();
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let line = text
            .lines()
            .find_map(|l| l.split_once("HELPER_RESPONSE:").map(|(_, r)| r.to_string()))
            .unwrap_or_else(|| {
                panic!(
                    "no response from the helper: {text}\n{}",
                    String::from_utf8_lossy(&out.stderr)
                )
            });
        serde_json::from_str(&line).unwrap()
    }
}

/// The text of a refused answer, or a failure that shows the answer.
pub fn why(resp: &Value) -> String {
    assert_eq!(resp["ok"], false, "{resp}");
    resp["error"].as_str().unwrap_or_default().to_string()
}

/// An admitted answer's result.
pub fn done(resp: &Value) -> Value {
    assert_eq!(resp["ok"], true, "{resp}");
    resp["result"].clone()
}

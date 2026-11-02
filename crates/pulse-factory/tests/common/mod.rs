//! Helpers for the `pulse-factory` tests. Nothing here spends money or reaches the network: the
//! forge is a stub `gh` and a local git repo, the seats are fakes or paper scripts.
#![allow(dead_code)]

pub mod ctl;
pub mod rig;

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub const BIN: &str = env!("CARGO_BIN_EXE_pulse-factory");

/// What a fixture workflow package holds. The defaults make a package that arms cleanly.
#[derive(Clone)]
pub struct Options {
    /// The YAML of the `plan_check` command, an argv list.
    pub plan_check: String,
    /// The minimum version the package asks of its one plugin.
    pub plugin_min: String,
    /// The version of the plugin the sandbox installs.
    pub plugin_version: String,
    /// The `max` of the lane seed.
    pub seed_max: u32,
    /// The `settings:` block of the stage `lane`, indented by 8 spaces.
    pub lane_settings: String,
    /// Extra keys of the stage `implement`, indented by 8 spaces (for example `before:`).
    pub implement_extra: String,
    pub model_sessions: u32,
    pub cold_builds: u32,
    /// The `stall_minutes` of the stage `implement`.
    pub implement_stall: u32,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            plan_check: "[printf, \"%s\", \"{param.count}\"]".to_string(),
            plugin_min: "1.26.0".to_string(),
            plugin_version: "1.26.1".to_string(),
            seed_max: 3,
            lane_settings: String::new(),
            implement_extra: String::new(),
            model_sessions: 2,
            cold_builds: 1,
            implement_stall: 20,
        }
    }
}

pub fn write_script(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The fixture workflow, as one YAML text. It has a lane seed, an item track of three stages and a
/// merge rule, like the shipped package, but it names a plugin `methodkit`.
pub fn workflow_yaml(o: &Options) -> String {
    format!(
        r#"version: 1
name: fixture-spine
params: [spine, spine_branch, spine_dir, repo, forge_repo, count, briefs]
plugins:
  - name: methodkit
    min_version: {plugin_min}
roles:
  lane:
    runtime: omp
    model: model-a
    rules: rules/lane.yaml
  implementer:
    runtime: omp
    model: model-b
    rules: rules/implementer.yaml
  verifier:
    runtime: omp
    model: model-c
    rules: rules/verifier.yaml
  close:
    runtime: omp
    model: model-d
    rules: rules/close.yaml
settings:
  model_sessions: {model_sessions}
  cold_builds: {cold_builds}
plan_check: {plan_check}
roots: ["{{param.spine_dir}}"]
unit_sources:
  - dir: "{{run.dir}}/queue"
    track: item
    vars: [worktree_path, branch]
seeds:
  - id: lane
    group: lane
    order: 1
    track: lane
    vars: {{}}
    repeat:
      when_units_done: item
      until_file: "{{run.dir}}/rounds.done"
      max: {seed_max}
tracks:
  lane:
    stages:
      - name: lane
        role: lane
        brief: [briefs/common.md]
        message: "Read {{param.briefs}}/common.md. Spine {{param.spine}}. OUT {{run.dir}}/out/{{unit.id}}."
        cwd: "{{param.repo}}"
        stop_file: "{{run.dir}}/out/{{unit.id}}/stop"
        stall_minutes: 15
        first_turn_seconds: 180
        settings:
          report_dir: "{{param.spine_dir}}"
          decisions_file: "{{run.dir}}/out/{{unit.id}}/decisions.md"
{lane_settings}        done:
          - file: "{{run.dir}}/out/{{unit.id}}/lane.done"
  item:
    stages:
      - name: implement
        role: implementer
        brief: [briefs/common.md]
        message: "Implement {{unit.id}}."
        cwd: "{{unit.vars.worktree_path}}"
        slots: [session, build]
{implement_extra}        stop_file: "{{run.dir}}/out/{{unit.id}}/stop"
        stall_minutes: {implement_stall}
        first_turn_seconds: 180
        settings:
          report_dir: "{{param.spine_dir}}/work-{{unit.id}}"
          worktree: "{{unit.vars.worktree_path}}"
          own_branch: "{{unit.vars.branch}}"
          decisions_file: "{{run.dir}}/out/{{unit.id}}/decisions.md"
        done:
          - file: "{{param.spine_dir}}/work-{{unit.id}}/report.md"
      - name: verify
        role: verifier
        brief: [briefs/common.md]
        message: "Verify {{unit.id}}."
        cwd: "{{unit.vars.worktree_path}}"
        stop_file: "{{run.dir}}/out/{{unit.id}}/stop"
        stall_minutes: 20
        first_turn_seconds: 180
        settings:
          report_dir: "{{param.spine_dir}}/work-{{unit.id}}"
          worktree: "{{unit.vars.worktree_path}}"
          own_branch: "{{unit.vars.branch}}"
        done:
          - file: "{{run.dir}}/out/{{unit.id}}/verdict.json"
      - name: close
        role: close
        brief: [briefs/common.md]
        message: "Close {{unit.id}}."
        cwd: "{{param.repo}}"
        stop_file: "{{run.dir}}/out/{{unit.id}}/stop"
        stall_minutes: 20
        first_turn_seconds: 180
        settings:
          push_dir: "{{unit.vars.worktree_path}}"
          push_branch: "{{unit.vars.branch}}"
          report_dir: "{{param.spine_dir}}/work-{{unit.id}}"
          worktree: "{{unit.vars.worktree_path}}"
          own_branch: "{{unit.vars.branch}}"
          decisions_file: "{{run.dir}}/out/{{unit.id}}/decisions.md"
        done:
          - pr:
              argv: [cat, "{{run.dir}}/out/{{unit.id}}/pr.url"]
              test: url
    merge:
      rule: green_checks_in_declared_order
      repo: "{{param.forge_repo}}"
      base: "{{param.spine_branch}}"
      pr: [cat, "{{run.dir}}/out/{{unit.id}}/pr.url"]
      head_check: true
      after_merge:
        - [sh, -c, "echo {{unit.id}} >> {{run.dir}}/after_merge.log"]
"#,
        plugin_min = o.plugin_min,
        plan_check = o.plan_check,
        seed_max = o.seed_max,
        lane_settings = o.lane_settings,
        implement_extra = o.implement_extra,
        model_sessions = o.model_sessions,
        cold_builds = o.cold_builds,
        implement_stall = o.implement_stall,
    )
}

pub fn write_package(dir: &Path, o: &Options) {
    std::fs::create_dir_all(dir.join("briefs")).unwrap();
    std::fs::create_dir_all(dir.join("rules")).unwrap();
    std::fs::write(dir.join("workflow.yaml"), workflow_yaml(o)).unwrap();
    std::fs::write(dir.join("briefs/common.md"), "# Common\nDo the work.\n").unwrap();
    let rules: [(&str, &[&str]); 4] = [
        (
            "lane",
            &["delete_own_scratch", "write_own_report_and_run_files"],
        ),
        (
            "implementer",
            &["delete_own_scratch", "write_own_report_and_run_files"],
        ),
        (
            "verifier",
            &["delete_own_scratch", "write_own_report_and_run_files"],
        ),
        (
            "close",
            &[
                "delete_own_scratch",
                "push_own_branch",
                "write_own_report_and_run_files",
            ],
        ),
    ];
    for (role, allow) in rules {
        let list: String = allow.iter().map(|a| format!("  - {a}\n")).collect();
        std::fs::write(
            dir.join(format!("rules/{role}.yaml")),
            format!("version: 1\nrole: {role}\nallow:\n{list}"),
        )
        .unwrap();
    }
}

/// A sorted listing of a directory tree, with file sizes: to see that nothing changed.
pub fn listing(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            let rel = p.strip_prefix(base).unwrap().display().to_string();
            let meta = std::fs::symlink_metadata(&p).unwrap();
            if meta.is_dir() {
                out.push(format!("{rel}/"));
                walk(base, &p, out);
            } else {
                out.push(format!("{rel} {}", meta.len()));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Everything one test needs: a state directory, a share directory, a stub `gh` first on `PATH`,
/// a plugin root, a stub git repo with a GitHub `origin`, a fixture workflow and a ledger path.
pub struct Sandbox {
    pub root: tempfile::TempDir,
    pub state: PathBuf,
    pub share: PathBuf,
    pub bin: PathBuf,
    pub plugin_root: PathBuf,
    pub repo: PathBuf,
    pub spine_dir: PathBuf,
    pub package: PathBuf,
    pub ledger: PathBuf,
}

impl Sandbox {
    pub fn new() -> Sandbox {
        Sandbox::with(&Options::default())
    }

    pub fn with(o: &Options) -> Sandbox {
        let root = tempfile::tempdir().unwrap();
        let p = root.path().to_path_buf();
        let state = p.join("state");
        let share = p.join("share");
        let bin = p.join("bin");
        let plugin_root = p.join("plugins");
        let repo = p.join("repo");
        let spine_dir = p.join("spine");
        let package = p.join("package");
        let ledger = p.join("ledger.md");
        for d in [&bin, &plugin_root, &repo, &spine_dir] {
            std::fs::create_dir_all(d).unwrap();
        }
        // The stub `gh` answers `repo view` from a file, and fails when `gh.fail` exists.
        write_script(
            &bin.join("gh"),
            &format!(
                "#!/bin/sh\nd='{}'\necho \"$@\" >> \"$d/gh.calls\"\nif [ -e \"$d/gh.fail\" ]; then echo 'gh: boom' >&2; exit 1; fi\ncat \"$d/gh.json\"\n",
                bin.display()
            ),
        );
        let sb = Sandbox {
            root,
            state,
            share,
            bin,
            plugin_root,
            repo,
            spine_dir,
            package,
            ledger,
        };
        sb.gh_answers("acme/widgets", "main");
        sb.install_plugin("methodkit", &o.plugin_version);
        git(&sb.repo, &["init", "-q", "-b", "main"]);
        git(
            &sb.repo,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/acme/widgets.git",
            ],
        );
        write_package(&sb.package, o);
        sb
    }

    pub fn gh_answers(&self, name_with_owner: &str, default_branch: &str) {
        std::fs::write(
            self.bin.join("gh.json"),
            format!(
                "{{\"nameWithOwner\":\"{name_with_owner}\",\"defaultBranchRef\":{{\"name\":\"{default_branch}\"}}}}"
            ),
        )
        .unwrap();
    }

    pub fn gh_fails(&self) {
        std::fs::write(self.bin.join("gh.fail"), "").unwrap();
    }

    pub fn gh_calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.bin.join("gh.calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Install a plugin in the layout of a plugin cache: `<root>/market/<name>/<version>/`.
    pub fn install_plugin(&self, name: &str, version: &str) {
        let base = self.plugin_root.join("market").join(name).join(version);
        let dir = base.join(".claude-plugin");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("plugin.json"),
            format!("{{\"version\":\"{version}\"}}"),
        )
        .unwrap();
        std::fs::write(base.join("SKILL.md"), "skill text\n").unwrap();
    }

    /// The directory of the installed plugin.
    pub fn plugin_dir(&self, name: &str, version: &str) -> PathBuf {
        self.plugin_root.join("market").join(name).join(version)
    }

    pub fn set_origin(&self, url: &str) {
        git(&self.repo, &["remote", "set-url", "origin", url]);
    }

    pub fn path_env(&self) -> String {
        format!("{}:/usr/local/bin:/usr/bin:/bin", self.bin.display())
    }

    /// A `pulse-factory` command with this sandbox's directories and no seat token.
    pub fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.args(args)
            .env_clear()
            .env("PATH", self.path_env())
            .env("HOME", self.root.path())
            .env("PF_STATE_DIR", &self.state)
            .env("PF_SHARE_DIR", &self.share)
            .current_dir(self.root.path());
        c
    }

    /// The parameters of a normal arm, as `--param` pairs.
    pub fn params(&self) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        m.insert("spine_branch".to_string(), "spine/s1".to_string());
        m.insert(
            "spine_dir".to_string(),
            self.spine_dir.display().to_string(),
        );
        m.insert("repo".to_string(), self.repo.display().to_string());
        m.insert("count".to_string(), "2".to_string());
        m
    }

    /// The arguments of a normal arm, up to the flags.
    pub fn arm_args(&self) -> Vec<String> {
        let mut a: Vec<String> = vec![
            "arm".into(),
            "s1".into(),
            "--workflow".into(),
            self.package.display().to_string(),
            "--ledger".into(),
            self.ledger.display().to_string(),
            "--plugin-root".into(),
            self.plugin_root.display().to_string(),
        ];
        for (k, v) in self.params() {
            a.push("--param".into());
            a.push(format!("{k}={v}"));
        }
        a
    }

    pub fn run(&self, args: &[String]) -> Output {
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        self.cmd(&refs).output().expect("run pulse-factory")
    }

    pub fn state_listing(&self) -> Vec<String> {
        listing(&self.state)
    }
}

pub fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

pub fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

pub fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

/// A stand-in controller for the client tests: it answers `runs` with a fixed list and records
/// every `arm` it gets. It runs on a thread of its own.
pub struct Stub {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    pub requests: std::sync::Arc<std::sync::Mutex<Vec<pf_ipc::Request>>>,
}

struct StubHandler {
    runs: Vec<serde_json::Value>,
    requests: std::sync::Arc<std::sync::Mutex<Vec<pf_ipc::Request>>>,
}

impl pf_ipc::Handler for StubHandler {
    fn handle(&self, request: pf_ipc::Request) -> pf_ipc::Response {
        self.requests.lock().unwrap().push(request.clone());
        match request.verb.as_str() {
            "runs" => pf_ipc::Response::ok(serde_json::json!({ "runs": self.runs })),
            "arm" => pf_ipc::Response::ok(serde_json::json!({ "run_id": "s1-1" })),
            other => pf_ipc::Response::err(format!("the stub does not know {other}")),
        }
    }
}

impl Stub {
    pub fn start(state: &Path, runs: Vec<serde_json::Value>) -> Stub {
        pf_ipc::prepare_state_dir(state).unwrap();
        let bound = pf_ipc::bind(state).unwrap();
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let handler = StubHandler {
            runs,
            requests: requests.clone(),
        };
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let thread = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(pf_ipc::serve(bound, std::sync::Arc::new(handler), async {
                let _ = rx.await;
            }));
        });
        Stub {
            stop: Some(tx),
            thread: Some(thread),
            requests,
        }
    }

    pub fn verbs(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.verb.clone())
            .collect()
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// A real controller process on the sandbox's state directory. It is killed on drop.
pub struct Ctl {
    child: std::process::Child,
}

impl Ctl {
    pub fn start(sb: &Sandbox, tick_ms: u64) -> Ctl {
        let child = sb
            .cmd(&["controller", "--tick-ms", &tick_ms.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("start the controller");
        let ctl = Ctl { child };
        let sock = sb.state.join("controller.sock");
        for _ in 0..200 {
            if pf_ipc::call(&sb.state, "runs", serde_json::json!({})).is_ok() {
                return ctl;
            }
            let _ = &sock;
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        panic!("the controller did not answer");
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for Ctl {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Sandbox {
    /// `status <run> --json`, parsed.
    pub fn status_json(&self, run: &str) -> serde_json::Value {
        let mut args: Vec<String> = vec!["status".into()];
        if !run.is_empty() {
            args.push(run.into());
        }
        args.push("--json".into());
        let out = self.run(&args);
        assert_eq!(code(&out), 0, "{}", stderr(&out));
        serde_json::from_str(&stdout(&out)).unwrap()
    }

    pub fn read_ledger(&self) -> String {
        std::fs::read_to_string(&self.ledger).unwrap_or_default()
    }

    pub fn read_json(&self, rel: &str) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(self.state.join(rel)).unwrap()).unwrap()
    }
}

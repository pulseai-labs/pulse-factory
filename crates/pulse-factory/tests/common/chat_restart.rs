//! The chat restart rig of `r2.s2.w5` (`chat_host_restart.rs`, `chat_host_resume.rs`,
//! `chat_host_lifetime.rs`, `chat_output_lost.rs`): the real `pulse-factory controller` binary with
//! `ClaudeChat` over `pf-claude-replay`, a chat that lives in a detached session host, and a way to
//! `SIGKILL` the controller and start it again (`Restart`). Nothing here starts a model, spends a
//! token or reaches beyond the temp directories. The rig's `Restart` guard stops every host when a
//! test ends.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use pf_adapter_claude::{CLAUDE_CODE_RUNTIME, ClaudeChat, ClaudeConfig};
use pf_engine::port::chat::ChatPort;
use pulse_factory::controller::ports::{Ports, RunPorts, RunSpec};
use pulse_factory::controller::{Config, Controller};
use serde_json::Value;

use super::BIN;
use super::restart::{Restart, host_bin};

/// The ports of a controller that runs no run: a chat test needs none.
pub struct NoRuns;

impl Ports for NoRuns {
    fn build(&self, _spec: &RunSpec) -> Result<RunPorts, String> {
        Err("this test runs no run".to_string())
    }
}

/// `pf-claude-replay`, in the test binaries' target directory. It must be built first
/// (`cargo build -p pf-adapter-claude --bins`, as each AC does). A missing file fails the test.
pub fn replay_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let bin = exe
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("pf-claude-replay");
    assert!(
        bin.is_file(),
        "{} is missing: run `cargo build -p pf-adapter-claude --bins` first",
        bin.display()
    );
    bin
}

pub fn fixture(name: &str) -> PathBuf {
    if Path::new(name).is_absolute() {
        return PathBuf::from(name);
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../pf-adapter-claude/fixtures")
        .join(format!("{name}.jsonl"))
}

/// A copy of `big_output` in `dir` whose first big tool output is written three times, with the
/// header's hash made again for the changed body. One recorded turn writes less than the smallest
/// replay buffer (65536 bytes), so a test that needs the host to drop output past the controller's
/// position needs more bytes than the recording holds. The copies sit inside the dropped stretch, so
/// the decoder never reads them. Returns the file's absolute path as text.
pub fn tripled_big_output(dir: &Path) -> String {
    let text = std::fs::read_to_string(fixture("big_output")).unwrap();
    let mut lines = text.lines();
    let header: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    let mut body: Vec<String> = Vec::new();
    for line in lines {
        body.push(line.to_string());
        let big = serde_json::from_str::<Value>(line)
            .is_ok_and(|v| v["to"] == "host" && v["msg"]["type"] == "user" && line.len() > 30_000);
        if big && body.iter().filter(|l| l.len() > 30_000).count() == 1 {
            body.push(line.to_string());
            body.push(line.to_string());
        }
    }
    let mut header = header;
    header["pf_fixture"]["body_sha256"] =
        Value::String(pf_host::sha256::sha256_hex(body.join("\n").as_bytes()));
    let path = dir.join("big_output_tripled.jsonl");
    std::fs::write(&path, format!("{header}\n{}\n", body.join("\n"))).unwrap();
    path.display().to_string()
}

/// The `result` lines of a fixture: `total_cost_usd` of each, in micros, as the decoder rounds it.
pub fn result_micros(name: &str) -> Vec<u64> {
    std::fs::read_to_string(fixture(name))
        .unwrap()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["to"] == "host" && v["msg"]["type"] == "result")
        .map(|v| (v["msg"]["total_cost_usd"].as_f64().unwrap_or(0.0) * 1e6).round() as u64)
        .collect()
}

/// How many host lines a fixture holds before its first line of top-level type `ty`.
pub fn host_lines_before(name: &str, ty: &str) -> u64 {
    let mut n = 0;
    for line in std::fs::read_to_string(fixture(name)).unwrap().lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v["to"] != "host" {
            continue;
        }
        if v["msg"]["type"] == ty {
            return n;
        }
        n += 1;
    }
    panic!("{name} has no host line of type {ty}");
}

/// The text of the first assistant block of a fixture.
pub fn first_assistant_text(name: &str) -> String {
    for line in std::fs::read_to_string(fixture(name)).unwrap().lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v["to"] == "host"
            && v["msg"]["type"] == "assistant"
            && let Some(blocks) = v["msg"]["message"]["content"].as_array()
            && let Some(text) = blocks.iter().find_map(|b| b["text"].as_str())
        {
            return text.to_string();
        }
    }
    panic!("{name} has no assistant text");
}

pub fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| !s.contains(") Z "))
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// One stored chat item.
#[derive(Clone, Debug)]
pub struct Item {
    pub seq: i64,
    pub kind: String,
    pub payload: Value,
}

/// A chat session row, as far as the tests read it.
#[derive(Clone, Debug)]
pub struct Row {
    pub state: String,
    pub ended_at: Option<String>,
    pub cost_micros: i64,
    pub runtime_ref: Option<String>,
    pub model: String,
    pub host_cursor: Option<Value>,
}

pub struct ChatRig {
    pub r: Restart,
    /// The wrapper's records: `argv.<n>`, `pid.<n>`, `progress.<n>` and the gate files.
    pub dir: PathBuf,
    pub wrapper: PathBuf,
    pub repo: PathBuf,
    pub project: String,
    pub session: String,
}

impl ChatRig {
    /// A controller whose chat runtime is `ClaudeChat` over the replay of `fixture`. `gates` are
    /// `(top-level type, gate name)` pairs, used in order on a fresh launch; the replay holds before
    /// the line of that type until `release(<gate name>)`. A launch with `--resume` plays
    /// `resume_fixture` (or `fixture`) with no gate. `env` is more controller environment. The
    /// controller runs, and the session `S-1` is started.
    pub fn new(
        fixture_name: &str,
        resume_fixture: Option<&str>,
        gates: &[(&str, &str)],
        env: &[(&str, &str)],
    ) -> ChatRig {
        ChatRig::with(fixture_name, resume_fixture, gates, env, &[])
    }

    /// [`ChatRig::new`], with server settings (`settings set <key> <value>`) made before the
    /// session starts, such as `session.replay_bytes`.
    pub fn with(
        fixture_name: &str,
        resume_fixture: Option<&str>,
        gates: &[(&str, &str)],
        env: &[(&str, &str)],
        settings: &[(&str, &str)],
    ) -> ChatRig {
        let mut r = Restart::new();
        let dir = r.root.join("chatd");
        std::fs::create_dir_all(&dir).unwrap();
        let gate_args: String = gates
            .iter()
            .map(|(ty, name)| format!(" --gate '{ty}:{}/gate.{name}'", dir.display()))
            .collect();
        let resume_fixture = resume_fixture.unwrap_or(fixture_name);
        let body = format!(
            "#!/bin/sh\n\
             if [ \"$1\" = \"--version\" ]; then exec '{replay}' --fixture '{fixture}' \"$@\"; fi\n\
             n=$(ls '{d}' | grep -c '^argv\\.')\n\
             printf '%s\\0' \"$@\" > '{d}/argv.'$n\n\
             echo $$ > '{d}/pid.'$n\n\
             case \" $* \" in\n\
             *\" --resume \"*) exec '{replay}' --fixture '{resume}' --progress '{d}/progress.'$n \"$@\" ;;\n\
             esac\n\
             exec '{replay}' --fixture '{fixture}'{gates} --progress '{d}/progress.'$n \"$@\"\n",
            replay = replay_bin().display(),
            fixture = fixture(fixture_name).display(),
            resume = fixture(resume_fixture).display(),
            d = dir.display(),
            gates = gate_args,
        );
        let wrapper = dir.join("claude-wrapper");
        std::fs::write(&wrapper, body).unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        r.extra_env
            .push(("PF_CHAT_CLAUDE_BIN".into(), wrapper.display().to_string()));
        for (k, v) in env {
            r.extra_env.push((k.to_string(), v.to_string()));
        }
        r.start_controller();
        for (key, value) in settings {
            let out = r.cli(&["settings", "set", key, value]);
            assert!(
                out.status.success(),
                "settings set {key}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let repo = r.root.join("work").join("alpha");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(
            &repo,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.invalid",
                "commit",
                "--allow-empty",
                "-q",
                "-m",
                "x",
            ],
        );
        let mut rig = ChatRig {
            r,
            dir,
            wrapper,
            repo,
            project: String::new(),
            session: String::new(),
        };
        let added = rig.json(&["project", "add", &rig.repo.display().to_string()]);
        rig.project = added["project"]["id"].as_str().unwrap().to_string();
        let started = rig.json(&["session", "start", "--project", &rig.project.clone()]);
        rig.session = started["session"]["id"].as_str().unwrap().to_string();
        rig
    }

    /// Run the CLI with `--json` and parse the answer. It fails the test when the verb refuses.
    pub fn json(&self, args: &[&str]) -> Value {
        let mut all = args.to_vec();
        all.push("--json");
        let out = self.r.cli(&all);
        assert!(
            out.status.success(),
            "{args:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{args:?}: {e}: {}", String::from_utf8_lossy(&out.stdout)))
    }

    /// Run the CLI and answer the refusal text (exit 1).
    pub fn refused(&self, args: &[&str]) -> String {
        let out = self.r.cli(args);
        assert_eq!(out.status.code(), Some(1), "{args:?} should refuse");
        String::from_utf8_lossy(&out.stderr).into_owned()
    }

    pub fn send(&self, text: &str) {
        self.json(&["session", "send", &self.session, text]);
    }

    // ------------------------------------------------------------------ the wrapper's records

    pub fn gate_path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("gate.{name}"))
    }

    pub fn release(&self, name: &str) {
        std::fs::write(self.gate_path(name), b"").unwrap();
    }

    pub fn launches(&self) -> usize {
        std::fs::read_dir(&self.dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("argv."))
            .count()
    }

    pub fn argv(&self, n: usize) -> Vec<String> {
        std::fs::read(self.dir.join(format!("argv.{n}")))
            .unwrap()
            .split(|b| *b == 0)
            .filter(|p| !p.is_empty())
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .collect()
    }

    pub fn pid(&self, n: usize) -> u32 {
        self.r.wait_for(
            &format!("the pid of launch {n}"),
            Duration::from_secs(30),
            || self.dir.join(format!("pid.{n}")).exists(),
        );
        std::fs::read_to_string(self.dir.join(format!("pid.{n}")))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    pub fn progress(&self, n: usize) -> u64 {
        std::fs::read_to_string(self.dir.join(format!("progress.{n}")))
            .ok()
            .and_then(|t| t.trim().parse().ok())
            .unwrap_or(0)
    }

    pub fn wait_progress(&self, n: usize, at_least: u64) {
        self.r.wait_for(
            &format!("launch {n} to write {at_least} host lines"),
            Duration::from_secs(30),
            || self.progress(n) >= at_least,
        );
    }

    /// The transcript stand-in a `--resume` open looks for.
    pub fn write_transcript(&self, runtime_ref: &str) {
        let dir = self.r.root.join(".claude/projects/-demo");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{runtime_ref}.jsonl")), b"{}\n").unwrap();
    }

    // ------------------------------------------------------------------ the store, read only

    fn db(&self) -> rusqlite::Connection {
        let db = rusqlite::Connection::open_with_flags(
            self.r.state.join("store.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        db.busy_timeout(Duration::from_secs(5)).unwrap();
        db
    }

    pub fn items(&self) -> Vec<Item> {
        let db = self.db();
        let mut stmt = db
            .prepare("SELECT seq, kind, payload FROM chat_items WHERE session_id = ?1 ORDER BY seq")
            .unwrap();
        stmt.query_map([&self.session], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .map(|(seq, kind, payload)| Item {
            seq,
            kind,
            payload: serde_json::from_str(&payload).unwrap_or(Value::Null),
        })
        .collect()
    }

    pub fn kinds(&self) -> Vec<String> {
        self.items().into_iter().map(|i| i.kind).collect()
    }

    pub fn count(&self, kind: &str) -> usize {
        self.items().iter().filter(|i| i.kind == kind).count()
    }

    pub fn notices(&self, code: &str) -> Vec<Item> {
        self.items()
            .into_iter()
            .filter(|i| i.kind == "notice" && i.payload["code"] == code)
            .collect()
    }

    pub fn row(&self) -> Row {
        let db = self.db();
        db.query_row(
            "SELECT state, ended_at, cost_micros, runtime_session_ref, model, host_cursor \
             FROM chat_sessions WHERE id = ?1",
            [&self.session],
            |r| {
                Ok(Row {
                    state: r.get(0)?,
                    ended_at: r.get(1)?,
                    cost_micros: r.get(2)?,
                    runtime_ref: r.get(3)?,
                    model: r.get(4)?,
                    host_cursor: r
                        .get::<_, Option<String>>(5)?
                        .and_then(|t| serde_json::from_str(&t).ok()),
                })
            },
        )
        .unwrap()
    }

    /// `(revision, record JSON)` of every launch record of the session, oldest first, by
    /// incarnation and revision.
    pub fn records(&self) -> Vec<(i64, i64, Value)> {
        let db = self.db();
        let mut stmt = db
            .prepare(
                "SELECT incarnation, revision, record FROM launch_records \
                 WHERE session_kind = 'chat' AND session_id = ?1 ORDER BY incarnation, revision",
            )
            .unwrap();
        stmt.query_map([&self.session], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .map(|(i, rev, text)| (i, rev, serde_json::from_str(&text).unwrap()))
        .collect()
    }

    pub fn wait_items(&self, what: &str, ok: impl Fn(&[Item]) -> bool) {
        self.r
            .wait_for(what, Duration::from_secs(30), || ok(&self.items()));
    }

    /// The chat host rows of the session: `(unit, ended_at)`, oldest first.
    pub fn host_rows(&self) -> Vec<(String, Option<String>)> {
        self.r
            .host_rows()
            .into_iter()
            .filter(|row| row.3 == self.session)
            .map(|row| (row.0, row.1))
            .collect()
    }

    /// The unit of the newest host row of the session.
    pub fn unit(&self) -> String {
        self.r
            .wait_for("the chat's host row", Duration::from_secs(30), || {
                !self.host_rows().is_empty()
            });
        self.host_rows().pop().unwrap().0
    }

    pub fn chat_ledger(&self) -> Vec<String> {
        std::fs::read_to_string(self.r.state.join("chat.ledger"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    pub fn chat_lines(&self, word: &str) -> Vec<String> {
        self.chat_ledger()
            .into_iter()
            .filter(|l| l.contains(&format!(" chat {word} ")))
            .collect()
    }

    /// A git repo with one commit, and a project on it: `(project ID, repo path)`.
    pub fn new_project(&self, name: &str) -> (String, PathBuf) {
        let repo = self.r.root.join("work").join(name);
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(
            &repo,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.invalid",
                "commit",
                "--allow-empty",
                "-q",
                "-m",
                "x",
            ],
        );
        let added = self.json(&["project", "add", &repo.display().to_string()]);
        (added["project"]["id"].as_str().unwrap().to_string(), repo)
    }

    /// Start a chat session on a project: its ID.
    pub fn start_session(&self, project: &str) -> String {
        let v = self.json(&["session", "start", "--project", project]);
        v["session"]["id"].as_str().unwrap().to_string()
    }

    /// A controller in this test process, on the rig's state directory, with `ClaudeChat` over the
    /// rig's wrapper and the detached launcher of the rig's runtime directory. The rig's own
    /// controller must be down (`kill9`). `tweak` adjusts the config, as the lifetime tests need
    /// (`chat_idle`, `chat_max_live`).
    pub fn in_process(&self, tweak: impl FnOnce(&mut Config)) -> Arc<Controller> {
        let mut claude = ClaudeConfig::new(self.r.share.clone());
        claude.claude_bin_override = Some(self.wrapper.clone());
        claude.config_dir = self.r.root.join(".claude");
        claude.inherited_env = Some(BTreeMap::from([
            (
                "PATH".to_string(),
                std::env::var("PATH").unwrap_or_default(),
            ),
            ("HOME".to_string(), self.r.root.display().to_string()),
        ]));
        let chat: Arc<dyn ChatPort> = Arc::new(ClaudeChat::new(claude));
        let mut cfg = Config::new(self.r.state.clone(), 3_600_000, BIN.into());
        cfg.chat_runtimes
            .insert(CLAUDE_CODE_RUNTIME.to_string(), chat);
        cfg.runtime_dir = self.r.rt.clone();
        cfg.host_bin = Some(host_bin());
        cfg.chat_share_dir = self.r.share.clone();
        cfg.server_name = "test-server".to_string();
        tweak(&mut cfg);
        Controller::open(cfg, Arc::new(NoRuns)).expect("the second controller opens")
    }

    /// `SIGKILL` the controller and start it again. The socket answers when tier 1 has finished.
    pub fn restart(&mut self) {
        self.r.kill9();
        self.r.start_controller();
    }
}

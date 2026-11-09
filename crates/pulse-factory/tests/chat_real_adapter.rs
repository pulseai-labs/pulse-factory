//! AC-10 of `r1.s3.w3`: the controller against the real Claude Code adapter, `ClaudeChat`, with
//! `claude_bin_override` set to a wrapper that records its argument list and its environment and
//! then `exec`s `pf-claude-replay` on a recorded fixture. No test starts the real `claude`, spends a
//! token or reaches a network. The one place that names the seat-mods plugin is the persona check
//! below, which proves the persona holds no such word.

mod common;

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::chat::*;
use common::ctl::Live;
use common::*;
use pf_adapter_claude::{CLAUDE_CODE_RUNTIME, ClaudeChat, ClaudeConfig};
use pf_engine::port::chat::ChatPort;
use pf_engine::port::store::Store;
use pf_ipc::Handler;
use serde_json::{Value, json};

/// `pf-claude-replay`, next to the test binaries' directory. It must be built first
/// (`cargo build -p pf-adapter-claude --bin pf-claude-replay`). A missing file fails the test.
fn replay_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap();
    let bin = dir.join("pf-claude-replay");
    assert!(
        bin.is_file(),
        "{} is missing: run `cargo build -p pf-adapter-claude --bin pf-claude-replay` first",
        bin.display()
    );
    bin
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../pf-adapter-claude/fixtures")
        .join(format!("{name}.jsonl"))
}

/// The wrapper: it records each launch's argument list and environment (NUL separated) in
/// `argv.<n>` and `env.<n>`, and its process id in `pid`, then runs the replay on the fixture.
fn wrapper(dir: &Path, fixture: &Path) -> PathBuf {
    // The adapter asks `--version` before each launch. That call is not a launch: it records
    // nothing and goes straight to the replay.
    let body = format!(
        "#!/bin/sh\n\
         if [ \"$1\" != \"--version\" ]; then\n\
         n=$(ls '{d}' | grep -c '^argv\\.')\n\
         printf '%s\\0' \"$@\" > '{d}/argv.'$n\n\
         env -0 > '{d}/env.'$n\n\
         echo $$ > '{d}/pid'\n\
         fi\n\
         exec '{r}' --fixture '{f}' \"$@\"\n",
        d = dir.display(),
        r = replay_bin().display(),
        f = fixture.display(),
    );
    let bin = dir.join("claude-wrapper");
    std::fs::write(&bin, body).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

fn nul_split(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect()
}

struct Real {
    live: Live,
    /// The wrapper's records.
    dir: tempfile::TempDir,
}

impl Real {
    fn argv(&self, n: usize) -> Vec<String> {
        nul_split(&std::fs::read(self.dir.path().join(format!("argv.{n}"))).unwrap())
    }

    fn env(&self, n: usize) -> BTreeMap<String, String> {
        nul_split(&std::fs::read(self.dir.path().join(format!("env.{n}"))).unwrap())
            .into_iter()
            .filter_map(|l| {
                l.split_once('=')
                    .map(|(k, v)| (k.to_string(), v.to_string()))
            })
            .collect()
    }

    fn launches(&self) -> usize {
        std::fs::read_dir(self.dir.path())
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("argv.")
            })
            .count()
    }

    fn pid(&self) -> u32 {
        std::fs::read_to_string(self.dir.path().join("pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }
}

/// A live controller whose runtime is `ClaudeChat` over the replay of `name`.
fn real(
    name: &str,
    tweak: impl FnOnce(&mut pulse_factory::controller::Config) + Send + 'static,
) -> Real {
    let dir = tempfile::tempdir().unwrap();
    let wrapper = wrapper(dir.path(), &fixture(name));
    let mut cc = ClaudeConfig::new(dir.path().join("share"));
    cc.claude_bin_override = Some(wrapper);
    cc.config_dir = dir.path().join("claude-config");
    cc.init_seconds = 30;
    cc.inherited_env = Some(BTreeMap::from([
        (
            "PATH".to_string(),
            std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string()),
        ),
        ("HOME".to_string(), dir.path().display().to_string()),
        ("PF_SHOULD_NOT_LEAK".to_string(), "1".to_string()),
    ]));
    let chat: Arc<dyn ChatPort> = Arc::new(ClaudeChat::new(cc));
    let live = Live::start_config(BIN.into(), move |cfg| {
        cfg.chat_runtimes
            .insert(CLAUDE_CODE_RUNTIME.to_string(), chat);
        cfg.server_name = "test-server".to_string();
        cfg.chat_agent_env.insert(
            "ORCHESTRATOR".to_string(),
            [("EXAMPLE_ROLE".to_string(), "orchestrator".to_string())].into(),
        );
        tweak(cfg);
    });
    Real { live, dir }
}

fn value_after(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

#[test]
fn the_seeded_row_passes_the_adapters_own_check_and_the_launch_carries_its_flags() {
    let real = real("init", |_| {});
    let live = &real.live;
    let root = repo(live, "alpha");
    let project = add_project(live, &root);
    let started = cli_json(live, &["session", "start", "--project", &project]);
    let s = started["session"]["id"].as_str().unwrap().to_string();

    // C4: the seeded row passes `ClaudeChat`'s `options()`, so the open is not refused.
    assert_eq!(real.launches(), 1);
    let argv = real.argv(0);
    assert_eq!(
        value_after(&argv, "--model").as_deref(),
        Some("claude-opus-5-5")
    );
    assert_eq!(value_after(&argv, "--effort").as_deref(), Some("high"));
    assert_eq!(
        value_after(&argv, "--permission-mode").as_deref(),
        Some("bypassPermissions")
    );
    let persona = value_after(&argv, "--append-system-prompt").expect("the persona");
    for word in ["alpha", &root.display().to_string(), "test-server", &s] {
        assert!(persona.contains(word), "the persona names {word}");
    }

    let env = real.env(0);
    assert_eq!(
        env.get("CLAUDE_CODE_ARTIFACT").map(String::as_str),
        Some("1")
    );
    assert_eq!(
        env.get("EXAMPLE_ROLE").map(String::as_str),
        Some("orchestrator")
    );
    let pf: Vec<_> = env.keys().filter(|k| k.starts_with("PF_")).collect();
    assert!(
        pf.is_empty(),
        "no PF_ name reaches the chat process: {pf:?}"
    );

    // A plain turn: the reply and a `turn_result` with the fixture's usage and a cost above 0.
    cli_json(live, &["session", "send", &s, "hello"]);
    wait_turns(live, &s, 1);
    let all = items(live, &s);
    let reply = all
        .iter()
        .find(|i| i.kind == "assistant_text")
        .expect("a reply");
    let reply: Value = serde_json::from_str(&reply.payload).unwrap();
    assert!(!reply["text"].as_str().unwrap().is_empty());
    let result = all.iter().find(|i| i.kind == "turn_result").unwrap();
    let result: Value = serde_json::from_str(&result.payload).unwrap();
    assert!(
        result["usage"]["cost_micros"].as_u64().unwrap() > 0,
        "{result}"
    );
    assert!(
        result["usage"]["output_tokens"].as_u64().unwrap() > 0,
        "{result}"
    );
    let row = live.controller.store().chat_session(&s).unwrap().unwrap();
    assert_eq!(
        row.cost_micros,
        result["usage"]["cost_micros"].as_u64().unwrap()
    );

    // An override binary is unqualified everywhere it shows.
    let listed = cli_json(live, &["session", "list"]);
    let process = &listed["sessions"][0]["process"];
    assert_eq!(process["qualified"], false, "{listed}");
    assert_eq!(process["runtime_version"], "2.1.319", "{listed}");
    let text = live.cli(&["session", "list"]);
    assert!(stdout(&text).contains("unqualified"), "{}", stdout(&text));
    let open = chat_ledger(live)
        .into_iter()
        .find(|l| l.contains(" chat open "))
        .unwrap();
    assert!(
        open.contains("qualified=false") && open.contains("version=2.1.319"),
        "{open}"
    );
    assert!(
        std::fs::read_to_string(live.sb.state.join("controller.ledger"))
            .unwrap()
            .contains("chat_send")
    );
}

#[test]
fn a_deny_on_the_recorded_permission_path_gets_its_reply() {
    let real = real("permissions", |_| {});
    let live = &real.live;
    let (_, s) = started(live, "alpha");
    cli_json(live, &["session", "set", &s, "--mode", "manual"]);
    cli_json(live, &["session", "send", &s, "list the files"]);
    wait_for(live, &s, "the approval request", |i| {
        i.iter().any(|x| x.kind == "approval_request")
    });
    let all = items(live, &s);
    let request = all.iter().find(|i| i.kind == "approval_request").unwrap();
    let body: Value = serde_json::from_str(&request.payload).unwrap();
    assert_eq!(body["tool"], "Bash", "{body}");
    let row = live.controller.store().chat_session(&s).unwrap().unwrap();
    assert_eq!(row.permission_mode, "manual");

    cli_json(
        live,
        &[
            "session",
            "answer",
            &s,
            &request.seq.to_string(),
            "--deny",
            "no",
        ],
    );
    wait_turns(live, &s, 1);
    let all = items(live, &s);
    let kinds: Vec<&str> = all.iter().map(|i| i.kind.as_str()).collect();
    let ask = kinds.iter().position(|k| *k == "approval_request").unwrap();
    let answer = kinds.iter().position(|k| *k == "approval_answer").unwrap();
    let reply = kinds.iter().position(|k| *k == "assistant_text").unwrap();
    let result = kinds.iter().position(|k| *k == "turn_result").unwrap();
    assert!(
        ask < answer && answer < reply && reply < result,
        "{kinds:?}"
    );
    let answer: Value = serde_json::from_str(&all[answer].payload).unwrap();
    assert_eq!(
        (answer["outcome"].as_str(), answer["decision"].as_str()),
        (Some("answered"), Some("deny"))
    );
}

#[test]
fn with_no_override_and_no_copy_the_start_names_the_install_step_and_runs_no_claude_from_path() {
    let stub_dir = tempfile::tempdir().unwrap();
    let marker = stub_dir.path().join("ran");
    let stub = stub_dir.path().join("claude");
    std::fs::write(&stub, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

    let share = tempfile::tempdir().unwrap();
    let mut cc = ClaudeConfig::new(share.path());
    cc.inherited_env = Some(BTreeMap::from([(
        "PATH".to_string(),
        format!("{}:/usr/bin:/bin", stub_dir.path().display()),
    )]));
    cc.config_dir = share.path().join("claude-config");
    let chat: Arc<dyn ChatPort> = Arc::new(ClaudeChat::new(cc));
    let share_dir = share.path().to_path_buf();
    let live = Live::start_config(BIN.into(), move |cfg| {
        cfg.chat_runtimes
            .insert(CLAUDE_CODE_RUNTIME.to_string(), chat);
        // The managed copy is what the controller checks for the runtime's readiness.
        cfg.chat_managed = true;
        cfg.chat_share_dir = share_dir;
    });
    let root = repo(&live, "alpha");
    let project = add_project(&live, &root);
    let why = cli_refused(&live, &["session", "start", "--project", &project]);
    assert!(
        why.contains("install step for the chat runtime"),
        "the refusal names the fix: {why}"
    );
    assert!(!marker.exists(), "the claude on PATH ran");
    // The runtime shows as not ready, with the same text.
    let listed = cli_json(&live, &["session", "list"]);
    let runtime = &listed["runtimes"][0];
    assert_eq!(runtime["ready"], false, "{listed}");
    assert!(
        runtime["error"]
            .as_str()
            .unwrap()
            .contains("chat-runtime install"),
        "{runtime}"
    );
}

#[test]
fn a_caller_that_descends_from_the_live_chat_process_is_refused() {
    let proc_dir = tempfile::tempdir().unwrap();
    let proc_root = proc_dir.path().to_path_buf();
    let real = real("init", move |cfg| cfg.proc_root = proc_root);
    let live = &real.live;
    let root = repo(live, "alpha");
    let added = live.controller.handle(pf_ipc::Request::new(
        "project_add",
        json!({"root": root.display().to_string(), "name": null, "command_id": "add-1"}),
    ));
    assert!(added.ok, "{:?}", added.error);
    let project = added.result["project"]["id"].as_str().unwrap().to_string();
    let started = live.controller.handle(pf_ipc::Request::new(
        "session_start",
        json!({"project_id": project, "command_id": "start-1"}),
    ));
    assert!(started.ok, "{:?}", started.error);

    // The wrapper `exec`s the replay, so the pid it wrote is the chat process.
    let pid = real.pid();
    fake_proc(proc_dir.path(), b"HOME=/x\0", pid);
    let send = pf_ipc::call(
        &live.sb.state,
        "session_send",
        json!({"session_id": "S-1", "text": "hi", "command_id": "c-send"}),
    )
    .unwrap();
    assert!(!send.ok);
    assert!(
        send.error.unwrap().contains(&format!("chat process {pid}")),
        "the refusal names the chat process"
    );
    let start = pf_ipc::call(
        &live.sb.state,
        "session_start",
        json!({"project_id": project, "command_id": "c-start"}),
    )
    .unwrap();
    assert!(!start.ok);
    assert_eq!(ledger_kind(live, "chat_send").len(), 0);
    assert_eq!(ledger_kind(live, "chat_session_start").len(), 1);
}

#[test]
fn the_persona_holds_the_facts_and_none_of_the_banned_words_and_two_processes_get_one_text() {
    let real = real("init", |cfg| cfg.chat_idle = Duration::ZERO);
    let live = &real.live;
    let root = repo(live, "alpha");
    let project = add_project(live, &root);
    let started = cli_json(live, &["session", "start", "--project", &project]);
    let s = started["session"]["id"].as_str().unwrap().to_string();
    let first = value_after(&real.argv(0), "--append-system-prompt").expect("a persona");

    assert!(first.contains("alpha") && first.contains(&root.display().to_string()));
    assert!(first.contains("test-server"));
    assert!(
        first.contains(
            "Nothing but an operator message, or a background task that you started, begins a turn"
        ),
        "{first}"
    );
    let lower = first.to_lowercase();
    for banned in ["seat-mods", "sleep", "cron", "schedulewakeup", "monitor"] {
        assert!(!lower.contains(banned), "the persona holds {banned}");
    }

    // A release, a transcript for the resume check, and a second process of the same session.
    let stored = live
        .controller
        .store()
        .chat_session(&s)
        .unwrap()
        .unwrap()
        .runtime_session_ref
        .unwrap();
    let config = real.dir.path().join("claude-config/projects/-some-project");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join(format!("{stored}.jsonl")), "{}\n").unwrap();
    live.controller.sweep_chats();
    assert!(live.controller.live_chat_sessions().is_empty());
    cli_json(live, &["session", "send", &s, "again"]);
    wait_turns(live, &s, 1);
    assert_eq!(real.launches(), 2);
    let second = value_after(&real.argv(1), "--append-system-prompt").expect("a persona");
    assert_eq!(first, second, "one session, one persona");
    assert!(
        real.argv(1).iter().any(|a| a == "--resume"),
        "the second launch resumes: {:?}",
        real.argv(1)
    );
}

#[test]
fn the_persona_names_the_skill_at_the_real_share_dir_and_that_file_exists() {
    // `scripts/install.sh` copies `skill/` to `<share dir>/skill`. The test does the same.
    let share = tempfile::tempdir().unwrap();
    let skill_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skill/SKILL.md");
    assert!(skill_src.is_file(), "{}", skill_src.display());
    std::fs::create_dir_all(share.path().join("skill")).unwrap();
    std::fs::copy(&skill_src, share.path().join("skill/SKILL.md")).unwrap();
    let share_path = share.path().to_path_buf();

    let real = real("init", move |cfg| cfg.chat_share_dir = share_path);
    let live = &real.live;
    let root = repo(live, "alpha");
    let project = add_project(live, &root);
    cli_json(live, &["session", "start", "--project", &project]);

    let persona = value_after(&real.argv(0), "--append-system-prompt").expect("a persona");
    assert!(
        !persona.contains("<share dir>"),
        "a placeholder is left: {persona}"
    );
    let named = share.path().join("skill/SKILL.md");
    assert!(
        persona.contains(&named.display().to_string()),
        "the persona names {}: {persona}",
        named.display()
    );
    assert!(named.is_file(), "the named path exists");
}

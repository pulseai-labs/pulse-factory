//! Helpers for the adapter tests. Every test uses a shell stub or `pf-claude-replay` and spends nothing.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use serde_json::Value;

pub mod propose_rule;

/// The fixtures directory of the crate.
pub fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

/// One fixture: its header, and its body as `(to, msg)` pairs, in order.
pub struct Fixture {
    pub name: String,
    pub header: Value,
    pub body: Vec<(String, Value)>,
}

pub fn load_fixture(name: &str) -> Fixture {
    let path = fixtures_dir().join(format!("{name}.jsonl"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut lines = text.lines().filter(|l| !l.is_empty());
    let header: Value = serde_json::from_str(lines.next().expect("a header")).unwrap();
    let body = lines
        .map(|l| {
            let v: Value = serde_json::from_str(l).unwrap();
            (v["to"].as_str().unwrap().to_string(), v["msg"].clone())
        })
        .collect();
    Fixture {
        name: name.to_string(),
        header,
        body,
    }
}

/// Every fixture in the directory, by file name.
pub fn all_fixtures() -> Vec<Fixture> {
    let mut names: Vec<String> = std::fs::read_dir(fixtures_dir())
        .unwrap()
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            n.strip_suffix(".jsonl").map(str::to_string)
        })
        .collect();
    names.sort();
    names.iter().map(|n| load_fixture(n)).collect()
}

// ------------------------------------------------------------------ a managed copy and a stub

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;

use pf_adapter_claude::pinned::{self, Pin};
use pf_adapter_claude::{ClaudeChat, ClaudeConfig};
use pf_engine::claim::sha256_hex;
use pf_engine::port::chat::ChatOpen;

/// Shell that answers the `initialize` request: it reads the first line and writes a success
/// response with the same request id. Put it first in a stub body.
pub const ANSWER_INITIALIZE: &str = r#"read -r line
id=$(printf '%s' "$line" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')
printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{}}}\n' "$id"
"#;

/// A stub `claude` placed as the managed copy in a temporary `share_dir`, with a pin made from its
/// SHA-256 and size.
pub struct Managed {
    /// Holds `share_dir`; the stub writes its records in `dir`.
    pub root: tempfile::TempDir,
    pub share: PathBuf,
    pub dir: PathBuf,
    pub bin: PathBuf,
    pub pin: Pin,
}

/// Write a stub. `--version` prints `version_line` and records `version` in `$DIR/calls`. Any other
/// call records its arguments, NUL-separated, in `$DIR/argv`, then runs `body` (a POSIX shell
/// snippet; `$DIR` is the records directory).
pub fn stub_script(dir: &Path, version_line: &str, body: &str) -> String {
    format!(
        "#!/bin/sh\nDIR='{}'\nif [ \"$1\" = \"--version\" ]; then echo version >> \"$DIR/calls\"; echo '{}'; exit 0; fi\necho run >> \"$DIR/calls\"\nprintf '%s\\0' \"$@\" > \"$DIR/argv\"\n{}\n",
        dir.display(),
        version_line,
        body
    )
}

pub fn managed(version_line: &str, body: &str) -> Managed {
    let root = tempfile::tempdir().unwrap();
    let share = root.path().join("share");
    let dir = root.path().join("records");
    std::fs::create_dir_all(&dir).unwrap();
    let bin_dir = share.join("claude").join(pinned::CLI_VERSION);
    std::fs::create_dir_all(&bin_dir).unwrap();
    let bin = bin_dir.join("claude");
    let script = stub_script(&dir, version_line, body);
    std::fs::write(&bin, &script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let pin = Pin::single(
        pinned::CLI_VERSION,
        pinned::host_platform().expect("a pinned platform"),
        &sha256_hex(script.as_bytes()),
        script.len() as u64,
    );
    Managed {
        root,
        share,
        dir,
        bin,
        pin,
    }
}

/// The usual stub: a qualified version, answers `initialize`, then reads stdin until it closes.
pub fn good_stub() -> Managed {
    managed(
        &format!("{} (Claude Code)", pinned::CLI_VERSION),
        &format!("{ANSWER_INITIALIZE}cat > /dev/null"),
    )
}

pub fn config(m: &Managed) -> ClaudeConfig {
    let mut c = ClaudeConfig::new(&m.share);
    c.pin = m.pin.clone();
    c.config_dir = m.root.path().join("claude-config");
    c.init_seconds = 10;
    c.control_seconds = 5;
    c.inherited_env = Some(BTreeMap::new());
    c
}

pub fn chat(m: &Managed) -> ClaudeChat {
    ClaudeChat::new(config(m))
}

/// A `ChatOpen` with the pinned defaults, in a directory that exists.
pub fn spec(cwd: &Path) -> ChatOpen {
    ChatOpen {
        cwd: cwd.display().to_string(),
        resume: None,
        instructions: "be brief".into(),
        model: "claude-opus-5-5".into(),
        effort: "high".into(),
        permission_mode: "bypassPermissions".into(),
        env: BTreeMap::new(),
        options: BTreeMap::new(),
        deny_commands: Vec::new(),
    }
}

/// The arguments the stub recorded, in order.
pub fn recorded_argv(m: &Managed) -> Vec<String> {
    let raw = std::fs::read(m.dir.join("argv")).expect("the stub recorded its argv");
    raw.split(|b| *b == 0)
        .filter(|a| !a.is_empty() || false)
        .map(|a| String::from_utf8_lossy(a).to_string())
        .collect()
}

/// The calls the stub saw: `version` or `run`.
pub fn recorded_calls(m: &Managed) -> Vec<String> {
    std::fs::read_to_string(m.dir.join("calls"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// Is `s` a version-4 UUID?
pub fn is_uuid_v4(s: &str) -> bool {
    let b = s.as_bytes();
    s.len() == 36
        && [8, 13, 18, 23].iter().all(|i| b[*i] == b'-')
        && b[14] == b'4'
        && matches!(b[19], b'8' | b'9' | b'a' | b'b')
        && s.chars().enumerate().all(|(i, c)| {
            [8, 13, 18, 23].contains(&i) || c.is_ascii_hexdigit() && !c.is_ascii_uppercase()
        })
}

// ------------------------------------------------------------------ the replay binary as the CLI

use std::time::{Duration, Instant};

use pf_engine::port::chat::{ChatEvent, ChatEvents, ChatOpened, ChatPort};
use serde_json::json;

/// The shipped replay binary.
pub const REPLAY: &str = env!("CARGO_BIN_EXE_pf-claude-replay");

/// Write a fixture with a valid header, from `(to, msg)` pairs. `t_ms` is five per line. Returns the
/// path. Used for scenes no recording holds; the recorder's own header is copied in shape only.
pub fn write_fixture(dir: &Path, case: &str, body: &[(&str, Value)]) -> PathBuf {
    let timed: Vec<(&str, u64, Value)> = body
        .iter()
        .enumerate()
        .map(|(i, (to, msg))| (*to, i as u64 * 5, msg.clone()))
        .collect();
    write_fixture_timed(dir, case, &timed)
}

/// Like [`write_fixture`], with the recorded time of each line.
pub fn write_fixture_timed(dir: &Path, case: &str, body: &[(&str, u64, Value)]) -> PathBuf {
    let lines: Vec<String> = body
        .iter()
        .map(|(to, t_ms, msg)| {
            serde_json::to_string(&json!({"to": to, "t_ms": t_ms, "msg": msg})).unwrap()
        })
        .collect();
    let header = json!({"pf_fixture": {
        "cli_version": pinned::CLI_VERSION,
        "recorded": "2026-11-08",
        "recorder": "real_probe_chat",
        "case": case,
        "body_sha256": sha256_hex(lines.join("\n").as_bytes()),
    }});
    let path = dir.join(format!("{case}.jsonl"));
    let mut text = serde_json::to_string(&header).unwrap();
    text.push('\n');
    text.push_str(&lines.join("\n"));
    text.push('\n');
    std::fs::write(&path, text).unwrap();
    path
}

/// A copy of a recorded fixture with its body changed by `edit`, and a header that matches.
pub fn edited_fixture(dir: &Path, name: &str, edit: impl Fn(&str, &mut Value)) -> PathBuf {
    let f = load_fixture(name);
    let body: Vec<(String, Value)> = f
        .body
        .into_iter()
        .map(|(to, mut msg)| {
            edit(&to, &mut msg);
            (to, msg)
        })
        .collect();
    let refs: Vec<(&str, Value)> = body.iter().map(|(t, m)| (t.as_str(), m.clone())).collect();
    write_fixture(dir, &format!("{name}_edited"), &refs)
}

/// A claude replacement: a wrapper script that runs the replay on `fixture`. With `record`, every
/// line the adapter writes is copied to `<dir>/stdin` first; the leader's pid goes to `<dir>/pid`.
pub fn replay_wrapper(dir: &Path, fixture: &Path, paced: bool, record: bool) -> PathBuf {
    let pace = if paced { "--pace recorded" } else { "" };
    let body = if record {
        format!(
            "echo $$ > '{d}/pid'\ntee -a '{d}/stdin' | '{r}' --fixture '{f}' {pace} \"$@\"\n",
            d = dir.display(),
            r = REPLAY,
            f = fixture.display()
        )
    } else {
        format!(
            "echo $$ > '{d}/pid'\nexec '{r}' --fixture '{f}' {pace} \"$@\"\n",
            d = dir.display(),
            r = REPLAY,
            f = fixture.display()
        )
    };
    let bin = dir.join("claude-replay");
    std::fs::write(&bin, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// A chat over the replay of one fixture, with its records directory.
pub struct Rig {
    pub root: tempfile::TempDir,
    pub dir: PathBuf,
    pub work: tempfile::TempDir,
    pub chat: ClaudeChat,
    /// A directory the rig keeps alive for the test, such as the one holding a hand-made fixture.
    pub extra: Option<tempfile::TempDir>,
}

pub fn rig_with(
    fixture: &Path,
    paced: bool,
    record: bool,
    tweak: impl FnOnce(&mut ClaudeConfig),
) -> Rig {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("records");
    std::fs::create_dir_all(&dir).unwrap();
    let mut cfg = ClaudeConfig::new(root.path().join("share"));
    cfg.claude_bin_override = Some(replay_wrapper(&dir, fixture, paced, record));
    cfg.config_dir = root.path().join("claude-config");
    cfg.init_seconds = 10;
    cfg.control_seconds = 5;
    cfg.inherited_env = Some(BTreeMap::new());
    tweak(&mut cfg);
    Rig {
        root,
        dir,
        work: tempfile::tempdir().unwrap(),
        chat: ClaudeChat::new(cfg),
        extra: None,
    }
}

pub fn rig(fixture: &Path) -> Rig {
    rig_with(fixture, false, false, |_| {})
}

impl Rig {
    pub fn open(&self) -> ChatOpened {
        self.chat.open(&spec(self.work.path())).expect("open")
    }

    /// The lines the adapter wrote (needs `record`), parsed.
    pub fn stdin_lines(&self) -> Vec<Value> {
        std::fs::read_to_string(self.dir.join("stdin"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{l:?}: {e}")))
            .collect()
    }

    /// Wait until the adapter has written a line that `want` accepts. Returns the lines.
    pub fn wait_stdin(&self, want: impl Fn(&[Value]) -> bool) -> Vec<Value> {
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            let lines = self.stdin_lines();
            if want(&lines) {
                return lines;
            }
            assert!(Instant::now() < end, "the adapter wrote only {lines:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// Take events until `done` accepts what came, up to `secs`. Panics with what came otherwise.
pub fn collect_until(
    events: &mut ChatEvents,
    secs: u64,
    done: impl Fn(&[ChatEvent]) -> bool,
) -> Vec<ChatEvent> {
    let end = Instant::now() + Duration::from_secs(secs);
    let mut got = Vec::new();
    loop {
        while let Ok(e) = events.try_recv() {
            got.push(e);
        }
        if done(&got) {
            return got;
        }
        assert!(
            Instant::now() < end,
            "the events did not come in {secs} s; got {} events: {got:#?}",
            got.len()
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

pub fn count_turn_results(events: &[ChatEvent]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, ChatEvent::TurnResult { .. }))
        .count()
}

/// Stop the chat and return what is left on the channel, ending with `Exited`.
pub fn stop_and_drain(chat: &ClaudeChat, opened: &mut ChatOpened) -> Vec<ChatEvent> {
    chat.stop(&opened.chat).unwrap();
    std::iter::from_fn(|| opened.events.try_recv().ok()).collect()
}

// ------------------------------------------------------------------ scenes built by hand

/// Builders for the lines of a hand-made scene. A scene is a list of `(to, msg)`.
pub mod scene {
    use serde_json::{Value, json};

    pub type Line = (&'static str, Value);

    pub fn init_request() -> Line {
        (
            "cli",
            json!({"type": "control_request", "request_id": "req-1", "request": {"subtype": "initialize"}}),
        )
    }

    pub fn control_ok(id: &str, body: Value) -> Line {
        (
            "host",
            json!({"type": "control_response", "response": {"subtype": "success", "request_id": id, "response": body}}),
        )
    }

    pub fn control_error(id: &str, text: &str) -> Line {
        (
            "host",
            json!({"type": "control_response", "response": {"subtype": "error", "request_id": id, "error": text}}),
        )
    }

    /// The first two lines of every scene: `initialize` and its answer.
    pub fn open() -> Vec<Line> {
        vec![init_request(), control_ok("req-1", json!({}))]
    }

    pub fn user(uuid: &str, text: &str) -> Line {
        (
            "cli",
            json!({"type": "user", "message": {"role": "user", "content": text}, "parent_tool_use_id": null, "uuid": uuid}),
        )
    }

    pub const SESSION: &str = "recorded-session";

    pub fn sys_init(uuids: &[&str]) -> Line {
        (
            "host",
            json!({"type": "system", "subtype": "init", "apiKeySource": "none",
                "tools": ["Bash"], "claude_code_version": "2.1.319", "session_id": SESSION,
                "permissionMode": "bypassPermissions", "user_message_uuids": uuids}),
        )
    }

    pub fn delta(text: &str) -> Line {
        (
            "host",
            json!({"type": "stream_event", "parent_tool_use_id": null, "session_id": SESSION,
                "event": {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}}}),
        )
    }

    pub fn text(text: &str) -> Line {
        (
            "host",
            json!({"type": "assistant", "parent_tool_use_id": null, "session_id": SESSION,
                "message": {"role": "assistant", "content": [{"type": "text", "text": text}]}}),
        )
    }

    /// A `result` with the running totals: cost in dollars and one model's token counts.
    pub fn result(cost: f64, input: u64, output: u64, uuids: &[&str]) -> Line {
        (
            "host",
            json!({"type": "result", "subtype": "success", "is_error": false, "duration_ms": 7,
                "stop_reason": "end_turn", "total_cost_usd": cost, "session_id": SESSION,
                "usage": {"cache_creation": {"ephemeral_1h_input_tokens": 0, "ephemeral_5m_input_tokens": 0}},
                "modelUsage": {"m": {"inputTokens": input, "outputTokens": output,
                    "cacheReadInputTokens": 0, "cacheCreationInputTokens": 0}},
                "user_message_uuids": uuids}),
        )
    }

    /// One whole turn for message `uuid`: init, a reply, a result.
    pub fn turn(uuid: &str, reply: &str, cost: f64, input: u64, output: u64) -> Vec<Line> {
        vec![
            sys_init(&[uuid]),
            delta(reply),
            text(reply),
            result(cost, input, output, &[uuid]),
        ]
    }

    /// An inbound `can_use_tool` for `tool`.
    pub fn can_use(id: &str, tool: &str, input: Value) -> Line {
        (
            "host",
            json!({"type": "control_request", "request_id": id,
                "request": {"subtype": "can_use_tool", "tool_name": tool, "input": input,
                            "tool_use_id": format!("tu-{id}"), "description": "a description"}}),
        )
    }

    pub fn cli_answer(id: &str) -> Line {
        (
            "cli",
            json!({"type": "control_response", "response": {"subtype": "success", "request_id": id, "response": {}}}),
        )
    }
}

/// A transcript under `<config_dir>/projects/*/<id>.jsonl`, so a resume of `id` can start.
pub fn put_transcript(config_dir: &Path, id: &str) {
    let dir = config_dir.join("projects").join("-some-project");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{id}.jsonl")), "{}\n").unwrap();
}

/// A process is gone when it has no `/proc` entry, or only a zombie one that its parent has not
/// reaped yet.
pub fn is_gone(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => true,
        Ok(stat) => {
            stat.rsplit_once(')')
                .and_then(|(_, rest)| rest.trim_start().chars().next())
                == Some('Z')
        }
    }
}

pub fn wait_gone(pid: u32) -> bool {
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        if is_gone(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    is_gone(pid)
}

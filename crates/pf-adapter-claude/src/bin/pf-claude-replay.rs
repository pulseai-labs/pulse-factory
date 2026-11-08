//! `pf-claude-replay`: stands in for the `claude` binary and plays one recorded fixture back.
//!
//! It never reaches a network, spends a token or starts another program. Tests and demos run the
//! real `ClaudeChat` against it (`claude_bin_override`), so a chat session needs no tokens.
//!
//! ```text
//! pf-claude-replay --fixture <file> [--pace recorded|none] [--version] [CLI flags…]
//! ```
//!
//! - `--version` prints `<cli_version> (Claude Code)` from the fixture's header.
//! - The header and `body_sha256` are checked first. A fixture that is unreadable, has no header,
//!   or whose body changed after recording ends with exit 2 and a message on stderr.
//! - The body plays in order. For each `to: cli` line the replay waits for the next line on stdin of
//!   the same `type` (and `subtype`, for a control message), then writes the `to: host` lines that
//!   follow, up to the next `to: cli` line. It puts the request ids and message UUIDs it read in
//!   place of the recorded ones, and the session id of its own argv (`--session-id` or `--resume`)
//!   in place of the recorded one.
//! - A repeated `initialize` in a recording was sent by the recorder to look at a snapshot. The
//!   adapter sends `initialize` once, so the replay does not wait for a second one.
//! - `--pace recorded` keeps the recorded gaps between lines. The default writes at once.
//! - A line on stdin that is not the one awaited is logged on stderr and skipped. The end of stdin
//!   ends the replay with exit 0, also in the middle of a fixture.
//! - Every other CLI flag is ignored. The replay holds no secret and reads no file but the fixture.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::process::ExitCode;
use std::time::Duration;

use pf_engine::claim::sha256_hex;
use serde_json::Value;

struct Line {
    to: String,
    t_ms: u64,
    msg: Value,
}

struct Fixture {
    cli_version: String,
    body: Vec<Line>,
}

struct Args {
    fixture: Option<String>,
    version: bool,
    paced: bool,
    session: Option<String>,
}

fn fail(message: &str) -> ExitCode {
    eprintln!("pf-claude-replay: {message}");
    ExitCode::from(2)
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        fixture: None,
        version: false,
        paced: false,
        session: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--version" => args.version = true,
            "--fixture" => args.fixture = Some(it.next().ok_or("--fixture needs a file")?),
            "--pace" => match it.next().as_deref() {
                Some("recorded") => args.paced = true,
                Some("none") => args.paced = false,
                other => return Err(format!("--pace takes recorded or none, not {other:?}")),
            },
            "--session-id" | "--resume" => args.session = it.next(),
            _ => {}
        }
    }
    Ok(args)
}

fn load(path: &str) -> Result<Fixture, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let mut lines = text.lines();
    let header: Value = serde_json::from_str(lines.next().unwrap_or(""))
        .map_err(|e| format!("{path}: the first line is not JSON: {e}"))?;
    let h = &header["pf_fixture"];
    let cli_version = h["cli_version"]
        .as_str()
        .ok_or_else(|| format!("{path}: the header has no cli_version"))?
        .to_string();
    let expected = h["body_sha256"]
        .as_str()
        .ok_or_else(|| format!("{path}: the header has no body_sha256"))?;
    let body_lines: Vec<&str> = lines.filter(|l| !l.is_empty()).collect();
    let actual = sha256_hex(body_lines.join("\n").as_bytes());
    if actual != expected {
        return Err(format!(
            "{path}: the body changed after recording (body_sha256 is {actual}, the header says {expected})"
        ));
    }
    let mut body = Vec::new();
    for (i, l) in body_lines.iter().enumerate() {
        let v: Value = serde_json::from_str(l)
            .map_err(|e| format!("{path}: body line {} is not JSON: {e}", i + 1))?;
        let to = v["to"].as_str().unwrap_or("").to_string();
        if to != "cli" && to != "host" {
            return Err(format!(
                "{path}: body line {} has no `to` of cli or host",
                i + 1
            ));
        }
        body.push(Line {
            to,
            t_ms: v["t_ms"].as_u64().unwrap_or(0),
            msg: v["msg"].clone(),
        });
    }
    Ok(Fixture { cli_version, body })
}

/// The `type` of a message and, for a control message, its `subtype`.
fn kind(msg: &Value) -> (String, String) {
    let t = msg["type"].as_str().unwrap_or("").to_string();
    let sub = match t.as_str() {
        "control_request" => msg["request"]["subtype"].as_str(),
        "control_response" => msg["response"]["subtype"].as_str(),
        _ => None,
    };
    (t, sub.unwrap_or("").to_string())
}

/// Put `map`'s values in place of every string that equals one of its keys.
fn replace(v: &mut Value, map: &BTreeMap<String, String>) {
    match v {
        Value::String(s) => {
            if let Some(new) = map.get(s.as_str()) {
                *s = new.clone();
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| replace(x, map)),
        Value::Object(o) => o.values_mut().for_each(|x| replace(x, map)),
        _ => {}
    }
}

/// The session id the recording names: the first one a host line carries at its top level.
fn recorded_session(body: &[Line]) -> Option<String> {
    body.iter()
        .filter(|l| l.to == "host")
        .find_map(|l| l.msg["session_id"].as_str().map(str::to_string))
}

/// Read stdin until a line of kind `want` comes. `None` when stdin ends.
fn wait_for(
    input: &mut impl Iterator<Item = std::io::Result<String>>,
    want: &(String, String),
) -> Option<Value> {
    loop {
        let line = input.next()?.ok()?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(&line) {
            Err(_) => eprintln!("pf-claude-replay: skipped a stdin line that is not JSON"),
            Ok(v) if &kind(&v) == want => return Some(v),
            Ok(v) => {
                let (t, s) = kind(&v);
                eprintln!(
                    "pf-claude-replay: skipped an unexpected stdin line ({t} {s}); waiting for {} {}",
                    want.0, want.1
                );
            }
        }
    }
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => return fail(&e),
    };
    let Some(path) = args.fixture.as_deref() else {
        return fail("--fixture <file> is required");
    };
    let fixture = match load(path) {
        Ok(f) => f,
        Err(e) => return fail(&e),
    };
    if args.version {
        println!("{} (Claude Code)", fixture.cli_version);
        return ExitCode::SUCCESS;
    }

    let mut map: BTreeMap<String, String> = BTreeMap::new();
    if let (Some(recorded), Some(own)) = (recorded_session(&fixture.body), &args.session) {
        map.insert(recorded, own.clone());
    }
    let stdin = std::io::stdin();
    let mut input = stdin.lock().lines();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut initialized = false;
    let mut previous_ms = 0u64;

    for line in &fixture.body {
        if line.to == "cli" {
            previous_ms = line.t_ms;
            let want = kind(&line.msg);
            if want == ("control_request".to_string(), "initialize".to_string()) {
                if initialized {
                    continue;
                }
                initialized = true;
            }
            let Some(actual) = wait_for(&mut input, &want) else {
                return ExitCode::SUCCESS;
            };
            let key = match want.0.as_str() {
                "control_request" => Some("request_id"),
                "user" => Some("uuid"),
                _ => None,
            };
            if let Some(key) = key
                && let (Some(recorded), Some(own)) = (line.msg[key].as_str(), actual[key].as_str())
            {
                map.insert(recorded.to_string(), own.to_string());
            }
            continue;
        }
        if args.paced {
            std::thread::sleep(Duration::from_millis(line.t_ms.saturating_sub(previous_ms)));
        }
        previous_ms = line.t_ms;
        let mut msg = line.msg.clone();
        replace(&mut msg, &map);
        let text = serde_json::to_string(&msg).unwrap_or_default();
        if writeln!(out, "{text}").and_then(|()| out.flush()).is_err() {
            return ExitCode::SUCCESS;
        }
    }
    // The recording is played out. Take what the host still writes, until it closes stdin.
    for line in input {
        match line {
            Ok(l) if !l.trim().is_empty() => {
                eprintln!("pf-claude-replay: skipped a stdin line after the recording ended");
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    ExitCode::SUCCESS
}

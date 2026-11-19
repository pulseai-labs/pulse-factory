//! `hook`: the extension runs this for each tool call of a seat. Stdin holds `{role, tool, input}`.
//! The answer is `{"decision":"allow"|"deny","reason":…}` and the exit code is 0. Every failure is
//! a deny: no key, an unknown key, no controller, unreadable input.
//!
//! Only the key, the tool and the command or path go to the controller, so a large file's
//! content never reaches the 1 MiB limit of the socket.

use std::io::Read;
use std::time::{Duration, Instant};

use pf_ipc::ClientError;
use serde_json::{Value, json};

/// The most stdin the hook reads. A write call carries a whole file.
const STDIN_MAX: u64 = 64 << 20;

/// How long the hook waits for a controller to answer when none does, if `PF_HOOK_WAIT_SECONDS` is
/// absent or out of range (`session.hook_wait_seconds`, Q8).
pub const DEFAULT_WAIT_SECONDS: u64 = 15;

/// The gap between two attempts to reach the controller.
const RETRY_GAP: Duration = Duration::from_millis(200);

/// The seconds the hook waits for a controller: the value of `PF_HOOK_WAIT_SECONDS` when it is a
/// whole number from 1 to 120, else 15. The seat's launch record carries the resolved
/// `session.hook_wait_seconds` in that variable.
pub fn wait_seconds(value: Option<&str>) -> u64 {
    value
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|n| (1..=120).contains(n))
        .unwrap_or(DEFAULT_WAIT_SECONDS)
}

/// Call the controller. When none answers the socket, try again every 200 ms until `wait` has
/// passed since the first attempt: a controller that restarts takes a moment to bind its socket
/// (tier 1 runs first). An answer, a deny included, is final, and so is any failure after the
/// controller was reached.
fn call_waiting(req: &Value, wait: Duration) -> Result<pf_ipc::Response, ClientError> {
    let first = Instant::now();
    loop {
        match pf_ipc::call(&pf_ipc::state_dir(), "hook", req.clone()) {
            Err(ClientError::NoController { .. }) if first.elapsed() < wait => {
                std::thread::sleep(RETRY_GAP.min(wait.saturating_sub(first.elapsed())));
            }
            other => return other,
        }
    }
}

fn deny(reason: &str) -> Value {
    json!({ "decision": "deny", "reason": reason })
}

/// Turn the extension's call into the controller's request. `Err` is a deny reason.
pub fn request_of(call: &Value, key: &str) -> Result<Value, String> {
    if key.is_empty() {
        return Err("no session key in the environment (PF_SESSION_KEY)".to_string());
    }
    let tool = call
        .get("tool")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .ok_or("the call names no tool")?
        .to_string();
    let input = call.get("input").cloned().unwrap_or(Value::Null);
    let lower = tool.to_ascii_lowercase();
    let mut req = json!({ "key": key, "tool": tool });
    if lower == "bash" {
        let command = input
            .get("command")
            .and_then(Value::as_str)
            .ok_or("a bash call carries no command")?;
        req["command"] = json!(command);
    } else if pf_rules::is_write_tool(&lower) {
        let path = ["path", "file_path"]
            .iter()
            .find_map(|k| input.get(*k).and_then(Value::as_str))
            .filter(|p| !p.is_empty())
            .ok_or("a write tool with no path is denied, because the rules cannot judge it")?;
        req["path"] = json!(path);
    }
    Ok(req)
}

fn answer() -> Value {
    let mut bytes = Vec::new();
    if std::io::stdin()
        .take(STDIN_MAX + 1)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return deny("the call could not be read");
    }
    if bytes.len() as u64 > STDIN_MAX {
        return deny("the call is too large to judge");
    }
    let Ok(call) = serde_json::from_slice::<Value>(&bytes) else {
        return deny("the call is not JSON");
    };
    let key = std::env::var("PF_SESSION_KEY").unwrap_or_default();
    let req = match request_of(&call, &key) {
        Ok(r) => r,
        Err(why) => return deny(&why),
    };
    let wait = Duration::from_secs(wait_seconds(
        std::env::var("PF_HOOK_WAIT_SECONDS").ok().as_deref(),
    ));
    match call_waiting(&req, wait) {
        Ok(resp) if resp.ok => {
            let decision = resp.result["decision"].as_str().unwrap_or("deny");
            if decision == "allow" {
                json!({ "decision": "allow", "reason": resp.result["reason"] })
            } else {
                json!({ "decision": "deny", "reason": resp.result["reason"] })
            }
        }
        Ok(resp) => deny(&format!(
            "the controller refused the call: {}",
            resp.error.unwrap_or_default()
        )),
        Err(e) => deny(&format!("no controller answers: {e}")),
    }
}

pub fn run() -> i32 {
    println!("{}", answer());
    0
}

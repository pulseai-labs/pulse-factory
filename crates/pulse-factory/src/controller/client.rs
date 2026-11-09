//! The client side of the verbs: one call to the controller, then the print.
//!
//! `claim`, `answer`, `clear` and `ack` refuse here when `PF_SEAT_TOKEN` is set, and the controller
//! checks the caller again (ADR-0005). The claim token lives in a file of mode 0600 and is never
//! printed.

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::cli::{
    AckArgs, AnswerArgs, ApiStatusArgs, ChatRuntimeInstallArgs, ClaimArgs, ClearArgs,
    DeviceListArgs, DevicePairArgs, DeviceRevokeArgs, HaltArgs, ProjectAddArgs, ProjectListArgs,
    ProjectRemoveArgs, RequestsArgs, SessionAnswerArgs, SessionEndArgs, SessionInterruptArgs,
    SessionItemsArgs, SessionListArgs, SessionRedactArgs, SessionSendArgs, SessionSetArgs,
    SessionStartArgs, ShowArgs, StatusArgs,
};

/// The file name of the claim token, in the state directory.
pub const CLAIM_TOKEN_FILE: &str = "claim-token";

fn call(verb: &str, args: Value) -> Result<Value, String> {
    let state = pf_ipc::state_dir();
    match pf_ipc::call(&state, verb, args) {
        Ok(resp) if resp.ok => Ok(resp.result),
        Ok(resp) => Err(resp
            .error
            .unwrap_or_else(|| "the controller refused".to_string())),
        Err(e) => Err(e.to_string()),
    }
}

fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

pub fn status(a: &StatusArgs) -> i32 {
    match call("status", json!({ "run": a.run })) {
        Ok(v) => {
            if a.json {
                print_json(&v);
            } else {
                print!("{}", status_text(&v));
            }
            0
        }
        Err(why) => {
            eprintln!("pulse-factory: status: {why}");
            1
        }
    }
}

pub fn status_text(v: &Value) -> String {
    let s = |p: &Value| p.as_str().unwrap_or("").to_string();
    let mut t = String::new();
    let mut line = |x: String| {
        t.push_str(&x);
        t.push('\n');
    };
    line(format!("run: {}", s(&v["run"])));
    line(format!("env: {}", s(&v["env"])));
    line(format!("state: {}", s(&v["state"])));
    line(format!(
        "open requests: {}",
        v["open_requests"].as_u64().unwrap_or(0)
    ));
    if let Some(r) = v["end_reason"].as_str() {
        line(format!("ended: {r}"));
    }
    if let Some(r) = v["halted_reason"].as_str() {
        line(format!("halted: {r}"));
    }
    if let Some(r) = v["degraded"].as_str() {
        line(format!("degraded: {r}"));
    }
    for n in v["notes"].as_array().into_iter().flatten() {
        line(format!("note: {}", s(n)));
    }
    for u in v["units"].as_array().into_iter().flatten() {
        let reason = u["reason"]
            .as_str()
            .map(|r| format!(" ({r})"))
            .unwrap_or_default();
        let request = u["request"]
            .as_str()
            .map(|r| format!(" [request {r}]"))
            .unwrap_or_default();
        line(format!(
            "unit {}: stage {}, {}{}{}",
            s(&u["id"]),
            s(&u["stage"]),
            s(&u["status"]),
            reason,
            request
        ));
    }
    for x in v["sessions"].as_array().into_iter().flatten() {
        let tk = &x["tokens"];
        line(format!(
            "session {}: unit {}, stage {}, role {}, model {}, runtime {}, {}, tokens in {} out {} cache-read {} cache-write {}, cost {}",
            s(&x["id"]),
            s(&x["unit"]),
            s(&x["stage"]),
            s(&x["role"]),
            s(&x["model"]),
            s(&x["runtime"]),
            s(&x["status"]),
            tk["input"],
            tk["output"],
            tk["cache_read"],
            tk["cache_write"],
            s(&x["cost"]),
        ));
    }
    line(format!("total cost: {}", s(&v["total"]["cost"])));
    t
}

pub fn halt(a: &HaltArgs) -> i32 {
    if std::env::var_os("PF_SEAT_TOKEN").is_some() {
        eprintln!("pulse-factory: halt refused: PF_SEAT_TOKEN is set, so this is a seat");
        return 1;
    }
    match call("halt", json!({ "run": a.run, "reason": a.reason })) {
        Ok(v) => {
            if a.json {
                print_json(&v);
            } else if v["run"].is_null() {
                println!("there is no run to halt");
            } else if v["already_halted"].as_bool() == Some(true) {
                println!(
                    "run {} was halted already; the first reason stays: {}",
                    v["run"].as_str().unwrap_or(""),
                    v["reason"].as_str().unwrap_or("")
                );
            } else {
                println!("run {} halted", v["run"].as_str().unwrap_or(""));
            }
            if !a.json {
                let stopped: Vec<String> = v["chats_stopped"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(s_of)
                    .collect();
                if !stopped.is_empty() {
                    println!("stopped the chat processes of {}", stopped.join(", "));
                }
            }
            0
        }
        Err(why) => {
            eprintln!("pulse-factory: halt: {why}");
            1
        }
    }
}

// ------------------------------------------------------------------ requests and the claim

fn refuse_seat(verb: &str) -> Option<i32> {
    if std::env::var_os("PF_SEAT_TOKEN").is_some() {
        eprintln!("pulse-factory: {verb} refused: PF_SEAT_TOKEN is set, so this is a seat");
        return Some(1);
    }
    None
}

pub fn requests(a: &RequestsArgs) -> i32 {
    match call("requests", json!({ "run": a.run, "all": a.all })) {
        Ok(v) => {
            if a.json {
                print_json(&v);
            } else {
                print!("{}", requests_text(&v));
            }
            0
        }
        Err(why) => {
            eprintln!("pulse-factory: requests: {why}");
            1
        }
    }
}

pub fn requests_text(v: &Value) -> String {
    let s = |p: &Value| p.as_str().unwrap_or("").to_string();
    let list = v["requests"].as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        return "no open requests\n".to_string();
    }
    let mut t = String::new();
    for r in &list {
        let unit = r["unit"]
            .as_str()
            .map_or("(the run)".to_string(), |u| format!("unit {u}"));
        let blocks: Vec<String> = r["blocks"]
            .as_array()
            .into_iter()
            .flatten()
            .map(&s)
            .collect();
        let blocks = if blocks.is_empty() {
            String::new()
        } else {
            format!(", blocks {}", blocks.join(" "))
        };
        let status = match r["status"].as_str() {
            Some("open") | None => String::new(),
            Some(other) => format!(" [{other}]"),
        };
        t.push_str(&format!(
            "{} rev {} {}{}: run {}, {}, stage {}, opened {}{}: {}\n",
            s(&r["id"]),
            r["revision"],
            s(&r["kind"]),
            status,
            s(&r["run"]),
            unit,
            s(&r["stage"]),
            s(&r["opened_at"]),
            blocks,
            s(&r["reason"]),
        ));
    }
    t
}

pub fn show(a: &ShowArgs) -> i32 {
    match call("show", json!({ "id": a.id })) {
        Ok(v) => {
            if a.json {
                print_json(&v);
            } else {
                print!("{}", show_text(&v));
            }
            0
        }
        Err(why) => {
            eprintln!("pulse-factory: show: {why}");
            1
        }
    }
}

pub fn show_text(v: &Value) -> String {
    let s = |p: &Value| p.as_str().unwrap_or("").to_string();
    let mut t = String::new();
    let mut line = |x: String| {
        t.push_str(&x);
        t.push('\n');
    };
    line(format!(
        "request: {} (revision {})",
        s(&v["id"]),
        v["revision"]
    ));
    line(format!("kind: {}", s(&v["kind"])));
    line(format!("status: {}", s(&v["status"])));
    line(format!("run: {}", s(&v["run"])));
    if let Some(u) = v["unit"].as_str() {
        line(format!("unit: {u}"));
    }
    line(format!("stage: {}", s(&v["stage"])));
    line(format!("opened: {}", s(&v["opened_at"])));
    line(format!("reason: {}", s(&v["reason"])));
    let blocks: Vec<String> = v["blocks"]
        .as_array()
        .into_iter()
        .flatten()
        .map(&s)
        .collect();
    if !blocks.is_empty() {
        line(format!("blocks: {}", blocks.join(" ")));
    }
    line(format!("body: {}", v["body"]));
    // The facts a seat's file added. They are seat-written text: every control character is
    // replaced before it reaches the terminal, so the text is shown and never acted on.
    if let Some(gaps) = v["body"]["gaps"].as_array() {
        for (i, g) in gaps.iter().enumerate() {
            line(format!(
                "gap {}: [{}] {}: {}",
                i + 1,
                tame(&s(&g["severity"])),
                tame(&s(&g["section"])),
                tame(&s(&g["question"]))
            ));
        }
    }
    if v["kind"].as_str() == Some("merge_ack") {
        // The facts the operator acks. They name the PR, where it merges and the exact head.
        for (label, key) in [
            ("pr", "pr"),
            ("base", "base"),
            ("head branch", "head_branch"),
            ("head sha", "head_sha"),
        ] {
            if let Some(value) = v["body"][key].as_str() {
                line(format!("{label}: {}", tame(value)));
            }
        }
    }
    if let Some(why) = v["body"]["gaps_error"].as_str() {
        line(format!("gaps_error: {}", tame(why)));
    }
    if let Some(text) = v["body"]["stop_text"].as_str() {
        line("stop text:".to_string());
        for l in text.lines() {
            line(format!("  | {}", tame(l)));
        }
    }
    if !v["answer"].is_null() {
        line(format!(
            "answer: {} (claim {}, {})",
            v["answer"],
            v["answer_generation"],
            s(&v["answered_at"])
        ));
    }
    let verbs: Vec<String> = v["verbs"]
        .as_array()
        .into_iter()
        .flatten()
        .map(&s)
        .collect();
    if verbs == ["ack"] {
        line("answered by: ack".to_string());
        line(format!(
            "  pulse-factory ack {} --rev {}",
            s(&v["id"]),
            v["revision"]
        ));
    } else if !verbs.is_empty() {
        line(format!("answered by: {}", verbs.join(", ")));
        line(format!(
            "  pulse-factory answer {} --rev {} --text \"<your ruling>\"",
            s(&v["id"]),
            v["revision"]
        ));
        if let Some(unit) = v["unit"].as_str() {
            line(format!(
                "  pulse-factory clear {} {} [--to <stage>]",
                s(&v["run"]),
                unit
            ));
        }
    }
    t
}

/// A seat's text for the terminal: line breaks become ` / `, and any other control character
/// (an escape sequence starts with one) becomes `?`.
fn tame(text: &str) -> String {
    let flat = text.replace("\r\n", " / ").replace(['\n', '\r'], " / ");
    flat.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// The default path of the claim token: `<state dir>/claim-token`.
pub fn default_token_path() -> PathBuf {
    pf_ipc::state_dir().join(CLAIM_TOKEN_FILE)
}

/// Write the token to `path` with mode 0600, replacing an older file. The token is made in a
/// temporary file of the same mode first, so it is never readable by others, not even for a moment.
fn write_token(path: &Path, token: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(token.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(&tmp, path)
}

fn read_token(path: &Option<PathBuf>) -> Result<String, String> {
    let path = path.clone().unwrap_or_else(default_token_path);
    match std::fs::read_to_string(&path) {
        Ok(text) if !text.trim().is_empty() => Ok(text.trim().to_string()),
        Ok(_) => Err(format!(
            "the claim token file {} is empty: run `pulse-factory claim`",
            path.display()
        )),
        Err(e) => Err(format!(
            "cannot read the claim token file {} ({e}): run `pulse-factory claim`",
            path.display()
        )),
    }
}

fn default_holder() -> String {
    let user = std::env::var("USER").unwrap_or_else(|_| "user".to_string());
    let host = std::fs::read_to_string("/etc/hostname")
        .map(|h| h.trim().to_string())
        .ok()
        .filter(|h| !h.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "host".to_string());
    format!("{user}@{host}:{}", std::process::id())
}

pub fn claim(a: &ClaimArgs) -> i32 {
    if let Some(code) = refuse_seat("claim") {
        return code;
    }
    let holder = a.holder.clone().unwrap_or_else(default_holder);
    let reply = match call("claim", json!({ "holder": holder, "run": a.run })) {
        Ok(v) => v,
        Err(why) => {
            eprintln!("pulse-factory: {why}");
            return 1;
        }
    };
    let token = reply["token"].as_str().unwrap_or_default();
    let generation = reply["generation"].as_u64().unwrap_or(0);
    let path = default_token_path();
    if token.is_empty() || write_token(&path, token).is_err() {
        eprintln!(
            "pulse-factory: claim {generation} was made, but the token file {} cannot be written: claim again",
            path.display()
        );
        return 1;
    }
    if a.json {
        print_json(&json!({
            "schema_version": 1,
            "generation": generation,
            "holder": reply["holder"],
            "run": reply["run"],
            "token_file": path.display().to_string(),
        }));
    } else {
        println!(
            "claim {generation} held by {}; token file: {}",
            reply["holder"].as_str().unwrap_or(""),
            path.display()
        );
    }
    0
}

pub fn answer(a: &AnswerArgs) -> i32 {
    if let Some(code) = refuse_seat("answer") {
        return code;
    }
    let token = match read_token(&a.claim_token_file) {
        Ok(t) => t,
        Err(why) => {
            eprintln!("pulse-factory: answer refused: {why}");
            return 1;
        }
    };
    let args = json!({ "request": a.id, "rev": a.rev, "text": a.text, "token": token });
    match call("answer", args) {
        Ok(v) => {
            if a.json {
                print_json(&v);
            } else {
                let unit = match (v["unit"].as_str(), v["hook_failure"].as_str()) {
                    (Some(u), Some(why)) => format!(
                        "; the answer hook failed, so unit {u} is parked again with a new request: {}",
                        tame(why)
                    ),
                    (Some(u), None) => format!("; unit {u} resumes"),
                    (None, _) => String::new(),
                };
                println!(
                    "answered {}@{} by claim {}{unit}",
                    v["request"].as_str().unwrap_or(""),
                    v["revision"],
                    v["generation"]
                );
            }
            0
        }
        Err(why) => {
            eprintln!("pulse-factory: {why}");
            1
        }
    }
}

pub fn clear(a: &ClearArgs) -> i32 {
    if let Some(code) = refuse_seat("clear") {
        return code;
    }
    let token = match read_token(&a.claim_token_file) {
        Ok(t) => t,
        Err(why) => {
            eprintln!("pulse-factory: clear refused: {why}");
            return 1;
        }
    };
    let args = json!({ "run": a.run, "unit": a.item, "to": a.to, "token": token });
    match call("clear", args) {
        Ok(v) => {
            if a.json {
                print_json(&v);
            } else {
                println!(
                    "cleared {} of run {} ({}@{} by claim {}); it resumes at stage {}",
                    v["unit"].as_str().unwrap_or(""),
                    v["run"].as_str().unwrap_or(""),
                    v["request"].as_str().unwrap_or(""),
                    v["revision"],
                    v["generation"],
                    v["to_stage"].as_str().unwrap_or("(where it parked)")
                );
            }
            0
        }
        Err(why) => {
            eprintln!("pulse-factory: {why}");
            1
        }
    }
}

pub fn ack(a: &AckArgs) -> i32 {
    if let Some(code) = refuse_seat("ack") {
        return code;
    }
    let token = match read_token(&a.claim_token_file) {
        Ok(t) => t,
        Err(why) => {
            eprintln!("pulse-factory: ack refused: {why}");
            return 1;
        }
    };
    let args = json!({ "request": a.id, "rev": a.rev, "token": token });
    match call("ack", args) {
        Ok(v) => {
            if a.json {
                print_json(&v);
            } else {
                let merged = if v["merged"].as_bool() == Some(true) {
                    format!("merged {} at {}", s_of(&v["pr"]), s_of(&v["head_sha"]))
                } else {
                    format!(
                        "the merge did not land ({}); unit {} is parked with a new request",
                        tame(&s_of(&v["outcome"])),
                        s_of(&v["unit"])
                    )
                };
                println!(
                    "acked {}@{} by claim {}; {merged}",
                    s_of(&v["request"]),
                    v["revision"],
                    v["generation"]
                );
                if let Some(why) = v["after_merge_failure"].as_str() {
                    println!("the after-merge commands failed: {}", tame(why));
                }
            }
            0
        }
        Err(why) => {
            eprintln!("pulse-factory: {why}");
            1
        }
    }
}

// ------------------------------------------------------------------ projects

/// A command ID for one call: 128 random bits as 32 hex digits.
fn random_command_id() -> Result<String, String> {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    std::fs::File::open(super::seat::RANDOM_DEVICE)
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| format!("no command ID can be made ({e}); give one with --command-id"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Print a refusal or an error of a project verb and give the exit code 1.
fn project_failed(verb: &str, why: &str) -> i32 {
    eprintln!("pulse-factory: project {verb}: {}", tame(why));
    1
}

/// One line of the text forms: `<id>  <name>  <root>` is built from tamed parts.
fn project_parts(p: &Value) -> (String, String, String) {
    (
        tame(&s_of(&p["id"])),
        tame(&s_of(&p["name"])),
        tame(&s_of(&p["root"])),
    )
}

pub fn project_add(a: &ProjectAddArgs) -> i32 {
    if let Some(code) = refuse_seat("project add") {
        return code;
    }
    // The controller runs in another directory, so the CLI sends an absolute path.
    let root = if a.root.is_absolute() {
        a.root.clone()
    } else {
        match std::env::current_dir() {
            Ok(dir) => dir.join(&a.root),
            Err(e) => {
                return project_failed("add", &format!("cannot read the current directory: {e}"));
            }
        }
    };
    let Some(root) = root.to_str() else {
        return project_failed("add", "the root is not valid UTF-8");
    };
    let command_id = match a.command_id.clone().map_or_else(random_command_id, Ok) {
        Ok(id) => id,
        Err(why) => return project_failed("add", &why),
    };
    let args = json!({ "root": root, "name": a.name, "command_id": command_id });
    match call("project_add", args) {
        Ok(v) => {
            if a.json {
                print_json(&v);
            } else {
                let (id, name, root) = project_parts(&v["project"]);
                println!("project {id} added: {name} ({root})");
            }
            0
        }
        Err(why) => project_failed("add", &why),
    }
}

pub fn project_list(a: &ProjectListArgs) -> i32 {
    match call("project_list", json!({})) {
        Ok(v) => {
            if a.json {
                print_json(&v);
            } else {
                let list = v["projects"].as_array().cloned().unwrap_or_default();
                if list.is_empty() {
                    println!("no projects");
                }
                for p in &list {
                    let (id, name, root) = project_parts(p);
                    // A project that no detection could give a kind ends its line with the cause.
                    match (p["kind"].is_null(), p["kind_error"].as_str()) {
                        (true, Some(cause)) => println!("{id}  {name}  {root}  (no kind: {cause})"),
                        _ => println!("{id}  {name}  {root}"),
                    }
                }
            }
            0
        }
        Err(why) => project_failed("list", &why),
    }
}

pub fn project_remove(a: &ProjectRemoveArgs) -> i32 {
    if let Some(code) = refuse_seat("project remove") {
        return code;
    }
    let command_id = match a.command_id.clone().map_or_else(random_command_id, Ok) {
        Ok(id) => id,
        Err(why) => return project_failed("remove", &why),
    };
    match call(
        "project_remove",
        json!({ "id": a.id, "command_id": command_id }),
    ) {
        Ok(v) => {
            if a.json {
                print_json(&v);
            } else {
                let (id, name, root) = project_parts(&v["project"]);
                println!("project {id} removed: {name} ({root})");
            }
            0
        }
        Err(why) => project_failed("remove", &why),
    }
}

// ------------------------------------------------------------------ devices and the API

/// Print a refusal or an error of a device verb and give the exit code 1. The text is tamed: a
/// device chose its own name.
fn device_failed(verb: &str, why: &str) -> i32 {
    eprintln!("pulse-factory: device {verb}: {}", tame(why));
    1
}

/// `device pair`: print one code and when it stops working. The code is printed here and nowhere
/// else: the operator copies it from this output.
pub fn device_pair(a: &DevicePairArgs) -> i32 {
    if let Some(code) = refuse_seat("device pair") {
        return code;
    }
    match call("device_pair", json!({})) {
        Ok(v) => {
            if a.json {
                print_json(&v);
            } else {
                println!("{}", s_of(&v["code"]));
                println!("expires {} (one use)", s_of(&v["expires_at"]));
            }
            0
        }
        Err(why) => device_failed("pair", &why),
    }
}

pub fn device_list(a: &DeviceListArgs) -> i32 {
    match call("device_list", json!({})) {
        Ok(v) => {
            if a.json {
                print_json(&v);
            } else {
                let list = v["devices"].as_array().cloned().unwrap_or_default();
                if list.is_empty() {
                    println!("no devices");
                }
                for d in &list {
                    println!("{}", device_line(d));
                }
            }
            0
        }
        Err(why) => device_failed("list", &why),
    }
}

/// One line of the text form: `<id>  <name>  paired <t>  last seen <t> from <peer>`, and
/// `  revoked <t>` for a revoked device. Every part is tamed.
fn device_line(d: &Value) -> String {
    let last = match d["last_seen_at"].as_str() {
        Some(at) => format!(
            "last seen {} from {}",
            tame(at),
            tame(d["last_peer"].as_str().unwrap_or("?"))
        ),
        None => "never seen".to_string(),
    };
    let revoked = d["revoked_at"]
        .as_str()
        .map(|t| format!("  revoked {}", tame(t)))
        .unwrap_or_default();
    format!(
        "{}  {}  paired {}  {last}{revoked}",
        tame(&s_of(&d["id"])),
        tame(&s_of(&d["name"])),
        tame(&s_of(&d["paired_at"])),
    )
}

pub fn device_revoke(a: &DeviceRevokeArgs) -> i32 {
    if let Some(code) = refuse_seat("device revoke") {
        return code;
    }
    let command_id = match a.command_id.clone().map_or_else(random_command_id, Ok) {
        Ok(id) => id,
        Err(why) => return device_failed("revoke", &why),
    };
    match call(
        "device_revoke",
        json!({ "id": a.id, "command_id": command_id }),
    ) {
        Ok(v) => {
            if a.json {
                print_json(&v);
            } else {
                println!(
                    "device {} revoked: {}",
                    tame(&s_of(&v["device"]["id"])),
                    tame(&s_of(&v["device"]["name"]))
                );
            }
            0
        }
        Err(why) => device_failed("revoke", &why),
    }
}

/// `api status`: `off`, `waiting for <addr>`, `listening on <addr>` or `failed: <why>`, and on a
/// second line the SPKI hash of the key when the key exists.
pub fn api_status(a: &ApiStatusArgs) -> i32 {
    match call("api_status", json!({})) {
        Ok(v) => {
            if a.json {
                print_json(&v);
            } else {
                print!("{}", api_status_text(&v));
            }
            0
        }
        Err(why) => {
            eprintln!("pulse-factory: api status: {}", tame(&why));
            1
        }
    }
}

pub fn api_status_text(v: &Value) -> String {
    let listen = tame(&s_of(&v["listen"]));
    let mut t = match v["state"].as_str() {
        Some("listening") => format!("listening on {listen}\n"),
        Some("waiting") => format!("waiting for {listen}\n"),
        Some("failed") => format!("failed: {}\n", tame(&s_of(&v["error"]))),
        _ => "off\n".to_string(),
    };
    if let Some(hash) = v["key_spki_sha256"].as_str() {
        t.push_str(&format!("key spki sha256: {}\n", tame(hash)));
    }
    t
}

fn s_of(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

// ------------------------------------------------------------------ chat sessions

fn session_failed(verb: &str, why: &str) -> i32 {
    eprintln!("pulse-factory: session {verb}: {}", tame(why));
    1
}

/// The command ID of one call: the one given, or a random one.
fn command_id_of(given: &Option<String>) -> Result<String, String> {
    given.clone().map_or_else(random_command_id, Ok)
}

/// Print the answer of a verb that answers a `SessionChanged`: JSON, or `line`, and the note.
fn finish(
    verb: &str,
    json: bool,
    reply: Result<Value, String>,
    line: impl FnOnce(&Value) -> String,
) -> i32 {
    match reply {
        Ok(v) => {
            if json {
                print_json(&v);
            } else {
                println!("{}", line(&v));
                if let Some(note) = v["note"].as_str() {
                    eprintln!("note: {}", tame(note));
                }
            }
            0
        }
        Err(why) => session_failed(verb, &why),
    }
}

pub fn session_start(a: &SessionStartArgs) -> i32 {
    if let Some(code) = refuse_seat("session start") {
        return code;
    }
    let command_id = match command_id_of(&a.command_id) {
        Ok(id) => id,
        Err(why) => return session_failed("start", &why),
    };
    let reply = call(
        "session_start",
        json!({ "project_id": a.project, "agent_id": a.agent, "command_id": command_id }),
    );
    finish("start", a.json, reply, |v| {
        format!(
            "session {} started in {} (worktree {})",
            tame(&s_of(&v["session"]["id"])),
            tame(&s_of(&v["worktree"]["path"])),
            tame(&s_of(&v["worktree"]["id"]))
        )
    })
}

pub fn session_send(a: &SessionSendArgs) -> i32 {
    if let Some(code) = refuse_seat("session send") {
        return code;
    }
    let command_id = match command_id_of(&a.command_id) {
        Ok(id) => id,
        Err(why) => return session_failed("send", &why),
    };
    let reply = call(
        "session_send",
        json!({ "session_id": a.id, "text": a.text, "command_id": command_id }),
    );
    finish("send", a.json, reply, |v| {
        format!("sent to {}", tame(&s_of(&v["session"]["id"])))
    })
}

/// Split an `--answer` pair at its last `=` that has no backslash before it. `\=` is a literal `=`
/// and `\\` a literal backslash in either part; a backslash before any other character stays as
/// it is. A pair with no separator is `None`. Text with no backslash splits at its last `=`.
fn split_answer(pair: &str) -> Option<(String, String)> {
    let mut parts = vec![String::new()];
    let mut chars = pair.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if matches!(chars.peek(), Some('=' | '\\')) => {
                if let (Some(next), Some(part)) = (chars.next(), parts.last_mut()) {
                    part.push(next);
                }
            }
            '=' => parts.push(String::new()),
            other => {
                if let Some(part) = parts.last_mut() {
                    part.push(other);
                }
            }
        }
    }
    let label = parts.pop()?;
    if parts.is_empty() {
        return None;
    }
    Some((parts.join("="), label))
}

pub fn session_answer(a: &SessionAnswerArgs) -> i32 {
    if let Some(code) = refuse_seat("session answer") {
        return code;
    }
    let answer = if a.allow {
        json!({ "type": "allow" })
    } else if let Some(message) = &a.deny {
        json!({ "type": "deny", "value": { "message": message } })
    } else if !a.answer.is_empty() {
        let mut map = serde_json::Map::new();
        for pair in &a.answer {
            match split_answer(pair) {
                Some((question, label)) if !question.is_empty() => {
                    map.insert(question, Value::String(label));
                }
                _ => {
                    return session_failed(
                        "answer",
                        &format!("--answer {pair:?} is not <question>=<label>"),
                    );
                }
            }
        }
        json!({ "type": "answers", "value": map })
    } else {
        return session_failed(
            "answer",
            "give --allow, --deny <message> or --answer <question>=<label>",
        );
    };
    let command_id = match command_id_of(&a.command_id) {
        Ok(id) => id,
        Err(why) => return session_failed("answer", &why),
    };
    let reply = call(
        "session_answer",
        json!({ "session_id": a.id, "seq": a.seq, "answer": answer, "command_id": command_id }),
    );
    finish("answer", a.json, reply, |v| {
        format!(
            "answered request {} of {}",
            a.seq,
            tame(&s_of(&v["session"]["id"]))
        )
    })
}

pub fn session_interrupt(a: &SessionInterruptArgs) -> i32 {
    if let Some(code) = refuse_seat("session interrupt") {
        return code;
    }
    let command_id = match command_id_of(&a.command_id) {
        Ok(id) => id,
        Err(why) => return session_failed("interrupt", &why),
    };
    let reply = call(
        "session_interrupt",
        json!({ "session_id": a.id, "command_id": command_id }),
    );
    finish("interrupt", a.json, reply, |v| {
        format!("interrupted {}", tame(&s_of(&v["session"]["id"])))
    })
}

pub fn session_set(a: &SessionSetArgs) -> i32 {
    if let Some(code) = refuse_seat("session set") {
        return code;
    }
    let mut options = serde_json::Map::new();
    for pair in &a.option {
        match pair.split_once('=') {
            Some((id, value)) if !id.is_empty() => {
                options.insert(id.to_string(), Value::String(value.to_string()));
            }
            _ => {
                return session_failed("set", &format!("--option {pair:?} is not <id>=<value>"));
            }
        }
    }
    let command_id = match command_id_of(&a.command_id) {
        Ok(id) => id,
        Err(why) => return session_failed("set", &why),
    };
    let reply = call(
        "session_set",
        json!({
            "session_id": a.id,
            "model": a.model,
            "effort": a.effort,
            "permission_mode": a.mode,
            "options": if options.is_empty() { Value::Null } else { Value::Object(options) },
            "command_id": command_id,
        }),
    );
    finish("set", a.json, reply, |v| {
        let s = &v["session"];
        let pending = s["pending_options"]
            .as_object()
            .filter(|m| !m.is_empty())
            .map(|m| {
                let pairs: Vec<String> = m
                    .iter()
                    .map(|(k, v)| format!("{}={}", tame(k), tame(&s_of(v))))
                    .collect();
                format!("; applies at the next process: {}", pairs.join(", "))
            })
            .unwrap_or_default();
        format!(
            "{} is set to {}/{}/{}{pending}",
            tame(&s_of(&s["id"])),
            tame(&s_of(&s["model"])),
            tame(&s_of(&s["effort"])),
            tame(&s_of(&s["permission_mode"]))
        )
    })
}

pub fn session_end(a: &SessionEndArgs) -> i32 {
    if let Some(code) = refuse_seat("session end") {
        return code;
    }
    let command_id = match command_id_of(&a.command_id) {
        Ok(id) => id,
        Err(why) => return session_failed("end", &why),
    };
    let reply = call(
        "session_end",
        json!({ "session_id": a.id, "command_id": command_id }),
    );
    finish("end", a.json, reply, |v| {
        format!("session {} ended", tame(&s_of(&v["session"]["id"])))
    })
}

pub fn session_redact(a: &SessionRedactArgs) -> i32 {
    if let Some(code) = refuse_seat("session redact") {
        return code;
    }
    let command_id = match command_id_of(&a.command_id) {
        Ok(id) => id,
        Err(why) => return session_failed("redact", &why),
    };
    let reply = call(
        "session_redact",
        json!({ "session_id": a.id, "seq": a.seq, "command_id": command_id }),
    );
    finish("redact", a.json, reply, |v| {
        format!(
            "redacted item {} of {}",
            v["item"]["seq"],
            tame(&s_of(&v["item"]["session_id"]))
        )
    })
}

/// A cost in millionths of a dollar as `$0.003465`.
pub fn cost_text(micros: u64) -> String {
    format!("${}.{:06}", micros / 1_000_000, micros % 1_000_000)
}

/// One line of `session list`.
pub fn session_line(s: &Value) -> String {
    let worktree = match (s["branch"].as_str(), s["detached"].as_bool()) {
        (Some(b), _) => format!("{} {}", tame(&s_of(&s["worktree_id"])), tame(b)),
        (None, Some(true)) => format!("{} detached", tame(&s_of(&s["worktree_id"]))),
        _ => tame(&s_of(&s["worktree_id"])),
    };
    let process = if s["process"].is_null() {
        "no process".to_string()
    } else {
        let p = &s["process"];
        let mut text = format!("process since {}", tame(&s_of(&p["since"])));
        match p["releases_at"].as_str() {
            Some(t) => text.push_str(&format!(", releases at {}", tame(t))),
            None => {
                if let Some(t) = p["waiting_since"].as_str() {
                    text.push_str(&format!(", waiting for an answer since {}", tame(t)));
                }
                for task in p["tasks"].as_array().into_iter().flatten() {
                    text.push_str(&format!(
                        ", background task {} since {}",
                        tame(&s_of(&task["id"])),
                        tame(&s_of(&task["since"]))
                    ));
                }
                if let Some(t) = p["turn_since"].as_str() {
                    text.push_str(&format!(", turn since {}", tame(t)));
                }
            }
        }
        if p["qualified"].as_bool() == Some(false) {
            text.push_str(&format!(
                ", unqualified {}",
                tame(&s_of(&p["runtime_version"]))
            ));
        }
        text
    };
    let mut line = format!(
        "{}  {} {}  {}  {}  {}/{}/{}  {}  {process}",
        tame(&s_of(&s["id"])),
        tame(&s_of(&s["project_id"])),
        tame(&s_of(&s["project_name"])),
        worktree,
        tame(&s_of(&s["state"])),
        tame(&s_of(&s["model"])),
        tame(&s_of(&s["effort"])),
        tame(&s_of(&s["permission_mode"])),
        cost_text(s["usage"]["cost_micros"].as_u64().unwrap_or(0)),
    );
    if let Some(why) = s["context_lost"].as_str() {
        line.push_str(&format!(", context lost: {}", tame(why)));
    }
    if let Some(t) = s["ended_at"].as_str() {
        line.push_str(&format!(", ended {}", tame(t)));
    }
    line
}

pub fn session_list(a: &SessionListArgs) -> i32 {
    let reply = call(
        "session_list",
        json!({ "project_id": a.project, "all": a.all }),
    );
    match reply {
        Ok(v) => {
            if a.json {
                print_json(&v);
            } else {
                let list = v["sessions"].as_array().cloned().unwrap_or_default();
                if list.is_empty() {
                    println!("no chat sessions");
                }
                for s in &list {
                    println!("{}", session_line(s));
                }
            }
            0
        }
        Err(why) => session_failed("list", &why),
    }
}

/// One line of `session items`: the `seq`, the kind and a one-line summary.
pub fn item_line(item: &Value) -> String {
    let body = &item["body"];
    let kind = s_of(&body["kind"]);
    let cut = |t: &str| -> String {
        let t = tame(t);
        if t.chars().count() > 100 {
            let head: String = t.chars().take(100).collect();
            format!("{head}...")
        } else {
            t
        }
    };
    let summary = match kind.as_str() {
        "operator_message" | "assistant_text" => cut(&s_of(&body["text"])),
        "tool_call" => format!(
            "{}{}",
            tame(&s_of(&body["tool"])),
            if body["is_error"].as_bool() == Some(true) {
                " (error)"
            } else {
                ""
            }
        ),
        "approval_request" => format!(
            "{} {}",
            tame(&s_of(&body["tool"])),
            cut(&s_of(&body["reason"]))
        ),
        "question_request" => body["items"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|q| cut(&s_of(&q["text"])))
            .collect::<Vec<_>>()
            .join(" | "),
        "approval_answer" | "question_answer" => format!(
            "{} {}answers {}",
            tame(&s_of(&body["outcome"])),
            body["decision"]
                .as_str()
                .map(|d| format!("{d} "))
                .unwrap_or_default(),
            item["answers_seq"]
        ),
        "background_task" => format!(
            "{} {}",
            tame(&s_of(&body["event"])),
            tame(&s_of(&body["task_id"]))
        ),
        "notice" => cut(&s_of(&body["text"])),
        "turn_result" => format!(
            "{} cost {}",
            tame(&s_of(&body["stop"])),
            cost_text(body["usage"]["cost_micros"].as_u64().unwrap_or(0))
        ),
        "redacted" => "(redacted)".to_string(),
        _ => "(unknown kind)".to_string(),
    };
    let kind = if kind == "redacted" {
        tame(&s_of(&body["item_kind"]))
    } else {
        kind
    };
    format!("{}  {}  {}", item["seq"], tame(&kind), summary)
}

pub fn session_items(a: &SessionItemsArgs) -> i32 {
    let reply = call(
        "session_items",
        json!({ "session_id": a.id, "after": a.after }),
    );
    match reply {
        Ok(v) => {
            if a.json {
                print_json(&v);
            } else {
                let list = v["items"].as_array().cloned().unwrap_or_default();
                if list.is_empty() {
                    println!("no items");
                }
                for item in &list {
                    println!("{}", item_line(item));
                }
            }
            0
        }
        Err(why) => session_failed("items", &why),
    }
}

/// `chat-runtime install`: fetch the managed Claude Code copy, check it against the pin and place
/// it. It runs in this process, with no controller.
pub fn chat_runtime_install(a: &ChatRuntimeInstallArgs) -> i32 {
    match super::chat_install::install_from_env() {
        Ok(path) => {
            let version = pf_adapter_claude::pinned::CLI_VERSION;
            if a.json {
                print_json(&json!({
                    "schema_version": 1,
                    "installed": true,
                    "version": version,
                    "path": path.display().to_string(),
                }));
            } else {
                println!(
                    "the chat runtime Claude Code {version} is installed at {}",
                    path.display()
                );
            }
            0
        }
        Err(why) => {
            if a.json {
                print_json(&json!({ "schema_version": 1, "installed": false, "error": why }));
            } else {
                eprintln!("pulse-factory: chat-runtime install: {}", tame(&why));
            }
            1
        }
    }
}

#[cfg(test)]
mod answer_tests {
    use super::split_answer;

    fn pair(a: &str, b: &str) -> Option<(String, String)> {
        Some((a.to_string(), b.to_string()))
    }

    #[test]
    fn a_pair_splits_at_the_last_bare_equals_sign() {
        assert_eq!(split_answer("Which one?=B"), pair("Which one?", "B"));
        // No backslash: the last `=` separates, as before.
        assert_eq!(split_answer("a=b=c"), pair("a=b", "c"));
        // An escaped `=` is literal, in the label or in the question (PR #41, class C4).
        assert_eq!(split_answer(r"Which?=x\=1"), pair("Which?", "x=1"));
        assert_eq!(split_answer(r"a\=b=c"), pair("a=b", "c"));
        // `\\` is a backslash, so the `=` after it separates. Another backslash stays as it is.
        assert_eq!(split_answer(r"path\\=C"), pair(r"path\", "C"));
        assert_eq!(split_answer(r"a\nb=c"), pair(r"a\nb", "c"));
        // No separator, or an empty label, as before.
        assert_eq!(split_answer("nothing"), None);
        assert_eq!(split_answer(r"only\=escaped"), None);
        assert_eq!(split_answer("q="), pair("q", ""));
    }
}

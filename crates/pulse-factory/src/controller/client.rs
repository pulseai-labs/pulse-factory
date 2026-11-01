//! The client side of `status` and `halt`: one call to the controller, then the print.

use serde_json::{Value, json};

use crate::cli::{HaltArgs, StatusArgs};

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
        line(format!(
            "unit {}: stage {}, {}{}",
            s(&u["id"]),
            s(&u["stage"]),
            s(&u["status"]),
            reason
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
            } else if v["already_halted"].as_bool() == Some(true) {
                println!(
                    "run {} was halted already; the first reason stays: {}",
                    v["run"].as_str().unwrap_or(""),
                    v["reason"].as_str().unwrap_or("")
                );
            } else {
                println!("run {} halted", v["run"].as_str().unwrap_or(""));
            }
            0
        }
        Err(why) => {
            eprintln!("pulse-factory: halt: {why}");
            1
        }
    }
}

//! The events the extension writes. One JSON object per line, in a per-session file.
//!
//! A line is data. The adapter parses it as JSON and reads fixed fields. It never runs text from
//! a line, and a line it cannot read is skipped and logged through `redact`.

use pf_engine::port::agent::Usage;
use serde_json::Value;

/// One event.
#[derive(Clone, Debug, PartialEq)]
pub enum Ev {
    SessionStart,
    AgentStart,
    /// `agent_end`. `will_continue` is true when OMP says the turn goes on.
    AgentEnd {
        will_continue: bool,
    },
    /// The usage of one assistant message.
    MessageEnd {
        usage: Usage,
    },
    /// A tool call started or ended. It is activity.
    ToolActivity,
    /// A bash call and its exit code. The output is not sent: the adapter measures a push in git.
    Bash {
        command: String,
        exit_code: Option<i64>,
    },
    /// The tools the seat holds, as the extension read them after it limited the list.
    Inventory {
        tools: Vec<String>,
        skills: Vec<String>,
    },
    /// The extension denied a tool call.
    Deny,
    /// The extension delivered a message.
    Delivered,
    /// A kind this adapter does not use.
    Other,
}

/// A parsed line, with the time the extension gave, in milliseconds.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub time_ms: Option<u64>,
    pub ev: Ev,
}

fn text(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn count(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// Dollars to millionths of a dollar.
fn micros(dollars: f64) -> u64 {
    if dollars.is_finite() && dollars > 0.0 {
        (dollars * 1_000_000.0).round() as u64
    } else {
        0
    }
}

pub fn parse_line(line: &str) -> Result<Line, String> {
    let v: Value = serde_json::from_str(line).map_err(|e| format!("not JSON: {e}"))?;
    if !v.is_object() {
        return Err("not a JSON object".to_string());
    }
    let kind = v.get("ev").and_then(Value::as_str).ok_or("no `ev` field")?;
    let time_ms = v.get("t").and_then(Value::as_u64);
    let ev = match kind {
        "session_start" => Ev::SessionStart,
        "agent_start" => Ev::AgentStart,
        "agent_end" => Ev::AgentEnd {
            will_continue: v.get("willContinue").and_then(Value::as_bool) == Some(true),
        },
        "message_end" => {
            if v.get("role").and_then(Value::as_str) != Some("assistant") {
                Ev::Other
            } else {
                let u = v.get("usage").cloned().unwrap_or(Value::Null);
                let cost = u
                    .get("cost")
                    .and_then(|c| c.get("total"))
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);
                Ev::MessageEnd {
                    usage: Usage {
                        input_tokens: count(&u, "input"),
                        output_tokens: count(&u, "output"),
                        cache_read_tokens: count(&u, "cacheRead"),
                        cache_write_tokens: count(&u, "cacheWrite"),
                        cost_micros: micros(cost),
                    },
                }
            }
        }
        "tool_call" | "tool_result" => Ev::ToolActivity,
        "bash" => Ev::Bash {
            command: text(&v, "command"),
            exit_code: v.get("exitCode").and_then(Value::as_i64),
        },
        "inventory" => {
            let list = |key: &str| -> Vec<String> {
                v.get(key)
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|t| t.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            Ev::Inventory {
                tools: list("activeTools"),
                skills: list("skills"),
            }
        }
        "deny" => Ev::Deny,
        "delivered" => Ev::Delivered,
        _ => Ev::Other,
    };
    Ok(Line { time_ms, ev })
}

/// Add one reading to a running total.
pub fn add(total: &mut Usage, more: &Usage) {
    total.input_tokens += more.input_tokens;
    total.output_tokens += more.output_tokens;
    total.cache_read_tokens += more.cache_read_tokens;
    total.cache_write_tokens += more.cache_write_tokens;
    total.cost_micros += more.cost_micros;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_usage_in_micros() {
        let l = parse_line(
            r#"{"ev":"message_end","t":5,"role":"assistant","usage":{"input":10,"output":2,"cacheRead":3,"cacheWrite":1,"cost":{"total":0.0013321499999999998}}}"#,
        )
        .unwrap();
        let Ev::MessageEnd { usage } = l.ev else {
            panic!("not a message end")
        };
        assert_eq!((usage.input_tokens, usage.cost_micros), (10, 1332));
    }

    #[test]
    fn rejects_text_that_is_not_an_event() {
        assert!(parse_line("rm -rf /").is_err());
        assert!(parse_line("[1]").is_err());
        assert!(parse_line(r#"{"x":1}"#).is_err());
    }
}

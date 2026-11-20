//! The launch record file the host reads: JSON, as the store holds it. The host takes from it only
//! what it needs to start the child.

use std::collections::BTreeMap;

use serde_json::Value;

/// How the child's input and output are connected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    /// A pseudo-terminal of this size. Output has the terminal's own line endings.
    Pty { rows: u16, cols: u16 },
    /// A pipe each for input and output. The child's error output goes to the host's own.
    Pipes,
}

/// What the host starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub argv: Vec<String>,
    pub cwd: String,
    /// The whole environment of the child, apart from its secret variables.
    pub env: BTreeMap<String, String>,
    pub transport: Transport,
}

impl Record {
    /// Parse a record. The fields are `argv` (a non-empty list of strings), `cwd` (a string, `/`
    /// when absent), `env` (an object of strings, empty when absent) and `transport` (the object
    /// `{"kind":"pipes"}` or `{"kind":"pty","rows":R,"cols":C}`, or the string `"pipes"` or `"pty"`).
    pub fn parse(bytes: &[u8]) -> Result<Record, String> {
        let v: Value =
            serde_json::from_slice(bytes).map_err(|e| format!("the record is not JSON: {e}"))?;
        let obj = v.as_object().ok_or("the record is not a JSON object")?;
        let argv: Vec<String> = obj
            .get("argv")
            .and_then(Value::as_array)
            .ok_or("the record has no `argv` list")?
            .iter()
            .map(|a| a.as_str().map(str::to_string))
            .collect::<Option<_>>()
            .ok_or("`argv` holds a value that is not a string")?;
        if argv.is_empty() || argv[0].is_empty() {
            return Err("`argv` is empty".to_string());
        }
        let cwd = match obj.get("cwd") {
            None | Some(Value::Null) => "/".to_string(),
            Some(Value::String(s)) => s.clone(),
            Some(_) => return Err("`cwd` is not a string".to_string()),
        };
        let mut env = BTreeMap::new();
        match obj.get("env") {
            None | Some(Value::Null) => {}
            Some(Value::Object(map)) => {
                for (k, val) in map {
                    let s = val
                        .as_str()
                        .ok_or_else(|| format!("`env` value of {k} is not a string"))?;
                    env.insert(k.clone(), s.to_string());
                }
            }
            Some(_) => return Err("`env` is not an object".to_string()),
        }
        let transport = match obj.get("transport") {
            Some(Value::String(s)) if s == "pipes" => Transport::Pipes,
            Some(Value::String(s)) if s == "pty" => Transport::Pty { rows: 24, cols: 80 },
            Some(Value::Object(t)) => match t.get("kind").and_then(Value::as_str) {
                Some("pipes") => Transport::Pipes,
                Some("pty") => {
                    let dim = |name: &str, default: u16| -> Result<u16, String> {
                        match t.get(name) {
                            None => Ok(default),
                            Some(n) => n
                                .as_u64()
                                .and_then(|n| u16::try_from(n).ok())
                                .filter(|n| *n > 0)
                                .ok_or_else(|| format!("`transport.{name}` is not 1 to 65535")),
                        }
                    };
                    Transport::Pty {
                        rows: dim("rows", 24)?,
                        cols: dim("cols", 80)?,
                    }
                }
                _ => return Err("`transport.kind` is not `pty` or `pipes`".to_string()),
            },
            _ => return Err("the record has no `transport`".to_string()),
        };
        Ok(Record {
            argv,
            cwd,
            env,
            transport,
        })
    }
}

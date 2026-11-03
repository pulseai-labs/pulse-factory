//! The facts a seat's file adds to a `gaps` or `stop` request (`docs/contracts/requests.md`).
//!
//! A `gaps` request holds the gap list the seat wrote. A `stop` request holds the stop file's
//! text. Both are seat-written, so both are data: they are read with a bound, redacted, cut, and
//! stored as JSON strings. Nothing here renders them or runs them (ADR-0005).

use std::io::Read;
use std::path::Path;

use serde_json::{Value, json};

use crate::status::redact;

/// The most a gap file may hold, in bytes. A bigger file gives `gaps_error`.
pub const GAPS_FILE_MAX_BYTES: u64 = 1 << 20;
/// The longest `section`, `question` or `severity` of one gap, in characters.
pub const GAP_TEXT_MAX_CHARS: usize = 2_000;
/// The most of a stop file the request keeps, in bytes.
pub const STOP_TEXT_MAX_BYTES: u64 = 64 * 1024;

/// The key of the gap list in a request body.
pub const KEY_GAPS: &str = "gaps";
/// The key that says why a gap file gave no list.
pub const KEY_GAPS_ERROR: &str = "gaps_error";
/// The key of the stop file's text in a request body.
pub const KEY_STOP_TEXT: &str = "stop_text";

/// Read a seat's gap file and return the facts to add to the request body: `{"gaps": [...]}`,
/// or `{"gaps_error": "<cause>"}` when the file is missing, too large or not in the shape.
///
/// The shape is the implementer's gaps record, `external_execution_gaps.implementer_return.gaps`,
/// or a top-level `gaps` array. Each gap is an object with the strings `section`, `question` and
/// `severity`. The strings are redacted and cut to [`GAP_TEXT_MAX_CHARS`].
pub fn gaps_detail(path: &Path) -> Value {
    match read_gaps(path) {
        Ok(gaps) => json!({ KEY_GAPS: gaps }),
        Err(cause) => json!({ KEY_GAPS_ERROR: cause }),
    }
}

fn read_gaps(path: &Path) -> Result<Vec<Value>, String> {
    let file = std::fs::File::open(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => "the file does not exist".to_string(),
        _ => format!("the file cannot be read: {e}"),
    })?;
    // One byte more than the bound tells a file that is too large from one that just fits.
    let mut bytes = Vec::new();
    file.take(GAPS_FILE_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("the file cannot be read: {e}"))?;
    if bytes.len() as u64 > GAPS_FILE_MAX_BYTES {
        return Err("the file is over 1 MiB".to_string());
    }
    let parsed: Value = serde_json::from_slice(&bytes).map_err(|e| {
        format!(
            "the file is not valid JSON (line {}, column {})",
            e.line(),
            e.column()
        )
    })?;
    gaps_of(&parsed)
}

/// The gap list of a parsed file, in either shape.
pub fn gaps_of(parsed: &Value) -> Result<Vec<Value>, String> {
    let list = parsed
        .pointer("/external_execution_gaps/implementer_return/gaps")
        .or_else(|| parsed.get(KEY_GAPS))
        .ok_or_else(|| {
            "the file has no `gaps` array, neither at `external_execution_gaps.implementer_return.gaps` nor at the top"
                .to_string()
        })?;
    let items = list
        .as_array()
        .ok_or_else(|| "`gaps` is not an array".to_string())?;
    items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let field = |key: &str| -> Result<String, String> {
                let text = item
                    .get(key)
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("gap {} has no string `{key}`", i + 1))?;
                Ok(redact(text).chars().take(GAP_TEXT_MAX_CHARS).collect())
            };
            Ok(json!({
                "section": field("section")?,
                "question": field("question")?,
                "severity": field("severity")?,
            }))
        })
        .collect()
}

/// Read a stop file and return the facts to add to the request body: `{"stop_text": "<text>"}`,
/// at most [`STOP_TEXT_MAX_BYTES`] bytes of it, redacted. `None` when the file cannot be read: the
/// request keeps its reason, which says so.
pub fn stop_detail(path: &Path) -> Option<Value> {
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(STOP_TEXT_MAX_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    // Redaction can lengthen a text, so the cut comes last, on a character boundary.
    let mut text = redact(&String::from_utf8_lossy(&bytes));
    let mut end = text.len().min(STOP_TEXT_MAX_BYTES as usize);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    Some(json!({ KEY_STOP_TEXT: text }))
}

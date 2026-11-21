//! The facts a seat's file adds to a `gaps` or `stop` request (`docs/contracts/requests.md`).
//!
//! A `gaps` request holds the gap list the seat wrote. A `stop` request holds the stop file's
//! text. Both are seat-written, so both are data: they are read with a bound, redacted, cut, and
//! stored as JSON strings. Nothing here renders them or runs them (ADR-0005).

use std::io::Read;
use std::path::Path;

use serde_json::{Value, json};

use crate::claim::decision::{KEY_PARK_OPTIONS, KEY_STOP_OPTIONS, KEY_STOP_OPTIONS_ERROR};
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
/// The fewest options a stop file's options block holds.
pub const STOP_OPTIONS_MIN: usize = 2;
/// The most options a stop file's options block holds.
pub const STOP_OPTIONS_MAX: usize = 4;
/// The longest option text, in characters, after redaction.
pub const STOP_OPTION_MAX_CHARS: usize = 200;
/// The most options a workflow's `park_on` entry declares.
pub const PARK_OPTIONS_MAX: usize = 4;
/// The longest label of a declared option, in characters.
pub const PARK_OPTION_LABEL_MAX_CHARS: usize = 80;
/// The longest answer text of a declared option, in characters.
pub const PARK_OPTION_ANSWER_MAX_CHARS: usize = 2_000;
/// The line that opens a stop file's options block.
const OPTIONS_LINE: &str = "Options:";
/// The start of every option line.
const OPTION_PREFIX: &str = "- ";
/// The mark of the seat's recommended option, after the line's prefix.
const RECOMMENDED_MARK: &str = "[recommended] ";

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
///
/// A stop file may end with an options block (`docs/contracts/requests.md`, "A stop's options").
/// A block that holds the rules gives `stop_options`, `[{"text", "recommended"}]` in file order. A
/// block that breaks a rule gives `stop_options_error`, a cause in engine words, and no option.
/// A file with no block gives neither key. The stop itself stands in each case: `stop_text` holds
/// the whole file, the block included.
pub fn stop_detail(path: &Path) -> Option<Value> {
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    // One byte more than the bound tells a file that is cut from one that just fits.
    file.take(STOP_TEXT_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    let cut = bytes.len() as u64 > STOP_TEXT_MAX_BYTES;
    let raw = String::from_utf8_lossy(&bytes[..bytes.len().min(STOP_TEXT_MAX_BYTES as usize)])
        .into_owned();
    let options = stop_options_of(&raw, cut);
    // Redaction can lengthen a text, so the cut comes last, on a character boundary.
    let mut text = redact(&raw);
    let mut end = text.len().min(STOP_TEXT_MAX_BYTES as usize);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    let mut facts = json!({ KEY_STOP_TEXT: text });
    if let Some(object) = facts.as_object_mut() {
        match options {
            Some(Ok(list)) => {
                object.insert(KEY_STOP_OPTIONS.to_string(), Value::Array(list));
            }
            Some(Err(why)) => {
                object.insert(KEY_STOP_OPTIONS_ERROR.to_string(), json!(why));
            }
            None => {}
        }
    }
    Some(facts)
}

/// The options a stop file's text names. `None` when the text holds no options block, `Some(Err)`
/// when it holds one that breaks a rule. `cut` says the text is the first bytes of a longer file:
/// a block there may be cut, so it is refused and never read as a shorter block.
fn stop_options_of(text: &str, cut: bool) -> Option<Result<Vec<Value>, String>> {
    let lines: Vec<&str> = text.lines().collect();
    // The block ends the file, so the last line that opens one is the one that counts.
    let start = lines.iter().rposition(|l| l.starts_with(OPTIONS_LINE))?;
    if cut {
        return Some(Err(format!(
            "the stop file is over {STOP_TEXT_MAX_BYTES} bytes, so its options block may be cut"
        )));
    }
    Some(read_options(&lines[start..]))
}

/// The block from its `Options:` line to the end of the file.
fn read_options(lines: &[&str]) -> Result<Vec<Value>, String> {
    if lines[0].trim_end() != OPTIONS_LINE {
        return Err(format!("the {OPTIONS_LINE} line is not alone on its line"));
    }
    let mut options: Vec<Value> = Vec::new();
    let mut marks = 0;
    let mut ended = false;
    for line in &lines[1..] {
        if line.trim().is_empty() {
            ended = true;
            continue;
        }
        if ended {
            return Err("the options block has text after it".to_string());
        }
        let Some(rest) = line.strip_prefix(OPTION_PREFIX) else {
            return Err(format!(
                "a line of the options block does not start with {OPTION_PREFIX:?}"
            ));
        };
        let (marked, rest) = match rest.strip_prefix(RECOMMENDED_MARK) {
            Some(rest) => (true, rest),
            None => (false, rest),
        };
        marks += usize::from(marked);
        // Redact first, then count: a secret that is cut to a mark must not decide the length.
        let text = redact(rest.trim());
        let chars = text.chars().count();
        if chars == 0 {
            return Err("an option is empty".to_string());
        }
        if chars > STOP_OPTION_MAX_CHARS {
            return Err(format!(
                "an option holds {chars} characters; the most is {STOP_OPTION_MAX_CHARS}"
            ));
        }
        if text.chars().any(char::is_control) {
            return Err("an option holds a control character".to_string());
        }
        options.push(json!({ "text": text, "recommended": marked }));
    }
    if !(STOP_OPTIONS_MIN..=STOP_OPTIONS_MAX).contains(&options.len()) {
        return Err(format!(
            "the options block holds {} options, not {STOP_OPTIONS_MIN} to {STOP_OPTIONS_MAX}",
            options.len()
        ));
    }
    if marks > 1 {
        return Err("more than one option is marked [recommended]".to_string());
    }
    Ok(options)
}

/// One option that a workflow's `park_on` entry declares: a label, and an answer text, a stage, or
/// both. The workflow file's reader makes it. The engine knows nothing of the file's form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParkOptionDecl {
    pub label: String,
    /// The text the option answers with.
    pub answer: Option<String>,
    /// The stage the unit resumes at.
    pub to: Option<String>,
    /// The workflow marks this option as the one it recommends.
    pub recommended: bool,
}

/// The facts of a park that its workflow's options add: `{"park_options": [{"label", "answer",
/// "to", "recommended"}, ...]}` in file order. An absent `answer` or `to` is left out. The decision
/// block reads this fact as data (`docs/contracts/requests.md`, "A workflow's options").
pub fn park_options_detail(options: &[ParkOptionDecl]) -> Value {
    let list: Vec<Value> = options
        .iter()
        .map(|o| {
            let mut entry = serde_json::Map::new();
            entry.insert("label".to_string(), json!(o.label));
            if let Some(answer) = &o.answer {
                entry.insert("answer".to_string(), json!(answer));
            }
            if let Some(to) = &o.to {
                entry.insert("to".to_string(), json!(to));
            }
            entry.insert("recommended".to_string(), json!(o.recommended));
            Value::Object(entry)
        })
        .collect();
    json!({ KEY_PARK_OPTIONS: list })
}

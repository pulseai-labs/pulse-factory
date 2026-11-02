//! The six node statuses of ADR-0006.
//!
//! Every node has one status. Every end status carries a one-line reason.

use serde::{Deserialize, Serialize};

/// The status of one node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Running,
    Done,
    Failed,
    Stopped,
    WaitingOnYou,
    Parked,
}

impl Status {
    /// The stored name of the status.
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Running => "running",
            Status::Done => "done",
            Status::Failed => "failed",
            Status::Stopped => "stopped",
            Status::WaitingOnYou => "waiting_on_you",
            Status::Parked => "parked",
        }
    }

    /// Read a stored name back.
    pub fn parse(text: &str) -> Option<Status> {
        Some(match text {
            "running" => Status::Running,
            "done" => Status::Done,
            "failed" => Status::Failed,
            "stopped" => Status::Stopped,
            "waiting_on_you" => Status::WaitingOnYou,
            "parked" => Status::Parked,
            _ => return None,
        })
    }

    /// An end status needs a reason.
    pub fn is_end(self) -> bool {
        matches!(
            self,
            Status::Done | Status::Failed | Status::Stopped | Status::Parked
        )
    }
}

/// The end statuses a caller may record with the `End` command.
/// `Parked` has its own command, [`crate::admission::CommandBody::Park`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndStatus {
    Done,
    Failed,
    Stopped,
}

impl EndStatus {
    pub fn status(self) -> Status {
        match self {
            EndStatus::Done => Status::Done,
            EndStatus::Failed => Status::Failed,
            EndStatus::Stopped => Status::Stopped,
        }
    }
}

/// Why a reason text is not acceptable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReasonError {
    Empty,
    MultiLine,
}

impl std::fmt::Display for ReasonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReasonError::Empty => write!(f, "a reason must not be empty"),
            ReasonError::MultiLine => write!(f, "a reason must be one line"),
        }
    }
}

impl std::error::Error for ReasonError {}

/// Check that a reason is one non-empty line. Returns the trimmed text.
pub fn check_reason(text: &str) -> Result<&str, ReasonError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(ReasonError::Empty);
    }
    if trimmed.contains(['\n', '\r']) {
        return Err(ReasonError::MultiLine);
    }
    Ok(trimmed)
}

/// What replaces a secret in redacted text.
pub const REDACTED: &str = "[redacted]";

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// The longest reason text, in characters.
pub const REASON_MAX_CHARS: usize = 500;

/// Does the pattern, an ASCII text, start at `at`? The match ignores ASCII case.
fn starts_with_ci(chars: &[char], at: usize, pattern: &str) -> bool {
    let mut i = at;
    for p in pattern.chars() {
        match chars.get(i) {
            Some(c) if c.eq_ignore_ascii_case(&p) => i += 1,
            _ => return false,
        }
    }
    true
}

/// A word starts here: the character before is not a letter, a digit or `_`.
fn at_word_start(chars: &[char], i: usize) -> bool {
    i == 0 || !is_token_char(chars[i - 1])
}

fn is_blank(c: char) -> bool {
    c == ' ' || c == '\t'
}

/// The end of the run of characters that satisfy `keep`, from `from`.
fn run_end(chars: &[char], from: usize, keep: impl Fn(char) -> bool) -> usize {
    let mut end = from;
    while end < chars.len() && keep(chars[end]) {
        end += 1;
    }
    end
}

/// The end of a value that starts at `from`: a quoted text up to its closing quote on the same
/// line, or else the run of characters up to the next white space. `from == end` means no value.
fn value_end(chars: &[char], from: usize) -> usize {
    if let Some(&quote) = chars.get(from).filter(|c| matches!(c, '"' | '\'')) {
        let closing = chars[from + 1..]
            .iter()
            .take_while(|c| !matches!(c, '\n' | '\r'))
            .position(|c| *c == quote);
        if let Some(offset) = closing {
            return from + 1 + offset + 1;
        }
    }
    run_end(chars, from, |c| !c.is_whitespace())
}

/// The end of the line that holds `from`. A ` / ` that [`one_line_reason`] wrote for a line
/// break ends the line too, so a second pass over a flat text cuts only the value.
fn line_end(chars: &[char], from: usize) -> usize {
    let mut end = from;
    while end < chars.len() && !matches!(chars[end], '\n' | '\r') {
        if chars[end] == ' ' && starts_with_ci(chars, end, " / ") {
            break;
        }
        end += 1;
    }
    end
}

/// `gh[pousr]_` and `github_pat_` tokens: the prefix and 16 or more token characters.
fn github_token(chars: &[char], i: usize) -> Option<usize> {
    if !at_word_start(chars, i) {
        return None;
    }
    let prefix = if starts_with_ci(chars, i, "github_pat_") {
        "github_pat_".len()
    } else if chars[i] == 'g'
        && chars.get(i + 1) == Some(&'h')
        && chars
            .get(i + 2)
            .is_some_and(|c| matches!(c, 'p' | 'o' | 'u' | 's' | 'r'))
        && chars.get(i + 3) == Some(&'_')
    {
        4
    } else {
        return None;
    };
    let end = run_end(chars, i + prefix, is_token_char);
    (end - (i + prefix) >= 16).then_some(end)
}

/// A whole-word `sk-` followed by 16 or more key characters.
fn sk_key(chars: &[char], i: usize) -> Option<usize> {
    if !at_word_start(chars, i) || !starts_with_ci(chars, i, "sk-") {
        return None;
    }
    let end = run_end(chars, i + 3, |c| {
        c.is_ascii_alphanumeric() || c == '-' || c == '_'
    });
    (end - (i + 3) >= 16).then_some(end)
}

/// `Bearer <credential>`: the word stays and the credential goes.
fn bearer(chars: &[char], i: usize) -> Option<(usize, usize)> {
    if !at_word_start(chars, i) || !starts_with_ci(chars, i, "bearer") {
        return None;
    }
    let blanks = run_end(chars, i + 6, is_blank);
    if blanks == i + 6 {
        return None;
    }
    let end = run_end(chars, blanks, |c| !c.is_whitespace());
    (end > blanks).then_some((blanks, end))
}

/// `Authorization: <value>`: the name stays and the value goes, to the end of the line.
fn authorization(chars: &[char], i: usize) -> Option<(usize, usize)> {
    if !at_word_start(chars, i) || !starts_with_ci(chars, i, "authorization") {
        return None;
    }
    let colon = run_end(chars, i + "authorization".len(), is_blank);
    if chars.get(colon) != Some(&':') {
        return None;
    }
    let start = run_end(chars, colon + 1, is_blank);
    let end = line_end(chars, start);
    // Keep trailing blanks out of the cut.
    let mut cut = end;
    while cut > start && is_blank(chars[cut - 1]) {
        cut -= 1;
    }
    (cut > start).then_some((start, cut))
}

/// `NAME_TOKEN=value`, `NAME_KEY=value` and `NAME_SECRET=value`: the name stays.
fn assignment(chars: &[char], i: usize) -> Option<(usize, usize)> {
    if !at_word_start(chars, i) {
        return None;
    }
    let name_end = run_end(chars, i, is_token_char);
    if name_end == i || chars.get(name_end) != Some(&'=') {
        return None;
    }
    let name: String = chars[i..name_end]
        .iter()
        .collect::<String>()
        .to_ascii_lowercase();
    if !["_token", "_key", "_secret"]
        .iter()
        .any(|s| name.ends_with(s))
    {
        return None;
    }
    let start = name_end + 1;
    let end = value_end(chars, start);
    (end > start).then_some((start, end))
}

/// `--api-key=value` and `--api-key value`: the flag stays and the value goes.
fn api_key_flag(chars: &[char], i: usize) -> Option<(usize, usize)> {
    let before_ok = i == 0 || !(is_token_char(chars[i - 1]) || chars[i - 1] == '-');
    if !before_ok || !starts_with_ci(chars, i, "--api-key") {
        return None;
    }
    let after = i + "--api-key".len();
    let start = match chars.get(after) {
        Some('=') => after + 1,
        Some(c) if is_blank(*c) => run_end(chars, after, is_blank),
        _ => return None,
    };
    let end = value_end(chars, start);
    (end > start).then_some((start, end))
}

/// `://user:secret@host`: the user name stays and the secret goes.
fn url_secret(chars: &[char], i: usize) -> Option<(usize, usize)> {
    if !(chars[i] == ':' && chars.get(i + 1) == Some(&'/') && chars.get(i + 2) == Some(&'/')) {
        return None;
    }
    let auth_start = i + 3;
    let auth_end = run_end(chars, auth_start, |c| {
        !c.is_whitespace() && !matches!(c, '/' | '?' | '#')
    });
    let at = chars[auth_start..auth_end]
        .iter()
        .rposition(|c| *c == '@')?
        + auth_start;
    let colon = chars[auth_start..at].iter().position(|c| *c == ':')? + auth_start;
    (colon + 1 < at).then_some((colon + 1, at))
}

/// Remove secrets from outside text before it reaches the ledger or a status line.
///
/// It removes these forms, and it keeps the word or name in front of a value, so a reader sees
/// what kind of secret it was:
///
/// - `gh[pousr]_…` and `github_pat_…` tokens;
/// - a whole-word `sk-` followed by 16 or more key characters;
/// - `Bearer <credential>`;
/// - `Authorization:` values, to the end of the line;
/// - the values of `*_TOKEN=`, `*_KEY=` and `*_SECRET=`;
/// - `--api-key=<value>` and `--api-key <value>`;
/// - the secret in `https://user:secret@host`.
///
/// A second pass over redacted text changes nothing.
pub fn redact(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        // Each rule gives the range of the secret. What lies before it, from `i`, is kept.
        let hit = github_token(&chars, i)
            .or_else(|| sk_key(&chars, i))
            .map(|end| (i, end))
            .or_else(|| bearer(&chars, i))
            .or_else(|| authorization(&chars, i))
            .or_else(|| assignment(&chars, i))
            .or_else(|| api_key_flag(&chars, i))
            .or_else(|| url_secret(&chars, i));
        match hit {
            Some((start, end)) => {
                out.extend(&chars[i..start]);
                out.push_str(REDACTED);
                i = end;
            }
            None => {
                out.push(chars[i]);
                i += 1;
            }
        }
    }
    out
}

/// The reason that stands in for an empty text.
pub const REASON_NONE_GIVEN: &str = "no reason text given";

/// Turn any outside text into a reason that [`check_reason`] accepts: one non-empty line.
///
/// The text is redacted first. Each line break and each carriage return becomes ` / `, and a
/// `\r\n` pair is one break. Other control characters are removed. The result is cut to
/// [`REASON_MAX_CHARS`] characters, on a character boundary. A text that is empty after this
/// gives [`REASON_NONE_GIVEN`].
pub fn one_line_reason(text: &str) -> String {
    let redacted = redact(text);
    let mut flat = String::with_capacity(redacted.len());
    let mut chars = redacted.trim().chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                flat.push_str(" / ");
            }
            '\n' => flat.push_str(" / "),
            c if c.is_control() => {}
            c => flat.push(c),
        }
    }
    let flat = flat.trim();
    if flat.is_empty() {
        REASON_NONE_GIVEN.to_string()
    } else {
        flat.chars().take(REASON_MAX_CHARS).collect()
    }
}

/// Reason a unit gets when the run is halted and its next step is a launch or a merge.
pub const REASON_RUN_HALTED: &str = "run halted";

/// Reason a unit gets when its seat was lost across a controller restart.
pub const REASON_SEAT_LOST: &str = "seat lost across a controller restart";

//! The session screen as data. The adapter uses it for reason text only, and never to detect a
//! state (ADR-0011). There is no terminal emulator here: it removes escape sequences from the
//! bytes the process wrote, and a repaint of a full-screen program keeps its old text.

use pf_engine::status::REDACTED;

/// Remove terminal escape sequences from output bytes. A colour sequence goes. Every other
/// sequence becomes one space, so words that a cursor move separated stay separate.
pub fn strip_ansi(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c != '\u{1b}' {
            out.push(c);
            i += 1;
            continue;
        }
        i += 1;
        match chars.get(i) {
            Some('[') => {
                i += 1;
                while i < chars.len() && !('\u{40}'..='\u{7e}').contains(&chars[i]) {
                    i += 1;
                }
                if chars.get(i) != Some(&'m') {
                    out.push(' ');
                }
                i += 1;
            }
            Some(']' | 'P' | '_' | '^' | 'X') => {
                // OSC, DCS, APC, PM and SOS strings end with BEL or ST (ESC \).
                i += 1;
                while i < chars.len() {
                    if chars[i] == '\u{7}' {
                        i += 1;
                        break;
                    }
                    if chars[i] == '\u{1b}' && chars.get(i + 1) == Some(&'\\') {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            }
            Some('(' | ')' | '*' | '+') => i += 2,
            Some(_) => i += 1,
            None => {}
        }
    }
    out
}

/// Collapse each run of white space and control characters to one space. A reason text holds one
/// line, so the engine would turn each line break into three characters, and a tail cut by
/// characters must not lose its last lines to that.
pub fn flatten(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for c in text.chars() {
        if c.is_whitespace() || c.is_control() {
            pending_space = !out.is_empty();
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(c);
        }
    }
    out
}

/// Replace each run of exactly 32 hex digits with the redaction mark: the shape of
/// `token::fresh_token`, the seat token. After a controller restart the adapter no longer knows the
/// token's value (the launch record holds only its hash), so a screen tail masks the shape. A run of
/// 31 digits, or of 40 (a git SHA), is kept.
pub fn mask_tokens(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if run.len() == 32 {
            out.push_str(REDACTED);
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for c in text.chars() {
        if c.is_ascii_hexdigit() {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

/// The last `max_chars` characters of the flattened text, with any seat-token shape masked.
pub fn tail(text: &str, max_chars: usize) -> String {
    let flat = mask_tokens(&flatten(text));
    let count = flat.chars().count();
    if count <= max_chars {
        return flat;
    }
    flat.chars().skip(count - max_chars).collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX32: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn masks_a_run_of_exactly_32_hex_digits_in_a_tail() {
        let text = format!("token {HEX32} end");
        assert_eq!(tail(&text, 200), format!("token {REDACTED} end"));
        // Upper case is hex too, and two runs are two masks.
        let two = format!("{} and {}", HEX32.to_uppercase(), HEX32);
        assert_eq!(tail(&two, 200), format!("{REDACTED} and {REDACTED}"));
        // The mask comes before the cut: a cut never leaves part of a token.
        let cut = tail(&format!("aaaa {HEX32}"), REDACTED.chars().count() + 2);
        assert!(!cut.contains("0123"), "{cut}");
    }

    #[test]
    fn keeps_runs_of_31_and_40_hex_digits() {
        let (short, sha) = (&HEX32[..31], format!("{HEX32}01234567"));
        assert_eq!(sha.len(), 40);
        assert_eq!(tail(&format!("a {short} b"), 200), format!("a {short} b"));
        assert_eq!(tail(&format!("a {sha} b"), 200), format!("a {sha} b"));
        // Only a maximal run of exactly 32 counts: a run of 33 is kept whole.
        let long = format!("{HEX32}a");
        assert_eq!(tail(&format!("a {long} b"), 200), format!("a {long} b"));
    }

    #[test]
    fn strips_colour_and_cursor_moves() {
        let raw = b"\x1b[31mTrust\x1b[0m\x1b[2;5Hthis folder?\x1b]0;title\x07";
        assert_eq!(flatten(&strip_ansi(raw)), "Trust this folder?");
    }

    #[test]
    fn strips_terminal_query_strings() {
        let raw = b"\x1bP>|omp(1.0)\x1b\\\x1b_Gi=31;OK\x1b\\Ready?";
        assert_eq!(flatten(&strip_ansi(raw)), "Ready?");
    }

    #[test]
    fn tail_keeps_the_end() {
        assert_eq!(tail("abc\n\n def", 5), "c def");
    }
}

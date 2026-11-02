//! The session screen as data. The adapter uses it for reason text only, and never to detect a
//! state (ADR-0011). There is no terminal emulator here: it removes escape sequences from the
//! bytes the process wrote, and a repaint of a full-screen program keeps its old text.

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

/// The last `max_chars` characters of the flattened text.
pub fn tail(text: &str, max_chars: usize) -> String {
    let flat = flatten(text);
    let count = flat.chars().count();
    if count <= max_chars {
        return flat;
    }
    flat.chars().skip(count - max_chars).collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;

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
